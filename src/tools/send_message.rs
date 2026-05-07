//! SendMessageTool — send a message to a running sub-agent, or query its status.
//!
//! Mirrors Go's SendMessageTool. The parent agent uses this to continue
//! work on a background agent, ask for progress, or retrieve results.

use crate::tools::agent_store::SharedAgentTaskStore;
use crate::tools::{Tool, ToolResult, ToolPermissionResult};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub struct SendMessageTool {
    store: SharedAgentTaskStore,
}

impl SendMessageTool {
    pub fn new(store: SharedAgentTaskStore) -> Self {
        Self { store }
    }
}

impl Clone for SendMessageTool {
    fn clone(&self) -> Self {
        Self { store: Arc::clone(&self.store) }
    }
}

impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to a running sub-agent, or query its status. \
         Use this to continue work on a background agent, ask for progress, or retrieve results."
    }

    fn input_schema(&self) -> Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["agent_id"],
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The agent ID to send a message to (from the agent launch result). Mutually exclusive with 'name'."
                },
                "name": {
                    "type": "string",
                    "description": "The registered agent name to send a message to (mutually exclusive with 'agent_id')."
                },
                "message": {
                    "type": "string",
                    "description": "Message to send to the agent. If empty, returns the agent's current status and result (if available)."
                },
                "summary": {
                    "type": "string",
                    "description": "Optional summary of what you are requesting or informing about (for logging purposes)."
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> ToolPermissionResult {
        ToolPermissionResult::passthrough() // no permissions required
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let agent_id = params.get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let name = params.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Resolve agent_id from name if agent_id is not provided
        let resolved_id = if agent_id.is_empty() && !name.is_empty() {
            name // name resolution happens via the store
        } else {
            agent_id
        };

        if resolved_id.is_empty() {
            return ToolResult::error("either agent_id or name is required");
        }

        let message = params.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if message.is_empty() {
            // Query status only
            if let Some(task) = self.store.get(&resolved_id) {
                let status = task.status().as_str();
                let output = task.get_output();
                let elapsed = task.start_time.elapsed();
                let duration_str = if elapsed.as_secs() < 60 {
                    format!("{}s", elapsed.as_secs())
                } else {
                    format!("{:.1}m", elapsed.as_secs() as f64 / 60.0)
                };

                let mut result = format!("Agent {} status: {}\nDuration: {}", resolved_id, status, duration_str);
                if !output.is_empty() {
                    result.push_str(&format!("\nOutput ({} chars):\n{}", output.len(), output));
                }
                return ToolResult::ok(result);
            } else {
                return ToolResult::error(format!("Agent '{}' not found", resolved_id));
            }
        }

        // Send message to the agent
        let added = self.store.add_pending_message(&resolved_id, &message);
        if added {
            ToolResult::ok(format!("Message sent to agent {}", resolved_id))
        } else {
            // Agent not found or not running — return status instead
            if let Some(task) = self.store.get(&resolved_id) {
                let status = task.status().as_str();
                let output = task.get_output();
                let mut result = format!("Agent {} is not running (status: {})", resolved_id, status);
                if !output.is_empty() {
                    result.push_str(&format!("\nOutput:\n{}", output));
                }
                ToolResult::ok(result)
            } else {
                ToolResult::error(format!("Agent '{}' not found", resolved_id))
            }
        }
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }
}