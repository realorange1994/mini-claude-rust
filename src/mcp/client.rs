//! MCP Client - stdio JSON-RPC 2.0 and HTTP+SSE communication with MCP servers

use super::{Tool, ToolResult, ToolResultContent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

// ─── JSON-RPC Types ───

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: i64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Option<i64>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
}

// ─── Server type ───

#[derive(Debug, Clone, PartialEq)]
enum ServerType {
    Stdio,
    Remote,
}

// ─── Stdio state ───

struct StdioState {
    /// The spawned child process (None before start, Some after)
    child: Option<Child>,
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<BufReader<std::process::ChildStdout>>,
}

// ─── Client ───

pub struct Client {
    name: String,
    server_type: ServerType,

    /// Stdio process handles
    stdio: Mutex<StdioState>,
    /// Command builder (used before spawn, discarded after)
    cmd: Mutex<Option<Command>>,
    next_id: AtomicI64,

    /// Remote server URL
    url: String,
    headers: HashMap<String, String>,

    /// Discovered tools
    tools: Mutex<Vec<Tool>>,
    running: Mutex<bool>,

    /// HTTP client for remote servers
    http_client: reqwest::blocking::Client,
}

impl Client {
    pub fn new_stdio(name: &str, command: &str, args: &[String], env: &HashMap<String, String>) -> Self {
        let mut cmd = Command::new(command);
        cmd.args(args);
        if !env.is_empty() {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        Self {
            name: name.to_string(),
            server_type: ServerType::Stdio,
            stdio: Mutex::new(StdioState {
                child: None,
                stdin: None,
                stdout: None,
            }),
            cmd: Mutex::new(Some(cmd)),
            next_id: AtomicI64::new(0),
            url: String::new(),
            headers: HashMap::new(),
            tools: Mutex::new(Vec::new()),
            running: Mutex::new(false),
            http_client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn new_remote(name: &str, url: &str, headers: &HashMap<String, String>) -> Self {
        Self {
            name: name.to_string(),
            server_type: ServerType::Remote,
            stdio: Mutex::new(StdioState {
                child: None,
                stdin: None,
                stdout: None,
            }),
            cmd: Mutex::new(None),
            next_id: AtomicI64::new(0),
            url: url.to_string(),
            headers: headers.clone(),
            tools: Mutex::new(Vec::new()),
            running: Mutex::new(false),
            http_client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Start the MCP server (stdio or remote)
    pub fn start(&self) -> Result<(), String> {
        if self.server_type == ServerType::Remote {
            self.start_remote()
        } else {
            self.start_stdio()
        }
    }

    fn start_stdio(&self) -> Result<(), String> {
        let mut cmd = self.cmd.lock().unwrap().take()
            .ok_or("stdio server already started".to_string())?;

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn {}: {}", self.name, e))?;

        let stdin = child.stdin.take().ok_or("failed to take stdin".to_string())?;
        let stdout = child.stdout.take().ok_or("failed to take stdout".to_string())?;
        let stderr = child.stderr.take().ok_or("failed to take stderr".to_string())?;

        // Start stderr relay thread
        let name = self.name.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                if let Ok(line) = line {
                    eprintln!("[{} stderr] {}", name, line);
                }
            }
        });

        {
            let mut stdio = self.stdio.lock().unwrap();
            stdio.stdin = Some(stdin);
            stdio.stdout = Some(BufReader::new(stdout));
            stdio.child = Some(child);
        }

        *self.running.lock().unwrap() = true;

        // Initialize
        self.initialize_stdio()?;

        // Discover tools
        let tools = self.list_tools_stdio()?;
        *self.tools.lock().unwrap() = tools;

        Ok(())
    }

    fn start_remote(&self) -> Result<(), String> {
        *self.running.lock().unwrap() = true;

        // Initialize
        self.initialize_remote()?;

        // Discover tools
        let tools = self.list_tools_remote()?;
        *self.tools.lock().unwrap() = tools;

        Ok(())
    }

    // ─── Initialization ───

    fn initialize_stdio(&self) -> Result<(), String> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "miniclaudecode", "version": "0.1.0"}
        });

        self.request_stdio("initialize", Some(params))?;

        // Send initialized notification
        let notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "notifications/initialized".to_string(),
        };
        self.send_stdio(&notif)?;

        Ok(())
    }

    fn initialize_remote(&self) -> Result<(), String> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "miniclaudecode", "version": "0.1.0"}
        });

        self.request_remote("initialize", Some(params))?;
        Ok(())
    }

    // ─── Tool discovery ───

    fn list_tools_stdio(&self) -> Result<Vec<Tool>, String> {
        let resp = self.request_stdio("tools/list", None)?;

        #[derive(Deserialize)]
        struct ToolsResponse {
            #[serde(default)]
            tools: Vec<Tool>,
        }

        let result: ToolsResponse = serde_json::from_value(resp)
            .map_err(|e| format!("parse tools: {}", e))?;

        Ok(result.tools)
    }

    fn list_tools_remote(&self) -> Result<Vec<Tool>, String> {
        let resp = self.request_remote("tools/list", None)?;

        #[derive(Deserialize)]
        struct ToolsResponse {
            #[serde(default)]
            tools: Vec<Tool>,
        }

        let result: ToolsResponse = serde_json::from_value(resp)
            .map_err(|e| format!("parse tools: {}", e))?;

        Ok(result.tools)
    }

    /// Get discovered tools
    pub fn tools(&self) -> Vec<Tool> {
        self.tools.lock().unwrap().clone()
    }

    // ─── Tool invocation ───

    pub fn call_tool(&self, name: &str, args: HashMap<String, serde_json::Value>) -> Result<ToolResult, String> {
        if self.server_type == ServerType::Remote {
            self.call_tool_remote(name, &args)
        } else {
            self.call_tool_stdio(name, &args)
        }
    }

    fn call_tool_stdio(&self, name: &str, args: &HashMap<String, serde_json::Value>) -> Result<ToolResult, String> {
        let params = serde_json::json!({
            "name": name,
            "arguments": args
        });

        let resp = self.request_stdio("tools/call", Some(params))?;
        parse_tool_result(&resp)
            .ok_or_else(|| "parse tool result failed".to_string())
    }

    fn call_tool_remote(&self, name: &str, args: &HashMap<String, serde_json::Value>) -> Result<ToolResult, String> {
        let params = serde_json::json!({
            "name": name,
            "arguments": args
        });

        let resp = self.request_remote("tools/call", Some(params))?;
        parse_tool_result(&resp)
            .ok_or_else(|| "parse tool result failed".to_string())
    }

    // ─── Stdio transport ───

    fn request_stdio(&self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        // Atomically write request then read response while holding the lock,
        // to prevent interleaving with concurrent requests on the same stdin/stdout.
        let mut stdio = self.stdio.lock().unwrap();

        // Write request
        let data = serde_json::to_string(&req).map_err(|e| format!("marshal: {}", e))?;
        let stdin = stdio.stdin.as_mut().ok_or("stdin not available".to_string())?;
        writeln!(stdin, "{}", data).map_err(|e| format!("write: {}", e))?;
        stdin.flush().map_err(|e| format!("flush: {}", e))?;

        // Read response, skipping notifications and validating ID match
        let reader = stdio.stdout.as_mut().ok_or("stdout not available".to_string())?;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| format!("read: {}", e))?;
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let resp: JsonRpcResponse = serde_json::from_str(&line)
                .map_err(|e| format!("parse: {} (raw: {})", e, &line[..line.len().min(200)]))?;

            // Skip notifications (no id field) — e.g. notifications/progress, notifications/message
            if resp.id.is_none() {
                continue;
            }

            // Validate response ID matches our request ID
            if resp.id != Some(id) {
                // ID mismatch — this shouldn't happen with atomic write+read,
                // but handle defensively by skipping stale responses
                continue;
            }

            if let Some(err) = resp.error {
                return Err(format!("MCP error [{}]: {}", err.code, err.message));
            }

            return resp.result.ok_or_else(|| "no result in response".to_string());
        }
    }

    fn send_stdio(&self, notification: &JsonRpcNotification) -> Result<(), String> {
        let data = serde_json::to_string(notification).map_err(|e| format!("marshal: {}", e))?;
        let mut stdio = self.stdio.lock().unwrap();
        let stdin = stdio.stdin.as_mut().ok_or("stdin not available".to_string())?;
        writeln!(stdin, "{}", data).map_err(|e| format!("write: {}", e))?;
        stdin.flush().map_err(|e| format!("flush: {}", e))?;
        Ok(())
    }

    // ─── Remote HTTP+SSE transport ───

    fn request_remote(&self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let req_body = match params {
            None => serde_json::to_string(&JsonRpcRequest {
                jsonrpc: "2.0",
                id,
                method: method.to_string(),
                params: None,
            }),
            Some(p) => serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": p,
            })),
        }.map_err(|e| format!("marshal: {}", e))?;

        let mut req = self.http_client.post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(req_body);

        for (k, v) in &self.headers {
            req = req.header(k, v);
        }

        let resp = req.send().map_err(|e| format!("request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("HTTP {}: {}", status, &body[..body.len().min(256)]));
        }

        let text = resp.text().map_err(|e| format!("read response: {}", e))?;
        let json_resp = Self::parse_sse_response(&text)
            .ok_or_else(|| "no JSON response in SSE stream".to_string())?;

        if let Some(err) = json_resp.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            let message = err.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
            return Err(format!("MCP error [{}]: {}", code, message));
        }

        json_resp.get("result").cloned()
            .ok_or_else(|| "no result in response".to_string())
    }

    fn parse_sse_response(text: &str) -> Option<serde_json::Value> {
        for line in text.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data: ") {
                if data.is_empty() || data == ":" {
                    continue;
                }
                if data.contains("\"result\"") || data.contains("\"error\"") {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                        return Some(val);
                    }
                }
            }
        }
        serde_json::from_str::<serde_json::Value>(text).ok()
    }

    // ─── Stop ───

    pub fn stop(&self) -> Result<(), String> {
        if !*self.running.lock().unwrap() {
            return Ok(());
        }

        if self.server_type == ServerType::Remote {
            *self.running.lock().unwrap() = false;
            return Ok(());
        }

        *self.running.lock().unwrap() = false;

        let mut stdio = self.stdio.lock().unwrap();

        // Close stdin
        stdio.stdin.take();

        // Wait for process to exit (with 5s timeout)
        if let Some(child) = stdio.child.as_mut() {
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if start.elapsed() > Duration::from_secs(5) {
                            let _ = child.kill();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => return Err(format!("wait: {}", e)),
                }
            }
        }

        Ok(())
    }
}

// ─── Parse ToolResult from JSON ───

pub fn parse_tool_result(value: &serde_json::Value) -> Option<ToolResult> {
    let content = value.get("content").and_then(|c| c.as_array())?;
    let is_error = value.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);

    let mut blocks = Vec::new();
    for block in content {
        if let Some(block_type) = block.get("type").and_then(|v| v.as_str()) {
            if block_type == "text" {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    blocks.push(ToolResultContent::Text { text: text.to_string() });
                }
            }
        }
    }

    Some(ToolResult {
        content: blocks,
        is_error,
    })
}
