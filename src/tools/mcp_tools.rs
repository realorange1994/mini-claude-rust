//! MCP tools - Model Context Protocol integration

use crate::tools::{Tool, ToolResult};
use crate::mcp::Manager as McpManager;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

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
        "List available tools from MCP servers. Optionally filter by server name or pattern."
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
            let desc = if tool.description.len() > 60 {
                format!("{}...", &tool.description[..60])
            } else {
                tool.description.clone()
            };
            output.push_str(&format!("  {}\n", tool.name));
            if !desc.is_empty() {
                output.push_str(&format!("    -> {}\n", desc));
            }
        }

        ToolResult::ok(output.trim().to_string())
    }
}

pub struct McpToolCaller {
    manager: Arc<McpManager>,
}

impl McpToolCaller {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpToolCaller {
    fn name(&self) -> &str {
        "mcp_call_tool"
    }

    fn description(&self) -> &str {
        "Call a tool on an MCP server. Use list_mcp_tools first to discover available tools."
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
                    "description": "Tool name to call."
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments to pass to the tool."
                }
            },
            "required": ["tool"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let tool_name = match params.get("tool").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("Error: tool is required"),
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

        let result = if let Some(server_name) = server {
            self.manager.call_tool_with_server(server_name, tool_name, args)
        } else {
            self.manager.call_tool(tool_name, args)
        };

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
                    ToolResult::error(text)
                } else {
                    ToolResult::ok(text)
                }
            }
            Err(e) => ToolResult::error(format!("MCP tool call failed: {}", e)),
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
