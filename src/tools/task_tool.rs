//! Task tools — task_create, task_list, task_get, task_update, task_stop.
//!
//! These tools use callback functions to stay decoupled from the WorkTaskStore,
//! allowing it to be owned by the agent loop while tools are registered in the registry.

use crate::tools::{Tool, ToolResult};
use crate::work_task::WorkTaskInfo;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

// Type aliases matching the Go callback pattern
type WorkTaskCreateFunc =
    Arc<dyn Fn(String, String, String, Option<HashMap<String, Value>>) -> String + Send + Sync>;
type WorkTaskListFunc = Arc<dyn Fn() -> Vec<WorkTaskInfo> + Send + Sync>;
type WorkTaskGetFunc = Arc<dyn Fn(String) -> Option<WorkTaskInfo> + Send + Sync>;
type WorkTaskUpdateFunc =
    Arc<dyn Fn(String, HashMap<String, Value>) -> Result<(), String> + Send + Sync>;
type WorkTaskStopFunc = Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>;

// ─── TaskCreateTool ─────────────────────────────────────────────────────────

pub struct TaskCreateTool {
    create_func: WorkTaskCreateFunc,
}

impl TaskCreateTool {
    pub fn new(create_func: WorkTaskCreateFunc) -> Self {
        Self { create_func }
    }
}

impl Clone for TaskCreateTool {
    fn clone(&self) -> Self {
        Self {
            create_func: Arc::clone(&self.create_func),
        }
    }
}

impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "task_create"
    }

    fn description(&self) -> &str {
        "Create a structured task to track work items. Use for complex multi-step tasks to organize progress."
    }

    fn input_schema(&self) -> Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["subject", "description"],
            "properties": {
                "subject": {
                    "type": "string",
                    "description": "Brief title for the task (imperative form, e.g., 'Fix authentication bug')"
                },
                "description": {
                    "type": "string",
                    "description": "Detailed description of what needs to be done"
                },
                "active_form": {
                    "type": "string",
                    "description": "Present continuous form shown in spinner (e.g., 'Fixing authentication bug')"
                },
                "metadata": {
                    "type": "object",
                    "description": "Optional arbitrary metadata to attach to the task"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let subject = params
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if subject.is_empty() {
            return ToolResult::error("subject is required");
        }
        if description.is_empty() {
            return ToolResult::error("description is required");
        }

        let active_form = params
            .get("active_form")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let metadata = params.get("metadata").and_then(|v| {
            v.as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, val)| (k.clone(), val.clone()))
                        .collect::<HashMap<_, _>>()
                })
        });

        let task_id = (self.create_func)(
            subject.to_string(),
            description.to_string(),
            active_form,
            metadata,
        );
        ToolResult::ok(format!("Task #{} created successfully: {}", task_id, subject))
    }
}

// ─── TaskListTool ───────────────────────────────────────────────────────────

pub struct TaskListTool {
    list_func: WorkTaskListFunc,
}

impl TaskListTool {
    pub fn new(list_func: WorkTaskListFunc) -> Self {
        Self { list_func }
    }
}

impl Clone for TaskListTool {
    fn clone(&self) -> Self {
        Self {
            list_func: Arc::clone(&self.list_func),
        }
    }
}

impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }

    fn description(&self) -> &str {
        "List all tasks. Returns a table of all tasks with their ID, subject, status, and dependencies."
    }

    fn input_schema(&self) -> Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let _ = params;
        let tasks = (self.list_func)();

        if tasks.is_empty() {
            return ToolResult::ok("No tasks found.".to_string());
        }

        let mut output = String::new();
        output.push_str(&format!(
            "{:<6} {:<40} {:<12} {:<10} {}\n",
            "ID", "Subject", "Status", "Owner", "Blocked By"
        ));
        output.push_str(&"-".repeat(80));
        output.push('\n');

        for task in &tasks {
            let subject = if task.subject.len() > 38 {
                format!("{}...", &task.subject[..35])
            } else {
                task.subject.clone()
            };
            let blocked_by = if task.blocked_by.is_empty() {
                "-".to_string()
            } else {
                task.blocked_by.join(", ")
            };
            let owner = if task.owner.is_empty() {
                "-".to_string()
            } else {
                task.owner.clone()
            };
            output.push_str(&format!(
                "#{:<5} {:<40} {:<12} {:<10} {}\n",
                task.id, subject, task.status, owner, blocked_by
            ));
        }

        output.push_str(&format!("\n{} task(s) total", tasks.len()));
        ToolResult::ok(output)
    }
}

// ─── TaskGetTool ───────────────────────────────────────────────────────────

pub struct TaskGetTool {
    get_func: WorkTaskGetFunc,
}

impl TaskGetTool {
    pub fn new(get_func: WorkTaskGetFunc) -> Self {
        Self { get_func }
    }
}

impl Clone for TaskGetTool {
    fn clone(&self) -> Self {
        Self {
            get_func: Arc::clone(&self.get_func),
        }
    }
}

impl Tool for TaskGetTool {
    fn name(&self) -> &str {
        "task_get"
    }

    fn description(&self) -> &str {
        "Get details of a specific task by ID. Returns full task information including description and dependencies."
    }

    fn input_schema(&self) -> Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the task to retrieve"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if task_id.is_empty() {
            return ToolResult::error("task_id is required");
        }

        let task = (self.get_func)(task_id.to_string());

        match task {
            None => ToolResult::error(format!("Task #{} not found", task_id)),
            Some(t) => {
                let mut output = String::new();
                output.push_str(&format!("Task #{}\n", t.id));
                output.push_str(&format!("  Subject:     {}\n", t.subject));
                output.push_str(&format!("  Status:      {}\n", t.status));
                output.push_str(&format!("  Description: {}\n", t.description));

                if !t.active_form.is_empty() {
                    output.push_str(&format!("  Active Form: {}\n", t.active_form));
                }
                if !t.owner.is_empty() {
                    output.push_str(&format!("  Owner:       {}\n", t.owner));
                }
                if !t.blocks.is_empty() {
                    output.push_str(&format!("  Blocks:      {}\n", t.blocks.join(", ")));
                }
                if !t.blocked_by.is_empty() {
                    output.push_str(&format!("  Blocked By:  {}\n", t.blocked_by.join(", ")));
                }
                if !t.metadata.is_empty() {
                    output.push_str("  Metadata:\n");
                    let mut keys: Vec<_> = t.metadata.keys().collect();
                    keys.sort();
                    for k in keys {
                        output.push_str(&format!("    {}: {}\n", k, t.metadata[k]));
                    }
                }
                output.push_str(&format!("  Created:     {}\n", t.created_at));
                output.push_str(&format!("  Updated:     {}\n", t.updated_at));
                ToolResult::ok(output)
            }
        }
    }
}

// ─── TaskUpdateTool ────────────────────────────────────────────────────────

pub struct TaskUpdateTool {
    update_func: WorkTaskUpdateFunc,
}

impl TaskUpdateTool {
    pub fn new(update_func: WorkTaskUpdateFunc) -> Self {
        Self { update_func }
    }
}

impl Clone for TaskUpdateTool {
    fn clone(&self) -> Self {
        Self {
            update_func: Arc::clone(&self.update_func),
        }
    }
}

impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "task_update"
    }

    fn description(&self) -> &str {
        "Update a task's fields. Use to change status, assign owners, mark dependencies, or edit descriptions."
    }

    fn input_schema(&self) -> Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the task to update"
                },
                "subject": {
                    "type": "string",
                    "description": "New subject for the task"
                },
                "description": {
                    "type": "string",
                    "description": "New description for the task"
                },
                "active_form": {
                    "type": "string",
                    "description": "Present continuous form shown in spinner (e.g., 'Running tests')"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "deleted"],
                    "description": "New status for the task"
                },
                "owner": {
                    "type": "string",
                    "description": "New owner for the task"
                },
                "add_blocks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs that this task blocks"
                },
                "add_blocked_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs that block this task"
                },
                "metadata": {
                    "type": "object",
                    "description": "Metadata keys to merge into the task. Set a key to null to delete it."
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if task_id.is_empty() {
            return ToolResult::error("task_id is required");
        }

        let mut updates: HashMap<String, Value> = HashMap::new();
        let mut updated_fields: Vec<&str> = Vec::new();

        // Scalar fields
        if let Some(v) = params.get("subject").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                updates.insert("subject".to_string(), Value::String(v.to_string()));
                updated_fields.push("subject");
            }
        }
        if let Some(v) = params.get("description").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                updates.insert("description".to_string(), Value::String(v.to_string()));
                updated_fields.push("description");
            }
        }
        if let Some(v) = params.get("active_form").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                updates.insert("activeForm".to_string(), Value::String(v.to_string()));
                updated_fields.push("activeForm");
            }
        }
        if let Some(v) = params.get("status").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                updates.insert("status".to_string(), Value::String(v.to_string()));
                updated_fields.push("status");
            }
        }
        if let Some(v) = params.get("owner").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                updates.insert("owner".to_string(), Value::String(v.to_string()));
                updated_fields.push("owner");
            }
        }

        // Coerce scalar to array for add_blocked_by (integer/scalar from LLM)
        if let Some(v) = params.get("add_blocked_by") {
            let coerced = coerce_scalar_to_array(v);
            if coerced.as_array().map_or(false, |a| !a.is_empty()) {
                updates.insert("addBlockedBy".to_string(), coerced);
                updated_fields.push("blockedBy");
            }
        }

        // Coerce scalar to array for add_blocks
        if let Some(v) = params.get("add_blocks") {
            let coerced = coerce_scalar_to_array(v);
            if coerced.as_array().map_or(false, |a| !a.is_empty()) {
                updates.insert("addBlocks".to_string(), coerced);
                updated_fields.push("blocks");
            }
        }

        if let Some(v) = params.get("metadata").and_then(|v| v.as_object()) {
            updates.insert("metadata".to_string(), Value::Object(v.clone()));
            updated_fields.push("metadata");
        }

        if updates.is_empty() {
            return ToolResult::error("no update fields provided");
        }

        match (self.update_func)(task_id.to_string(), updates) {
            Ok(()) => ToolResult::ok(format!(
                "Updated task #{}: {}",
                task_id,
                updated_fields.join(", ")
            )),
            Err(e) => ToolResult::error(format!("Failed to update task: {}", e)),
        }
    }
}

/// Coerce a scalar value (number or string) to an array, or pass through if already array.
fn coerce_scalar_to_array(val: &Value) -> Value {
    match val {
        Value::Array(arr) => {
            // Normalize each element: strip '#' prefix from strings, convert numbers to strings
            let normalized: Vec<Value> = arr
                .iter()
                .map(|v| match v {
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            Value::String(i.to_string())
                        } else if let Some(f) = n.as_f64() {
                            Value::String((f as i64).to_string())
                        } else {
                            Value::String(n.to_string())
                        }
                    }
                    Value::String(s) => Value::String(s.trim_start_matches('#').to_string()),
                    _ => v.clone(),
                })
                .collect();
            Value::Array(normalized)
        }
        Value::Number(n) => {
            let s = if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                (f as i64).to_string()
            } else {
                return Value::Array(Vec::new());
            };
            Value::Array(vec![Value::String(s)])
        }
        Value::String(s) => {
            Value::Array(vec![Value::String(s.trim_start_matches('#').to_string())])
        }
        _ => Value::Array(Vec::new()),
    }
}

// ─── TaskStopTool ──────────────────────────────────────────────────────────

pub struct TaskStopTool {
    stop_func: WorkTaskStopFunc,
}

impl TaskStopTool {
    pub fn new(stop_func: WorkTaskStopFunc) -> Self {
        Self { stop_func }
    }
}

impl Clone for TaskStopTool {
    fn clone(&self) -> Self {
        Self {
            stop_func: Arc::clone(&self.stop_func),
        }
    }
}

impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "task_stop"
    }

    fn description(&self) -> &str {
        "Stop a running background task by its ID. Use this to terminate long-running or stuck processes."
    }

    fn input_schema(&self) -> Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task to stop"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if task_id.is_empty() {
            return ToolResult::error("task_id is required");
        }

        match (self.stop_func)(task_id.to_string()) {
            Ok(()) => ToolResult::ok(format!("Task {} stopped successfully", task_id)),
            Err(e) => ToolResult::error(e),
        }
    }
}
