use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};

/// TaskOutput represents the output of a background task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutput {
    pub task_id: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub total_bytes: usize,
}

/// TaskOutputTool retrieves output from background tasks.
pub struct TaskOutputTool {
    outputs: Arc<Mutex<HashMap<String, TaskOutput>>>,
}

impl TaskOutputTool {
    pub fn new() -> Self {
        Self {
            outputs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn store_output(&self, task_id: &str, output: &str, exit_code: Option<i32>, truncated: bool) {
        let total_bytes = output.len();
        let task_output = TaskOutput {
            task_id: task_id.to_string(),
            output: output.to_string(),
            exit_code,
            truncated,
            total_bytes,
        };
        self.outputs.lock().unwrap().insert(task_id.to_string(), task_output);
    }

    pub fn get_output(&self, task_id: &str) -> Option<TaskOutput> {
        self.outputs.lock().unwrap().get(task_id).cloned()
    }

    pub fn remove_output(&self, task_id: &str) -> Option<TaskOutput> {
        self.outputs.lock().unwrap().remove(task_id)
    }

    pub fn list_tasks(&self) -> Vec<String> {
        self.outputs.lock().unwrap().keys().cloned().collect()
    }

    pub fn has_task(&self, task_id: &str) -> bool {
        self.outputs.lock().unwrap().contains_key(task_id)
    }
}

impl Default for TaskOutputTool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_get_output() {
        let tool = TaskOutputTool::new();
        tool.store_output("task-1", "hello world", Some(0), false);
        
        let output = tool.get_output("task-1").unwrap();
        assert_eq!(output.task_id, "task-1");
        assert_eq!(output.output, "hello world");
        assert_eq!(output.exit_code, Some(0));
        assert!(!output.truncated);
    }

    #[test]
    fn test_missing_output() {
        let tool = TaskOutputTool::new();
        assert!(tool.get_output("nonexistent").is_none());
    }

    #[test]
    fn test_remove_output() {
        let tool = TaskOutputTool::new();
        tool.store_output("task-1", "output", Some(0), false);
        let removed = tool.remove_output("task-1").unwrap();
        assert_eq!(removed.output, "output");
        assert!(!tool.has_task("task-1"));
    }

    #[test]
    fn test_list_tasks() {
        let tool = TaskOutputTool::new();
        tool.store_output("task-1", "a", Some(0), false);
        tool.store_output("task-2", "b", Some(1), false);
        let mut tasks = tool.list_tasks();
        tasks.sort();
        assert_eq!(tasks, vec!["task-1", "task-2"]);
    }

    #[test]
    fn test_truncated_output() {
        let tool = TaskOutputTool::new();
        let long_output = "x".repeat(10000);
        tool.store_output("big-task", &long_output, None, true);
        let output = tool.get_output("big-task").unwrap();
        assert!(output.truncated);
        assert_eq!(output.total_bytes, 10000);
    }
}
