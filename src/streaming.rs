//! Streaming response handling for agent loop
//! Full implementation of SSE parsing, stall detection, and chunk collection.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use futures::StreamExt;

use crate::tools::truncate_at;

/// Streaming chunk types
#[derive(Debug, Clone)]
pub enum ChunkType {
    Text,
    ToolCall,
    ToolArgument,
    Thinking,
    Usage,
    #[allow(dead_code)]
    Error,
    Done,
    BlockStop,
}

/// A single event emitted during a streaming response
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub chunk_type: ChunkType,
    pub content: String,
    pub id: Option<String>,
    pub name: Option<String>,
    pub usage: Option<Usage>,
}

/// Token usage information
#[derive(Debug, Clone)]
pub struct Usage {
    #[allow(dead_code)]
    pub input_tokens: i64,
    #[allow(dead_code)]
    pub output_tokens: i64,
}

/// CollectHandler assembles streamed tokens into a complete response
pub struct CollectHandler {
    text: RwLock<String>,
    tool_calls: RwLock<Vec<ToolCallInfo>>,
    thinking: RwLock<String>,
    tool_use_as_text: RwLock<bool>,
    usage: RwLock<Option<Usage>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl CollectHandler {
    pub fn new() -> Self {
        Self {
            text: RwLock::new(String::new()),
            tool_calls: RwLock::new(Vec::new()),
            thinking: RwLock::new(String::new()),
            tool_use_as_text: RwLock::new(false),
            usage: RwLock::new(None),
        }
    }

    /// Handle a single chunk
    pub fn handle(&self, chunk: StreamChunk) {
        match chunk.chunk_type {
            ChunkType::Text => {
                let mut text = self.text.write().unwrap();
                let content_lower = chunk.content.to_lowercase();

                // Detect model echoing tool syntax as text (2-of-3 structural markers)
                let has_type = content_lower.contains(r#""type":"tool_use""#)
                    || content_lower.contains(r#""type": "tool_use""#);
                let has_id = content_lower.contains(r#""id":""#)
                    || content_lower.contains(r#""id": ""#);
                let has_name = content_lower.contains(r#""name":""#)
                    || content_lower.contains(r#""name": ""#);

                if (has_type && has_id) || (has_type && has_name) || (has_id && has_name) {
                    let mut flag = self.tool_use_as_text.write().unwrap();
                    *flag = true;
                } else {
                    text.push_str(&chunk.content);
                }
            }
            ChunkType::ToolCall => {
                let mut calls = self.tool_calls.write().unwrap();
                calls.push(ToolCallInfo {
                    id: chunk.id.unwrap_or_default(),
                    name: chunk.name.unwrap_or_default(),
                    arguments: String::new(),
                });
            }
            ChunkType::ToolArgument => {
                let mut calls = self.tool_calls.write().unwrap();
                if let Some(last) = calls.last_mut() {
                    last.arguments.push_str(&chunk.content);
                }
            }
            ChunkType::Thinking => {
                let mut thinking = self.thinking.write().unwrap();
                thinking.push_str(&chunk.content);
            }
            ChunkType::Usage => {
                if let Some(u) = chunk.usage {
                    let mut usage = self.usage.write().unwrap();
                    *usage = Some(u);
                }
            }
            ChunkType::Error => {
                // Error is handled by caller
            }
            ChunkType::Done | ChunkType::BlockStop => {
                // Stream finished
            }
        }
    }

    /// Get the assembled text (falls back to thinking when text is empty)
    pub fn full_response(&self) -> String {
        let text = self.text.read().unwrap();
        if !text.is_empty() {
            return text.clone();
        }
        let thinking = self.thinking.read().unwrap();
        thinking.clone()
    }

    /// Get the thinking content
    pub fn thinking(&self) -> String {
        let thinking = self.thinking.read().unwrap();
        thinking.clone()
    }

    /// Check if model echoed tool syntax as text
    pub fn is_tool_use_as_text(&self) -> bool {
        *self.tool_use_as_text.read().unwrap()
    }

    /// Get tool calls
    pub fn tool_calls(&self) -> Vec<ToolCallInfo> {
        self.tool_calls.read().unwrap().clone()
    }

    /// Get usage info
    #[allow(dead_code)]
    pub fn usage(&self) -> Option<Usage> {
        self.usage.read().unwrap().clone()
    }
}

impl Default for CollectHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// TerminalHandler prints clean output to terminal
pub struct TerminalHandler {
    seen_tool_call: RwLock<bool>,
    thinking_buf: RwLock<String>,
    cur_tool_name: RwLock<String>,
    cur_tool_args: RwLock<String>,
}

impl TerminalHandler {
    pub fn new() -> Self {
        Self {
            seen_tool_call: RwLock::new(false),
            thinking_buf: RwLock::new(String::new()),
            cur_tool_name: RwLock::new(String::new()),
            cur_tool_args: RwLock::new(String::new()),
        }
    }

    pub fn handle(&self, chunk: StreamChunk) {
        match chunk.chunk_type {
            ChunkType::Thinking => {
                let mut buf = self.thinking_buf.write().unwrap();
                buf.push_str(&chunk.content);
            }
            ChunkType::ToolCall => {
                let mut seen = self.seen_tool_call.write().unwrap();
                *seen = true;
                drop(seen);

                // Show buffered thinking before tool call
                {
                    let buf = self.thinking_buf.read().unwrap();
                    if !buf.is_empty() {
                        let preview = truncate_at(buf.lines().next().unwrap_or(""), 120);
                        eprintln!("\n[THINK] {}", preview);
                    }
                }
                let mut buf = self.thinking_buf.write().unwrap();
                buf.clear();
                drop(buf);

                let mut name = self.cur_tool_name.write().unwrap();
                *name = chunk.name.unwrap_or_default();
                let mut args = self.cur_tool_args.write().unwrap();
                args.clear();
            }
            ChunkType::ToolArgument => {
                let mut args = self.cur_tool_args.write().unwrap();
                args.push_str(&chunk.content);
            }
            ChunkType::BlockStop => {
                // Flush pending tool call
                self.flush_tool_call();
            }
            ChunkType::Done => {
                self.flush_tool_call();
                let seen = self.seen_tool_call.read().unwrap();
                if !*seen {
                    let buf = self.thinking_buf.read().unwrap();
                    if !buf.is_empty() {
                        let preview = truncate_at(buf.lines().next().unwrap_or(""), 120);
                        eprintln!("\n[THINK] {}", preview);
                    }
                }
            }
            ChunkType::Text => {
                // Flush any pending tool call before text
                self.flush_tool_call();
                // Text is collected by CollectHandler; don't print here
            }
            _ => {}
        }
    }

    fn flush_tool_call(&self) {
        let name = {
            let n = self.cur_tool_name.read().unwrap();
            if n.is_empty() {
                return;
            }
            n.clone()
        };

        let args = {
            let a = self.cur_tool_args.read().unwrap();
            a.clone()
        };

        // Show a summary of the tool args
        let summary = tool_arg_summary(&name, &args);
        if !summary.is_empty() {
            eprintln!("  [{}]: {}", name, summary);
        } else {
            eprintln!("  [{}]", name);
        }

        let mut n = self.cur_tool_name.write().unwrap();
        *n = String::new();
        let mut a = self.cur_tool_args.write().unwrap();
        a.clear();
    }
}

impl Default for TerminalHandler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn tool_arg_summary(tool_name: &str, args_json: &str) -> String {
    let input: std::collections::HashMap<String, serde_json::Value> =
        serde_json::from_str(args_json).unwrap_or_default();

    match tool_name {
        "read_file" | "write_file" | "edit_file" | "multi_edit" | "fileops" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                if !path.is_empty() {
                    return path.to_string();
                }
            }
        }
        "list_dir" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                if !path.is_empty() {
                    return path.to_string();
                }
            }
            return ".".to_string(); // Default to current directory
        }
        "exec" | "terminal" => {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                if cmd.len() > 120 {
                    return format!("{}...", truncate_at(cmd, 120));
                }
                return cmd.to_string();
            }
        }
        "grep" => {
            if let (Some(pattern), Some(path)) = (
                input.get("pattern").and_then(|v| v.as_str()),
                input.get("path").and_then(|v| v.as_str()),
            ) {
                return format!("\"{}\" in {}", pattern, path);
            }
            if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
                return pattern.to_string();
            }
        }
        "glob" => {
            if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
                return pattern.to_string();
            }
        }
        "system" => {
            if let Some(op) = input.get("operation").and_then(|v| v.as_str()) {
                return op.to_string();
            }
        }
        "git" => {
            if let Some(args) = input.get("args").and_then(|v| v.as_str()) {
                return format!("git {}", args);
            }
        }
        "web_search" | "exa_search" => {
            if let Some(query) = input.get("query").and_then(|v| v.as_str()) {
                return query.to_string();
            }
        }
        "web_fetch" => {
            if let Some(url) = input.get("url").and_then(|v| v.as_str()) {
                return url.to_string();
            }
        }
        "process" => {
            if let Some(name) = input.get("process_name").and_then(|v| v.as_str()) {
                return name.to_string();
            }
            if let Some(pid) = input.get("pid").and_then(|v| v.as_i64()) {
                return format!("PID {}", pid);
            }
        }
        "runtime_info" => {
            if let Some(show) = input.get("show").and_then(|v| v.as_str()) {
                return show.to_string();
            }
        }
        _ => {}
    }

    // Fallback: compact format
    let parts: Vec<String> = input
        .iter()
        .filter_map(|(k, v)| {
            let v_str = match v {
                serde_json::Value::String(s) if !s.is_empty() => {
                    truncate_at(s, 80).to_string()
                }
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::String(_) | serde_json::Value::Null => return None,
                _ => v.to_string(),
            };
            Some(format!("{}={}", k, v_str))
        })
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    parts.join(" ")
}

/// StallDetector monitors streaming for stalls
pub struct StallDetector {
    last_event: RwLock<Instant>,
    #[allow(dead_code)]
    stall_timeout: Duration,
    #[allow(dead_code)]
    startup_timeout: Duration,
    stall_count: RwLock<usize>,
}

impl StallDetector {
    pub fn new() -> Self {
        Self {
            last_event: RwLock::new(Instant::now()),
            stall_timeout: Duration::from_secs(90),
            startup_timeout: Duration::from_secs(120),
            stall_count: RwLock::new(0),
        }
    }

    /// Reset timer on successful event
    pub fn reset(&self) {
        let mut last = self.last_event.write().unwrap();
        *last = Instant::now();
        let mut count = self.stall_count.write().unwrap();
        *count = 0;
    }

    /// Check if stalled. Returns Some(duration) if stalled.
    #[allow(dead_code)]
    pub fn check_stall(&self) -> Option<Duration> {
        let last = *self.last_event.read().unwrap();
        let elapsed = last.elapsed();
        if elapsed > self.stall_timeout {
            Some(elapsed)
        } else {
            None
        }
    }

    /// Increment stall count and return count
    #[allow(dead_code)]
    pub fn increment_stall(&self) -> usize {
        let mut count = self.stall_count.write().unwrap();
        *count += 1;
        *count
    }

    /// Get stall timeout based on whether first event has been received
    #[allow(dead_code)]
    pub fn timeout(&self) -> Duration {
        let last = *self.last_event.read().unwrap();
        if last.elapsed() < self.startup_timeout {
            // Use startup timeout until first event
            self.startup_timeout
        } else {
            self.stall_timeout
        }
    }
}

impl Default for StallDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// ProcessSseEvents processes SSE events from the Anthropic API.
pub async fn process_sse_events(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    max_tokens: i64,
    system: &str,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
    collect: &CollectHandler,
    term: &TerminalHandler,
    stall: &Arc<StallDetector>,
    interrupted: Arc<std::sync::atomic::AtomicBool>,
) -> Result<Vec<ToolCallInfo>> {
    // Check for interruption before starting
    if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(anyhow!("Request cancelled by user"));
    }

    // Build request payload
    let mut payload = serde_json::Map::new();
    payload.insert("model".to_string(), serde_json::json!(model));
    payload.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
    // Match Go SDK format: system is an array of text blocks
    payload.insert("system".to_string(), serde_json::json!([{"type": "text", "text": system}]));
    payload.insert("messages".to_string(), serde_json::json!(messages));
    if !tools.is_empty() {
        payload.insert("tools".to_string(), serde_json::json!(tools));
    }

    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));

    // Create a cancellation token driven by the interrupted flag (polls every 100ms)
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel_token.clone();
    let interrupted_clone = interrupted.clone();
    let cancel_guard = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            if interrupted_clone.load(std::sync::atomic::Ordering::SeqCst) {
                cancel_clone.cancel();
                return;
            }
        }
    });

    // Race HTTP send against cancellation
    let response = tokio::select! {
        resp = client
            .post(&url)
            .header("x-api-key", api_key)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("anthropic-version", "2023-06-01")
            .body(serde_json::to_string(&payload)?)
            .send() => {
                match resp {
                    Ok(r) => r,
                    Err(e) => {
                        cancel_guard.abort();
                        if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                            return Err(anyhow!("Request cancelled by user"));
                        }
                        return Err(anyhow!("API request failed: {}", e));
                    }
                }
            }
        _ = cancel_token.cancelled() => {
            cancel_guard.abort();
            return Err(anyhow!("Request cancelled by user"));
        }
    };

    cancel_guard.abort();

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("API error {}: {}", status, body));
    }

    let mut stream = response.bytes_stream();

    let mut buf = String::new();
    let mut sse_detected = false;

    while let Some(result) = stream.next().await {
        // Check for interruption during streaming
        if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!("Request cancelled by user"));
        }

        let bytes = match result {
            Ok(b) => b,
            Err(e) => return Err(anyhow!("stream error: {}", e)),
        };

        // Reset stall tracking on each event
        stall.reset();

        let raw = String::from_utf8_lossy(&bytes);

        // Try to detect non-SSE JSON response (raw Anthropic message format)
        if !sse_detected {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') && !trimmed.starts_with("data:") && !trimmed.starts_with("event:") {
                // Non-SSE JSON - parse as complete message and return
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    let tool_calls = parse_anthropic_message(&msg, collect, term);
                    return Ok(tool_calls);
                }
            }
            sse_detected = true;
        }

        // Process bytes as SSE
        for b in bytes {
            if b == b'\n' {
                let line = buf.trim().to_string();
                buf.clear();

                if line.is_empty() {
                    continue;
                }

                // Parse SSE line: "data: <json>"
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                        if let Some(chunk) = parse_sse_event(&event) {
                            collect.handle(chunk.clone());
                            term.handle(chunk.clone());

                            if collect.is_tool_use_as_text() {
                                return Err(anyhow!("model confused: echoed tool syntax as text"));
                            }
                        }
                    }
                }
            } else if b != b'\r' {
                buf.push(b as char);
            }
        }
    }

    // Signal end of stream
    term.handle(StreamChunk {
        chunk_type: ChunkType::Done,
        content: String::new(),
        id: None,
        name: None,
        usage: None,
    });

    Ok(collect.tool_calls())
}

/// Parse a complete Anthropic message JSON (non-streaming format) and extract tool calls/text
pub fn parse_anthropic_message(
    msg: &serde_json::Value,
    collect: &CollectHandler,
    term: &TerminalHandler,
) -> Vec<ToolCallInfo> {
    if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                match block_type {
                    "thinking" => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            let preview = truncate_at(thinking.lines().next().unwrap_or(""), 120);
                            eprintln!("\n[THINK] {}", preview);
                        }
                    }
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            // Push text as a text chunk
                            collect.handle(StreamChunk {
                                chunk_type: ChunkType::Text,
                                content: text.to_string(),
                                id: None,
                                name: None,
                                usage: None,
                            });
                        }
                    }
                    "tool_use" => {
                        if let (Some(id), Some(name)) = (
                            block.get("id").and_then(|i| i.as_str()),
                            block.get("name").and_then(|n| n.as_str()),
                        ) {
                            let args = block.get("input").map(|i| i.to_string()).unwrap_or_default();
                            // Push tool call chunk - send to collect first, then term (same as SSE path)
                            let tool_call_chunk = StreamChunk {
                                chunk_type: ChunkType::ToolCall,
                                content: String::new(),
                                id: Some(id.to_string()),
                                name: Some(name.to_string()),
                                usage: None,
                            };
                            collect.handle(tool_call_chunk.clone());
                            term.handle(tool_call_chunk);
                            // Push arguments chunk
                            let args_chunk = StreamChunk {
                                chunk_type: ChunkType::ToolArgument,
                                content: args,
                                id: None,
                                name: None,
                                usage: None,
                            };
                            collect.handle(args_chunk.clone());
                            term.handle(args_chunk);
                            // Push block stop to flush
                            let block_stop = StreamChunk {
                                chunk_type: ChunkType::BlockStop,
                                content: String::new(),
                                id: None,
                                name: None,
                                usage: None,
                            };
                            term.handle(block_stop);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    collect.tool_calls()
}

/// Parse an SSE data event into a StreamChunk
pub fn parse_sse_event(event: &serde_json::Value) -> Option<StreamChunk> {
    // Handle different event types from Anthropic SSE
    if let Some(gamma) = event.get("type").and_then(|v| v.as_str()) {
        match gamma {
            "message_start" => {
                // Message started
                Some(StreamChunk {
                    chunk_type: ChunkType::BlockStop,
                    content: String::new(),
                    id: None,
                    name: None,
                    usage: None,
                })
            }
            "content_block_start" => {
                let content_block = event.get("content_block")?;
                let block_type = content_block.get("type")?.as_str()?;
                if block_type == "tool_use" {
                    let id = content_block.get("id")?.as_str()?.to_string();
                    let name = content_block.get("name")?.as_str()?.to_string();
                    Some(StreamChunk {
                        chunk_type: ChunkType::ToolCall,
                        content: String::new(),
                        id: Some(id),
                        name: Some(name),
                        usage: None,
                    })
                } else if block_type == "thinking" {
                    Some(StreamChunk {
                        chunk_type: ChunkType::Thinking,
                        content: String::new(),
                        id: None,
                        name: None,
                        usage: None,
                    })
                } else {
                    None
                }
            }
            "content_block_delta" => {
                let delta = event.get("delta")?;
                let delta_type = delta.get("type")?.as_str()?;
                match delta_type {
                    "text_delta" => {
                        let text = delta.get("text")?.as_str()?.to_string();
                        Some(StreamChunk {
                            chunk_type: ChunkType::Text,
                            content: text,
                            id: None,
                            name: None,
                            usage: None,
                        })
                    }
                    "input_json_delta" => {
                        let partial = delta.get("partial_json")?.as_str()?.to_string();
                        Some(StreamChunk {
                            chunk_type: ChunkType::ToolArgument,
                            content: partial,
                            id: None,
                            name: None,
                            usage: None,
                        })
                    }
                    "thinking_delta" => {
                        let thinking = delta.get("thinking")?.as_str()?.to_string();
                        Some(StreamChunk {
                            chunk_type: ChunkType::Thinking,
                            content: thinking,
                            id: None,
                            name: None,
                            usage: None,
                        })
                    }
                    _ => None,
                }
            }
            "content_block_stop" => {
                Some(StreamChunk {
                    chunk_type: ChunkType::BlockStop,
                    content: String::new(),
                    id: None,
                    name: None,
                    usage: None,
                })
            }
            "message_delta" => {
                if let Some(usage) = event.get("usage") {
                    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    Some(StreamChunk {
                        chunk_type: ChunkType::Usage,
                        content: String::new(),
                        id: None,
                        name: None,
                        usage: Some(Usage {
                            input_tokens: input_tokens,
                            output_tokens: output_tokens,
                        }),
                    })
                } else {
                    None
                }
            }
            "message_stop" => {
                Some(StreamChunk {
                    chunk_type: ChunkType::Done,
                    content: String::new(),
                    id: None,
                    name: None,
                    usage: None,
                })
            }
            _ => None,
        }
    } else {
        None
    }
}
