//! AgentTool — spawns a sub-agent to handle complex, multi-step tasks.

use crate::context::ConversationContext;
use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Callback signature for spawning a child agent loop.
/// Returns (agent_id, result_text, error_text, tools_used, duration_ms).
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
    Option<Arc<tokio::sync::RwLock<ConversationContext>>>,  // parent_context
) -> (String, String, String, usize, u64) + Send + Sync>;

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
        F: Fn(&str, &str, &str, bool, &[String], &[String], bool, Option<Arc<tokio::sync::RwLock<ConversationContext>>>) -> (String, String, String, usize, u64)
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
         Supports both synchronous (default) and asynchronous (run_in_background=true) execution."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["description", "prompt"],
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Brief 3-5 word description of what the agent will do"
                },
                "prompt": {
                    "type": "string",
                    "description": "The complete task for the agent to perform. Be specific and include all necessary context."
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Type of specialized agent to use (optional). Leave blank for general-purpose."
                },
                "model": {
                    "type": "string",
                    "description": "Model override for the agent (optional). Defaults to parent's model."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run the agent in the background and return immediately (optional, default false)."
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
                }
            }
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
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
        let run_in_background = params.get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let inherit_context = params.get("inherit_context")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let allowed_tools = extract_string_list(params.get("allowed_tools"));
        let mut disallowed_tools = extract_string_list(params.get("disallowed_tools"));

        // Always disallow recursive agent spawning
        disallowed_tools.push("agent".to_string());

        if run_in_background {
            // Async path: SpawnFunc launches the task internally and returns the agent_id
            let (agent_id, _, _, _, _) = spawn_func(
                &prompt, &subagent_type, &model, true,
                &allowed_tools, &disallowed_tools, inherit_context,
                None, // parent_context set by the spawn_func closure
            );
            return ToolResult::ok(format!(
                "Agent launched in background.\n\n\
                 agentId: {agent_id}\n\
                 Status: async_launched\n\
                 Description: {description}",
            ));
        }

        // Sync path: block until complete
        let (agent_id, result, err_text, tools_used, duration_ms) = spawn_func(
            &prompt, &subagent_type, &model, false,
            &allowed_tools, &disallowed_tools, inherit_context,
            None, // parent_context set by the spawn_func closure
        );

        if !err_text.is_empty() {
            return ToolResult::error(err_text);
        }

        // Explore and plan agents return raw results without usage trailer
        let skip_usage = subagent_type == "explore" || subagent_type == "plan";
        let formatted = format_agent_result(&result, &agent_id, &subagent_type, tools_used, duration_ms, skip_usage);
        ToolResult::ok(formatted)
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

/// Formats a sub-agent's output with usage metadata.
/// When skip_usage is true, only the result text is returned (used for explore/plan agents).
fn format_agent_result(
    result: &str,
    agent_id: &str,
    agent_type: &str,
    tools_used: usize,
    duration_ms: u64,
    skip_usage: bool,
) -> String {
    if skip_usage {
        return result.to_string();
    }
    let mut output = String::with_capacity(result.len() + 200);
    output.push_str(result);
    output.push_str("\n\n---\n");
    if !agent_id.is_empty() {
        output.push_str(&format!("agentId: {agent_id}\n"));
    }
    if !agent_type.is_empty() {
        output.push_str(&format!("agentType: {agent_type}\n"));
    }
    output.push_str(&format!("<usage>tool_uses: {tools_used}\nduration_ms: {duration_ms}</usage>"));
    output
}
