//! TodoWriteTool — update the agent's structured todo list.
//!
//! The model calls this tool to create, update, or delete tasks.
//! The list is injected into the system prompt as a reminder.

use crate::context::{TodoItem, TodoList};
use crate::tools::{Tool, ToolResult};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub struct TodoWriteTool {
    todo_list: Arc<TodoList>,
}

impl TodoWriteTool {
    pub fn new(todo_list: Arc<TodoList>) -> Self {
        Self { todo_list }
    }
}

impl Clone for TodoWriteTool {
    fn clone(&self) -> Self {
        Self { todo_list: Arc::clone(&self.todo_list) }
    }
}

impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Update the todo list for the current session. To be used proactively and often to track progress and pending tasks. \
         Make sure that at least one task is in_progress at all times. \
         Always provide both content (imperative form, e.g. 'Fix authentication bug') and activeForm (present continuous, e.g. 'Fixing authentication bug') for each task.\n\n\
         ## When to Use This Tool\n\
         Use this tool proactively in these scenarios:\n\n\
         1. Complex multi-step tasks - When a task requires 3 or more distinct steps or actions\n\
         2. Non-trivial and complex tasks - Tasks that require careful planning or multiple operations\n\
         3. User explicitly requests todo list - When the user directly asks you to use the todo list\n\
         4. User provides multiple tasks - When users provide a list of things to be done (numbered or comma-separated)\n\
         5. After receiving new instructions - Immediately capture user requirements as todos\n\
         6. When you start working on a task - Mark it as in_progress BEFORE beginning work. Ideally you should only have one todo as in_progress at a time\n\
         7. After completing a task - Mark it as completed and add any new follow-up tasks discovered during implementation\n\n\
         ## When NOT to Use This Tool\n\n\
         Skip using this tool when:\n\
         1. There is only a single, straightforward task\n\
         2. The task is trivial and tracking it provides no organizational benefit\n\
         3. The task can be completed in less than 3 trivial steps\n\
         4. The task is purely conversational or informational\n\n\
         ## Task States and Management\n\n\
         1. Task States: Use these states to track progress:\n\
            - pending: Task not yet started\n\
            - in_progress: Currently working on (limit to ONE task at a time)\n\
            - completed: Task finished successfully\n\n\
         2. Task Management:\n\
            - Update task status in real-time as you work\n\
            - Mark tasks complete IMMEDIATELY after finishing (don't batch completions)\n\
            - Exactly ONE task must be in_progress at any time (not less, not more)\n\
            - Complete current tasks before starting new ones\n\
            - Remove tasks that are no longer relevant from the list entirely\n\n\
         3. Task Completion Requirements:\n\
            - ONLY mark a task as completed when you have FULLY accomplished it\n\
            - If you encounter errors, blockers, or cannot finish, keep the task as in_progress\n\
            - When blocked, create a new task describing what needs to be resolved\n\
            - Never mark a task as completed if:\n\
              - Tests are failing\n\
              - Implementation is partial\n\
              - You encountered unresolved errors\n\n\
         When in doubt, use this tool. Being proactive with task management demonstrates attentiveness and ensures you complete all requirements successfully."
    }

    fn input_schema(&self) -> Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["todos"],
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Complete list of tasks (replaces previous list)",
                    "items": {
                        "type": "object",
                        "required": ["content", "status"],
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "Task description in imperative form (e.g., 'Fix authentication bug')"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current task status"
                            },
                            "activeForm": {
                                "type": "string",
                                "description": "Present continuous form shown in spinner (e.g., 'Running tests')"
                            }
                        }
                    }
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None // no permissions required
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let todos = match params.get("todos") {
            Some(Value::Array(arr)) => arr.clone(),
            _ => {
                return ToolResult::error("todos must be an array");
            }
        };

        let mut items = Vec::new();
        for raw in todos {
            let obj = match raw {
                Value::Object(m) => m,
                _ => continue,
            };

            let content = obj.get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let status_str = obj.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");

            let status = match status_str {
                "in_progress" => crate::context::TodoStatus::InProgress,
                "completed" => crate::context::TodoStatus::Completed,
                _ => crate::context::TodoStatus::Pending,
            };

            let active_form = obj.get("activeForm")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            items.push(TodoItem { content, status, active_form });
        }

        self.todo_list.update(items.clone());

        // Matching upstream: reinforce that the model should use the todo list
        // to track progress and proceed with the current task.
        ToolResult::ok("Todos have been successfully. Ensure that you use the todo list to track your progress. Please proceed with the current tasks as applicable")
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }
}
