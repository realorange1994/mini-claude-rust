//! AgentTool — spawns a sub-agent to handle complex, multi-step tasks.

use crate::context::ConversationContext;
use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Callback signature for spawning a child agent loop.
/// Returns (agent_id, result_text, error_text, output_file, tools_used, duration_ms).
///
/// The `parent_context` parameter provides access to the parent agent's conversation
/// context for fork mode (inherit_context=true). It is None when the tool is called
/// outside of an agent loop context.
pub type AgentSpawnFunc = Arc<dyn Fn(
    &str,  // prompt
    &str,  // subagent_type
    &str,  // model
    bool,  // run_in_background
    &[String],    // allowed_tools
    &[String],    // disallowed_tools
    bool,  // inherit_context
    usize, // max_turns
    Option<Arc<tokio::sync::RwLock<ConversationContext>>>,  // parent_context
) -> (String, String, String, String, usize, u64) + Send + Sync>;

/// AgentTool spawns a child agent to execute a specialized task.
pub struct AgentTool {
    pub spawn_func: Option<AgentSpawnFunc>,
}

impl AgentTool {
    pub fn new() -> Self {
        Self { spawn_func: None }
    }

    pub fn with_spawn_func<F>(f: F) -> Self
    where
        F: Fn(&str, &str, &str, bool, &[String], &[String], bool, usize, Option<Arc<tokio::sync::RwLock<ConversationContext>>>) -> (String, String, String, String, usize, u64)
            + Send + Sync + 'static,
    {
        Self {
            spawn_func: Some(Arc::new(f)),
        }
    }

    /// Create with an already-wrapped AgentSpawnFunc.
    pub fn with_spawn_func_arc(spawn_func: AgentSpawnFunc) -> Self {
        Self {
            spawn_func: Some(spawn_func),
        }
    }
}

impl Default for AgentTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for AgentTool {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Launch a sub-agent to handle a complex, multi-step task autonomously. \
         Use this tool (NOT mcp_call_tool or any MCP LLM tool) when the user wants to dispatch, delegate, \
         or assign a task to a sub-agent. Sub-agents have their own isolated conversation context and tool access. \
         Sub-agents ALWAYS run in the background — you will receive an agentId immediately and do NOT need to wait for completion. \
         Do NOT call task_output to wait for the agent; continue working on other tasks in parallel."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["description", "prompt"],
            "properties": {
                "description": {
                    "type": "string",
                    "description": "A short (3-5 word) description of the task"
                },
                "prompt": {
                    "type": "string",
                    "description": "The task for the agent to perform"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Type of specialized agent to use (optional). Leave blank for general-purpose."
                },
                "model": {
                    "type": "string",
                    "enum": ["sonnet", "opus", "haiku"],
                    "description": "Model override for the agent (optional). Defaults to parent's model."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "DEPRECATED — sub-agents always run in background. This parameter is ignored."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Explicit whitelist of tools the agent can use (optional). Use [\"*\"] for all tools."
                },
                "disallowed_tools": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Tools the agent cannot use (optional). The 'agent' tool is always disallowed."
                },
                "inherit_context": {
                    "type": "boolean",
                    "description": "Fork mode: inherit the parent's conversation history (optional, default false). When true, the sub-agent sees the parent's full conversation context."
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Maximum number of turns the sub-agent can execute before being forcibly stopped (optional, default 200). A turn is one user/assistant exchange. Set a reasonable limit to prevent runaway agents."
                }
            }
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::Subprocess]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Classifier
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let spawn_func = match &self.spawn_func {
            Some(f) => f.clone(),
            None => return ToolResult::error("agent system not initialized"),
        };

        // Extract prompt
        let prompt = match params.get("prompt").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::error("prompt is required and must be a non-empty string"),
        };

        let description = params.get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let subagent_type = params.get("subagent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let model = params.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let inherit_context = params.get("inherit_context")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Extract max_turns — default to 200 for safety ceiling.
        let max_turns = params.get("max_turns")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(200);

        let allowed_tools = extract_string_list(params.get("allowed_tools"));
        let mut disallowed_tools = extract_string_list(params.get("disallowed_tools"));

        // Always disallow recursive agent spawning
        disallowed_tools.push("agent".to_string());

        // Sub-agents always run in background — sync path has been removed.
        let (agent_id, _, _, output_file, _, _) = spawn_func(
            &prompt, &subagent_type, &model, true,
            &allowed_tools, &disallowed_tools, inherit_context,
            max_turns,
            None, // parent_context set by the spawn_func closure
        );
        return ToolResult::ok(format!(
            "Agent launched in background.\n\n\
             agentId: {agent_id}\n\
             Status: async_launched\n\
             output_file: {output_file}\n\
             Description: {description}\n\n\
             The agent is working in the background. You will be notified automatically when it completes.\n\
             Do NOT call task_output to wait for this agent — it will block your turn and prevent you from responding to the user.\n\
             Do not duplicate this agent's work — avoid working with the same files or topics it is using.\n\
             Briefly tell the user what you launched, then end your response. The notification will arrive in a separate turn.",
        ));
    }
}

/// Extracts a list of strings from a JSON array value.
fn extract_string_list(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(arr)) = value else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
}
