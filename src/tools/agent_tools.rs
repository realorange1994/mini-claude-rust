//! Agent management tools — agent_list, agent_get, agent_kill.
//!
//! These tools allow the LLM (and the user via /agents) to inspect and
//! control background sub-agent tasks.

use crate::tools::agent_store::{AgentTaskStatus, SharedAgentTaskStore};
use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// ─── AgentListTool ──────────────────────────────────────────────────────────

pub struct AgentListTool {
    store: SharedAgentTaskStore,
}

impl AgentListTool {
    pub fn new(store: SharedAgentTaskStore) -> Self {
        Self { store }
    }
}

impl Tool for AgentListTool {
    fn name(&self) -> &str {
        "agent_list"
    }

    fn description(&self) -> &str {
        "List all background sub-agent tasks with their status. Optionally filter by status (pending, running, completed, failed, killed)."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "description": "Optional status filter: pending, running, completed, failed, killed"
                }
            }
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let status_filter = params
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(|s| AgentTaskStatus::from_str(s));

        let tasks = match status_filter {
            Some(status) => self.store.list_by_status(status),
            None => self.store.list(),
        };

        if tasks.is_empty() {
            let msg = match status_filter {
                Some(s) => format!("No agents with status '{}'.", s),
                None => "No agents running.".to_string(),
            };
            return ToolResult::ok(msg);
        }

        // Format as a table
        let mut output = String::new();
        output.push_str(&format!(
            "{:<10} {:<12} {:<30} {:<25} {:<10}\n",
            "ID", "Status", "Description", "Model", "Started"
        ));
        output.push_str(&"-".repeat(87));
        output.push('\n');

        for task in &tasks {
            let elapsed = task.start_time.elapsed();
            let started = if elapsed.as_secs() < 60 {
                format!("{}s ago", elapsed.as_secs())
            } else if elapsed.as_secs() < 3600 {
                format!("{}m ago", elapsed.as_secs() / 60)
            } else {
                format!("{}h ago", elapsed.as_secs() / 3600)
            };
            let desc = if task.description.len() > 28 {
                format!("{}...", &task.description[..25])
            } else {
                task.description.clone()
            };
            let model = if task.model.is_empty() {
                "-".to_string()
            } else if task.model.len() > 23 {
                format!("{}...", &task.model[..20])
            } else {
                task.model.clone()
            };
            output.push_str(&format!(
                "{:<10} {:<12} {:<30} {:<25} {:<10}\n",
                task.id,
                task.status().as_str(),
                desc,
                model,
                started,
            ));
        }

        output.push_str(&format!("\nTotal: {} agent(s)", tasks.len()));
        ToolResult::ok(output)
    }
}

// ─── AgentGetTool ───────────────────────────────────────────────────────────

pub struct AgentGetTool {
    store: SharedAgentTaskStore,
}

impl AgentGetTool {
    pub fn new(store: SharedAgentTaskStore) -> Self {
        Self { store }
    }
}

impl Tool for AgentGetTool {
    fn name(&self) -> &str {
        "agent_get"
    }

    fn description(&self) -> &str {
        "Get details of a specific sub-agent task including its captured output (tail lines)."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["agent_id"],
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The 8-char hex ID of the agent task"
                },
                "tail": {
                    "type": "integer",
                    "description": "Number of output lines to show from the end (default: 50, max: 200)",
                    "default": 50
                }
            }
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("agent_id is required"),
        };

        let tail = params
            .get("tail")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(200) as usize;

        let task = match self.store.get(&agent_id) {
            Some(t) => t,
            None => return ToolResult::error(format!("Agent '{}' not found", agent_id)),
        };

        let elapsed = task.start_time.elapsed();
        let duration_str = if elapsed.as_secs() < 60 {
            format!("{}s", elapsed.as_secs())
        } else {
            format!("{:.1}m", elapsed.as_secs() as f64 / 60.0)
        };

        let mut output = String::new();
        output.push_str(&format!("Agent ID:       {}\n", task.id));
        output.push_str(&format!("Status:         {}\n", task.status()));
        output.push_str(&format!("Description:    {}\n", task.description));
        output.push_str(&format!("Type:           {}\n", task.subagent_type));
        output.push_str(&format!("Model:          {}\n", if task.model.is_empty() { "-" } else { &task.model }));
        output.push_str(&format!("Duration:       {}\n", duration_str));
        output.push_str(&format!("Tools used:     {}\n", task.tools_used()));

        // Show output tail
        let raw_output = task.get_output();
        if !raw_output.is_empty() {
            let lines: Vec<&str> = raw_output.lines().collect();
            let total_lines = lines.len();
            let tail_lines: Vec<&&str> = lines.iter().rev().take(tail).collect();
            let mut tail_lines: Vec<&&str> = tail_lines.into_iter().rev().collect();

            output.push_str(&format!(
                "\n--- Output (last {}/{} lines) ---\n",
                tail_lines.len().min(total_lines),
                total_lines
            ));
            for line in &tail_lines {
                output.push_str(line);
                output.push('\n');
            }
        } else {
            output.push_str("\n--- Output: (none yet) ---\n");
        }

        ToolResult::ok(output)
    }
}

// ─── AgentKillTool ──────────────────────────────────────────────────────────

pub struct AgentKillTool {
    store: SharedAgentTaskStore,
}

impl AgentKillTool {
    pub fn new(store: SharedAgentTaskStore) -> Self {
        Self { store }
    }
}

impl Tool for AgentKillTool {
    fn name(&self) -> &str {
        "agent_kill"
    }

    fn description(&self) -> &str {
        "Kill a running sub-agent task by its ID. The agent will be cancelled and marked as killed."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["agent_id"],
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The 8-char hex ID of the agent task to kill"
                }
            }
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("agent_id is required"),
        };

        // Check if task exists first
        let task = match self.store.get(&agent_id) {
            Some(t) => t,
            None => return ToolResult::error(format!("Agent '{}' not found", agent_id)),
        };

        if task.is_terminal() {
            return ToolResult::ok(format!(
                "Agent '{}' is already in terminal state: {}",
                agent_id, task.status()
            ));
        }

        let killed = self.store.kill(&agent_id);
        if killed {
            ToolResult::ok(format!("Agent '{}' has been killed.", agent_id))
        } else {
            ToolResult::error(format!(
                "Failed to kill agent '{}' (may have already terminated)",
                agent_id
            ))
        }
    }
}

// ─── Registration helper ────────────────────────────────────────────────────

/// Register all agent management tools.
pub fn register_agent_tools(registry: &crate::tools::Registry, store: &SharedAgentTaskStore) {
    registry.register(AgentListTool::new(Arc::clone(store)));
    registry.register(AgentGetTool::new(Arc::clone(store)));
    registry.register(AgentKillTool::new(Arc::clone(store)));
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use crate::tools::agent_store::AgentTaskStore;
    use tokio_util::sync::CancellationToken;

    fn make_store() -> SharedAgentTaskStore {
        Arc::new(AgentTaskStore::new())
    }

    #[test]
    fn test_agent_list_empty() {
        let store = make_store();
        let tool = AgentListTool::new(store);
        let result = tool.execute(HashMap::new());
        assert!(!result.is_error);
        assert!(result.output.contains("No agents"));
    }

    #[test]
    fn test_agent_list_with_tasks() {
        let store = make_store();
        store.create("test task", "general", "do something", "test-model");
        let tool = AgentListTool::new(store);
        let result = tool.execute(HashMap::new());
        assert!(!result.is_error);
        assert!(result.output.contains("test task"));
        assert!(result.output.contains("Total: 1"));
    }

    #[test]
    fn test_agent_list_with_status_filter() {
        let store = make_store();
        let t1 = store.create("running task", "", "", "");
        store.start(&t1.id, CancellationToken::new());
        store.create("pending task", "", "", "");

        let tool = AgentListTool::new(store.clone());
        let mut params = HashMap::new();
        params.insert("status".to_string(), serde_json::json!("running"));
        let result = tool.execute(params);
        assert!(!result.is_error);
        assert!(result.output.contains("running task"));
        assert!(!result.output.contains("pending task"));
    }

    #[test]
    fn test_agent_get_not_found() {
        let store = make_store();
        let tool = AgentGetTool::new(store);
        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), serde_json::json!("nonexistent"));
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[test]
    fn test_agent_get_with_output() {
        let store = make_store();
        let task = store.create("test", "", "", "");
        task.write_output("Hello from agent\nLine 2\n");

        let tool = AgentGetTool::new(store);
        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), serde_json::json!(task.id));
        let result = tool.execute(params);
        assert!(!result.is_error);
        assert!(result.output.contains("Hello from agent"));
        assert!(result.output.contains("Line 2"));
    }

    #[test]
    fn test_agent_kill_success() {
        let store = make_store();
        let task = store.create("task", "", "", "");
        store.start(&task.id, CancellationToken::new());

        let tool = AgentKillTool::new(store.clone());
        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), serde_json::json!(task.id));
        let result = tool.execute(params);
        assert!(!result.is_error);
        assert!(result.output.contains("killed"));

        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status(), AgentTaskStatus::Killed);
    }

    #[test]
    fn test_agent_kill_not_found() {
        let store = make_store();
        let tool = AgentKillTool::new(store);
        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), serde_json::json!("bogus"));
        let result = tool.execute(params);
        assert!(result.is_error);
    }

    #[test]
    fn test_agent_kill_already_terminal() {
        let store = make_store();
        let task = store.create("task", "", "", "");
        store.complete(&task.id);

        let tool = AgentKillTool::new(store);
        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), serde_json::json!(task.id));
        let result = tool.execute(params);
        assert!(!result.is_error);
        assert!(result.output.contains("already in terminal state"));
    }
}
