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
        "Update your task list. Use this to track multi-step work. \
         Create tasks when starting non-trivial work, update status as you progress, \
         mark completed when done. Mark each task as completed as soon as you are done \
         with the task. Do not batch up multiple tasks before marking them as completed. \
         The list is shown in the system prompt as a reminder. \
         Call this tool with the full updated list — it replaces the previous list."
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

        // Build concise result
        let mut sb = String::from("Todo list updated:\n");
        for item in &items {
            let icon = match item.status {
                crate::context::TodoStatus::Pending => "\u{25cb} ",
                crate::context::TodoStatus::InProgress => "\u{25d0} ",
                crate::context::TodoStatus::Completed => "\u{25cf} ",
            };
            sb.push_str(icon);
            sb.push_str(&item.content);
            sb.push_str(" [");
            sb.push_str(match item.status {
                crate::context::TodoStatus::Pending => "pending",
                crate::context::TodoStatus::InProgress => "in_progress",
                crate::context::TodoStatus::Completed => "completed",
            });
            sb.push_str("]\n");
        }

        ToolResult::ok(sb)
    }
}
