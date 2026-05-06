//! MCP tools - Model Context Protocol integration
//!
//! Design: MCP tools are exposed via a single `mcp_call_tool` entry point.
//! The tool's description is dynamically generated each turn to list available
//! MCP tools by name + short summary, keeping context cost low (~30-50 tokens
//! per tool). If the LLM calls a tool with wrong params, the full schema is
//! returned in the error message so it can self-correct without extra turns.

use crate::tools::{Tool, ToolResult};
use crate::mcp::Manager as McpManager;
use crate::task_store::{SharedTaskStore, bash_bg_tasks_dir};
use crate::mcp::ToolResult as McpToolResult;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct ListMcpTools {
    manager: Arc<McpManager>,
}

impl ListMcpTools {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for ListMcpTools {
    fn name(&self) -> &str {
        "list_mcp_tools"
    }

    fn description(&self) -> &str {
        "List available tools from MCP servers with full descriptions. Use this to get detailed info about a specific MCP tool."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Filter by MCP server name."
                },
                "pattern": {
                    "type": "string",
                    "description": "Filter by tool name pattern."
                }
            },
            "required": []
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let server = params.get("server").and_then(|v| v.as_str());
        let pattern = params.get("pattern").and_then(|v| v.as_str());

        let all_tools = self.manager.list_tools();

        let mut filtered: Vec<_> = all_tools;

        if let Some(server) = server {
            filtered.retain(|t| t.name.contains(server));
        }

        if let Some(pattern) = pattern {
            let pattern_lower = pattern.to_lowercase();
            filtered.retain(|t| t.name.to_lowercase().contains(&pattern_lower));
        }

        if filtered.is_empty() {
            let servers = self.manager.list_servers();
            if servers.is_empty() {
                return ToolResult::ok("No MCP servers configured.".to_string());
            }
            return ToolResult::ok("No MCP tools found.".to_string());
        }

        let mut output = format!("MCP Tools ({} total)\n", filtered.len());
        for tool in filtered {
            let desc = if tool.description.len() > 120 {
                let mut end = 120;
                while end > 0 && !tool.description.is_char_boundary(end) { end -= 1; }
                format!("{}...", &tool.description[..end])
            } else {
                tool.description.clone()
            };
            output.push_str(&format!("  {}\n", tool.name));
            if !desc.is_empty() {
                output.push_str(&format!("    -> {}\n", desc));
            }
            // Include full schema so LLM can learn params
            if !tool.input_schema.is_null() {
                if let Some(schema_str) = serde_json::to_string_pretty(&tool.input_schema).ok() {
                    // Limit schema size to avoid flooding context
                    if schema_str.len() <= 500 {
                        output.push_str(&format!("    schema: {}\n", schema_str));
                    } else {
                        // For large schemas, only show required fields and property names
                        output.push_str(&format!("    schema: (large, use list_mcp_tools with pattern='{}' for full schema)\n", tool.name));
                    }
                }
            }
        }

        ToolResult::ok(output.trim().to_string())
    }


}

pub struct McpToolCaller {
    manager: Arc<McpManager>,
    task_store: SharedTaskStore,
}

/// Timeout (seconds) before an MCP tool call is moved to background.
const MCP_TOOL_TIMEOUT_SECS: u64 = 120;

impl McpToolCaller {
    pub fn new(manager: Arc<McpManager>, task_store: SharedTaskStore) -> Self {
        Self { manager, task_store }
    }
}

impl Tool for McpToolCaller {
    fn name(&self) -> &str {
        "mcp_call_tool"
    }

    /// Dynamically generate description listing all available MCP tools.
    /// This keeps the LLM informed of available tools without registering
    /// each one as a separate tool (which would consume far more context).
    fn description(&self) -> &str {
        // SAFETY: We leak a small string once per call to get a 'static lifetime.
        // This is acceptable because:
        // 1. The description is called once per API request to build the tools schema
        // 2. Each leak replaces the previous one (the old string is unreachable after)
        // 3. The total size is small (~1-3KB for typical MCP setups)
        // 4. The alternative (returning owned String) would require changing the Tool trait

        let tools = self.manager.list_tools();

        let mut desc = String::from(
            "Call a tool on an MCP server. Available MCP tools (use exact name as 'tool' param):\n"
        );

        if tools.is_empty() {
            desc.push_str("  (none - no MCP servers connected)");
        } else {
            for tool in &tools {
                // Name + one-line description (truncate to ~80 chars to save context)
                let short_desc = if tool.description.is_empty() {
                    String::new()
                } else if tool.description.len() <= 80 {
                    tool.description.clone()
                } else {
                    let mut end = 80;
                    while end > 0 && !tool.description.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...", &tool.description[..end])
                };

                if short_desc.is_empty() {
                    desc.push_str(&format!("  - {}\n", tool.name));
                } else {
                    desc.push_str(&format!("  - {}: {}\n", tool.name, short_desc));
                }
            }
            desc.push_str(
                "\nIf unsure about a tool's parameters, call list_mcp_tools first for the full schema. \
                If you get a parameter error, the correct schema will be included in the error message."
            );
        }

        // Leak the string to get 'static lifetime.
        // Previous leaked strings become unreachable and will be reclaimed by the OS at exit.
        Box::leak(desc.into_boxed_str())
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "MCP server name (optional, auto-detected if omitted)."
                },
                "tool": {
                    "type": "string",
                    "description": "Exact MCP tool name to call (e.g. 'coze_web_search')."
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments to pass to the tool. Must match the tool's expected schema."
                }
            },
            "required": ["tool"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly, crate::tools::ToolCapability::Network]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Classifier
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let tool_name = match params.get("tool").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("Error: 'tool' is required"),
        };

        let server = params.get("server").and_then(|v| v.as_str());
        let args: HashMap<String, Value> = params
            .get("arguments")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        // Spawn MCP call in a background thread, wait with timeout
        let result_slot: Arc<std::sync::Mutex<Option<Result<crate::mcp::ToolResult, String>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let result_slot_clone = Arc::clone(&result_slot);

        let manager = Arc::clone(&self.manager);
        let tool_name_owned = tool_name.to_string();
        let server_owned = server.map(|s| s.to_string());

        let handle = std::thread::Builder::new()
            .name(format!("mcp-timeout-{}", tool_name))
            .spawn(move || {
                let result = if let Some(server_name) = server_owned {
                    manager.call_tool_with_server(&server_name, &tool_name_owned, args)
                } else {
                    manager.call_tool(&tool_name_owned, args)
                };
                *result_slot_clone.lock().unwrap() = Some(result);
            })
            .unwrap(); // thread spawn should not fail in practice

        // Wait for result with timeout
        let deadline = Instant::now() + Duration::from_secs(MCP_TOOL_TIMEOUT_SECS);
        loop {
            {
                let mut slot = result_slot.lock().unwrap();
                if let Some(result) = slot.take() {
                    // Normal return — result is ready
                    drop(slot);
                    let _ = handle.join(); // clean up thread
                    return self.map_mcp_result(&tool_name, result);
                }
            }

            if Instant::now() >= deadline {
                // Timeout — move to background
                return self.move_to_background(tool_name, server, result_slot, handle);
            }

            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl McpToolCaller {
    /// Map an MCP ToolResult to our tool ToolResult, including schema hints on error.
    fn map_mcp_result(&self, tool_name: &str, result: Result<McpToolResult, String>) -> ToolResult {
        match result {
            Ok(tool_result) => {
                let text = tool_result.content
                    .iter()
                    .filter_map(|c| match c {
                        crate::mcp::ToolResultContent::Text { text } => Some(text.clone()),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if tool_result.is_error {
                    let schema_hint = self.get_tool_schema_hint(tool_name);
                    let mut output = text;
                    if let Some(hint) = schema_hint {
                        output.push_str(&format!("\n\n--- Tool schema for {} ---\n{}", tool_name, hint));
                    }
                    ToolResult::error(output)
                } else {
                    ToolResult::ok(text)
                }
            }
            Err(e) => {
                let mut output = format!("MCP tool call failed: {}", e);
                let schema_hint = self.get_tool_schema_hint(tool_name);
                if let Some(hint) = schema_hint {
                    output.push_str(&format!("\n\n--- Tool schema for {} ---\n{}", tool_name, hint));
                }
                ToolResult::error(output)
            }
        }
    }

    /// Move a timed-out MCP tool call to background: register task in TaskStore,
    /// return task_id to the caller. Background thread completes the task later.
    fn move_to_background(
        &self,
        tool_name: &str,
        server: Option<&str>,
        result_slot: Arc<std::sync::Mutex<Option<Result<McpToolResult, String>>>>,
        handle: std::thread::JoinHandle<()>,
    ) -> ToolResult {
        let output_dir = bash_bg_tasks_dir();
        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            return ToolResult::error(format!("Failed to create task output directory: {}", e));
        }

        let task_id = self.task_store.register_bg_task(
            "mcp_background",
            format!("MCP tool: {}{}", tool_name, server.map(|s| format!(" on {}", s)).unwrap_or_default()),
            output_dir.join(format!("mcp_{}.txt", tool_name.replace('/', "_"))).to_string_lossy().to_string(),
        );

        // Background thread: wait for MCP call to finish, then write output and complete task
        let task_store = Arc::clone(&self.task_store);
        let task_id_clone = task_id.clone();
        let tool_name_clone = tool_name.to_string();
        let output_dir_clone = output_dir.clone();
        let server_owned = server.map(|s| s.to_string());

        std::thread::Builder::new()
            .name(format!("mcp-bg-{}", task_id_clone))
            .spawn(move || {
                // Wait for the original thread to finish
                let result = match handle.join() {
                    Ok(()) => {
                        // Read result from shared slot
                        result_slot.lock().unwrap().take()
                            .unwrap_or_else(|| Err("thread finished but no result was written".to_string()))
                    }
                    Err(_) => Err("background thread panicked".to_string()),
                };

                // Write output file and complete/fail task
                let output_path = output_dir_clone.join(format!("mcp_{}.txt", tool_name_clone.replace('/', "_")));
                let output_text = match &result {
                    Ok(tr) => tr.content
                        .iter()
                        .filter_map(|c| match c {
                            crate::mcp::ToolResultContent::Text { text } => Some(text.clone()),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(e) => e.clone(),
                };

                let is_error = match &result {
                    Ok(tr) => tr.is_error,
                    Err(_) => true,
                };

                // Write output file
                let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let server_display = server_owned.as_deref().unwrap_or("auto");
                let file_content = format!(
                    "--- MCP Background Task: {} ---\n\
                     Tool: {}\n\
                     Server: {}\n\
                     Started: {}\n\
                     --- Output ---\n\
                     {}\n\
                     --- Status: {} ---\n",
                    task_id_clone,
                    tool_name_clone,
                    server_display,
                    now,
                    output_text,
                    if is_error { "failed" } else { "completed" },
                );
                let _ = std::fs::write(&output_path, &file_content);

                if is_error {
                    task_store.fail_task(&task_id_clone, &format!("MCP tool error: {}", output_text));
                } else {
                    task_store.complete_task(&task_id_clone, "MCP tool completed");
                }
            })
            .unwrap();

        let server_info = server.map(|s| format!(" on server '{}'", s)).unwrap_or_default();
        ToolResult::ok(format!(
            "MCP tool '{}' timed out after {}s and moved to background.{}\n\
             Task ID: {}\n\
             Use task_output tool to check the result when ready.",
            tool_name, MCP_TOOL_TIMEOUT_SECS, server_info, task_id
        ))
    }

    /// Get a compact schema hint for a tool, used in error feedback.
    /// Returns None if the tool doesn't exist.
    fn get_tool_schema_hint(&self, tool_name: &str) -> Option<String> {
        let tools = self.manager.list_tools();
        let tool = tools.iter().find(|t| t.name == tool_name)?;

        if tool.input_schema.is_null() {
            return Some(format!("(no input schema defined for {})", tool_name));
        }

        let schema_str = serde_json::to_string_pretty(&tool.input_schema).ok()?;

        // Limit schema size in error feedback
        if schema_str.len() <= 1000 {
            Some(schema_str)
        } else {
            // Extract just the required fields and property names for large schemas
            let mut compact = String::new();
            if let Some(required) = tool.input_schema.get("required").and_then(|r| r.as_array()) {
                let names: Vec<&str> = required.iter()
                    .filter_map(|v| v.as_str())
                    .collect();
                if !names.is_empty() {
                    compact.push_str(&format!("required: [{}]\n", names.join(", ")));
                }
            }
            if let Some(props) = tool.input_schema.get("properties").and_then(|p| p.as_object()) {
                compact.push_str("properties:\n");
                for (name, val) in props {
                    let type_str = val.get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown");
                    let desc = val.get("description")
                        .and_then(|d| d.as_str())
                        .map(|d| {
                            if d.len() > 60 {
                                let mut end = 60;
                                while end > 0 && !d.is_char_boundary(end) { end -= 1; }
                                format!("{}...", &d[..end])
                            } else {
                                d.to_string()
                            }
                        })
                        .unwrap_or_default();
                    if desc.is_empty() {
                        compact.push_str(&format!("  {}: {}\n", name, type_str));
                    } else {
                        compact.push_str(&format!("  {}: {} - {}\n", name, type_str, desc));
                    }
                }
            }
            Some(compact)
        }
    }
}

pub struct McpServerStatus {
    manager: Arc<McpManager>,
}

impl McpServerStatus {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpServerStatus {
    fn name(&self) -> &str {
        "mcp_server_status"
    }

    fn description(&self) -> &str {
        "Check the connection status of MCP servers."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Filter by server name."
                }
            },
            "required": []
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let server = params.get("server").and_then(|v| v.as_str());
        let servers = self.manager.list_servers();

        if servers.is_empty() {
            return ToolResult::ok("No MCP servers configured.".to_string());
        }

        let mut output = String::from("MCP Server Status\n");

        for name in servers {
            if let Some(filter) = server {
                if name != filter {
                    continue;
                }
            }

            let status = self.manager.get_server_status(&name);
            let icon = if status == "connected" { "[OK]" } else { "[FAIL]" };
            output.push_str(&format!("{} {}: {}\n", icon, name, status));
        }

        ToolResult::ok(output.trim().to_string())
    }
}
