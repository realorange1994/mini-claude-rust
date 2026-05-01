//! WorkTaskStore — manages work tasks (LLM TODO items) for an agent session.
//!
//! This is separate from TaskStore which manages async sub-agent tasks.
//! Tasks have dependency tracking with bidirectional blocks/blocked_by edges,
//! cycle detection via BFS, and validation of task IDs.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Work task status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkTaskStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

impl WorkTaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkTaskStatus::Pending => "pending",
            WorkTaskStatus::InProgress => "in_progress",
            WorkTaskStatus::Completed => "completed",
            WorkTaskStatus::Deleted => "deleted",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(WorkTaskStatus::Pending),
            "in_progress" => Some(WorkTaskStatus::InProgress),
            "completed" => Some(WorkTaskStatus::Completed),
            "deleted" => Some(WorkTaskStatus::Deleted),
            _ => None,
        }
    }
}

/// A single work item tracked by the LLM.
#[derive(Debug, Clone)]
pub struct WorkTask {
    pub id: String,
    pub subject: String,
    pub description: String,
    pub active_form: String,
    pub status: WorkTaskStatus,
    pub owner: String,
    pub metadata: HashMap<String, serde_json::Value>,
    pub blocks: Vec<String>,
    pub blocked_by: Vec<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

/// WorkTaskInfo is a display-friendly representation of a work task.
#[derive(Debug, Clone)]
pub struct WorkTaskInfo {
    pub id: String,
    pub subject: String,
    pub description: String,
    pub active_form: String,
    pub status: String,
    pub owner: String,
    pub metadata: HashMap<String, serde_json::Value>,
    pub blocks: Vec<String>,
    pub blocked_by: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Manages work tasks with thread-safe access.
pub struct WorkTaskStore {
    tasks: Mutex<HashMap<String, WorkTask>>,
    next_id: AtomicU32,
}

pub type SharedWorkTaskStore = Arc<WorkTaskStore>;

impl WorkTaskStore {
    /// Creates a new empty work task store.
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(0),
        }
    }

    /// Create a new shared instance.
    pub fn new_shared() -> SharedWorkTaskStore {
        Arc::new(Self::new())
    }

    /// Creates a new work task with status "pending" and returns its ID.
    pub fn create_task(
        &self,
        subject: &str,
        description: &str,
        active_form: &str,
        metadata: Option<HashMap<String, serde_json::Value>>,
    ) -> String {
        let id_num = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let task_id = id_num.to_string();
        let now = SystemTime::now();

        let task = WorkTask {
            id: task_id.clone(),
            subject: subject.to_string(),
            description: description.to_string(),
            active_form: active_form.to_string(),
            status: WorkTaskStatus::Pending,
            owner: String::new(),
            metadata: metadata.unwrap_or_default(),
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            created_at: now,
            updated_at: now,
        };

        let mut tasks = self.tasks.lock().unwrap();
        tasks.insert(task_id.clone(), task);
        task_id
    }

    /// Returns a work task by ID, or None if not found.
    pub fn get_task(&self, id: &str) -> Option<WorkTask> {
        let tasks = self.tasks.lock().unwrap();
        tasks.get(id).cloned()
    }

    /// Returns a work task as display-friendly WorkTaskInfo, or None if not found.
    pub fn get_task_info(&self, id: &str) -> Option<WorkTaskInfo> {
        let task = self.get_task(id)?;
        Some(self.task_to_info(&task))
    }

    /// Returns all non-deleted tasks, sorted by ID (creation order).
    pub fn list_tasks(&self) -> Vec<WorkTaskInfo> {
        let tasks = self.tasks.lock().unwrap();
        let mut list: Vec<&WorkTask> = tasks
            .values()
            .filter(|t| t.status != WorkTaskStatus::Deleted)
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list.iter().map(|t| self.task_to_info(t)).collect()
    }

    /// Updates a work task with the given fields.
    /// Supported update keys: status, subject, description, activeForm, owner,
    /// metadata, addBlocks, addBlockedBy.
    pub fn update_task(&self, id: &str, updates: &HashMap<String, serde_json::Value>) -> Result<(), String> {
        let mut tasks = self.tasks.lock().unwrap();

        if !tasks.contains_key(id) {
            return Err(format!("task {} not found", id));
        }

        // Collect dependency additions first (before any mutation)
        let mut add_blocks: Vec<String> = Vec::new();
        let mut add_blocked_by: Vec<String> = Vec::new();

        if let Some(val) = updates.get("addBlocks") {
            if let Some(arr) = val.as_array() {
                for item in arr {
                    let dep_id = normalize_dep_id(item);
                    if dep_id.is_empty() {
                        continue;
                    }
                    add_blocks.push(dep_id);
                }
            }
        }

        if let Some(val) = updates.get("addBlockedBy") {
            if let Some(arr) = val.as_array() {
                for item in arr {
                    let dep_id = normalize_dep_id(item);
                    if dep_id.is_empty() {
                        continue;
                    }
                    // Cycle check before adding
                    if Self::would_create_cycle(&tasks, id, &dep_id) {
                        continue;
                    }
                    add_blocked_by.push(dep_id);
                }
            }
        }

        // Apply scalar updates to the target task
        if let Some(task) = tasks.get_mut(id) {
            if let Some(val) = updates.get("status") {
                if let Some(status_str) = val.as_str() {
                    let new_status = WorkTaskStatus::from_str(status_str)
                        .ok_or_else(|| format!("invalid status: {}", status_str))?;
                    task.status = new_status;
                }
            }
            if let Some(val) = updates.get("subject") {
                if let Some(v) = val.as_str() {
                    task.subject = v.to_string();
                }
            }
            if let Some(val) = updates.get("description") {
                if let Some(v) = val.as_str() {
                    task.description = v.to_string();
                }
            }
            if let Some(val) = updates.get("activeForm") {
                if let Some(v) = val.as_str() {
                    task.active_form = v.to_string();
                }
            }
            if let Some(val) = updates.get("owner") {
                if let Some(v) = val.as_str() {
                    task.owner = v.to_string();
                }
            }
            if let Some(val) = updates.get("metadata") {
                if let Some(m) = val.as_object() {
                    for (k, v) in m {
                        if v.is_null() {
                            task.metadata.remove(k);
                        } else {
                            task.metadata.insert(k.clone(), v.clone());
                        }
                    }
                }
            }

            // Add new blocks (before filtering)
            for dep_id in &add_blocks {
                if !task.blocks.contains(dep_id) {
                    task.blocks.push(dep_id.clone());
                }
            }
            // Add new blocked_by (before filtering)
            for dep_id in &add_blocked_by {
                if !task.blocked_by.contains(dep_id) {
                    task.blocked_by.push(dep_id.clone());
                }
            }

            task.updated_at = SystemTime::now();
        }

        // Bidirectional update for addBlocks
        // Collect valid dep IDs first (need immutable borrow)
        let valid_block_ids: Vec<String> = add_blocks.iter()
            .filter(|dep_id| tasks.contains_key(*dep_id))
            .cloned()
            .collect();

        // Filter target task's blocks to only valid IDs
        let invalid_block_ids: Vec<String> = {
            if let Some(task) = tasks.get(id) {
                task.blocks.iter()
                    .filter(|bid| !tasks.contains_key(*bid))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            }
        };
        if let Some(task) = tasks.get_mut(id) {
            for inv in &invalid_block_ids {
                task.blocks.retain(|bid| bid != inv);
            }
        }

        // Update blocked task's blocked_by for each valid dep
        for dep_id in &valid_block_ids {
            if let Some(blocked) = tasks.get_mut(dep_id) {
                if !blocked.blocked_by.contains(&id.to_string()) {
                    blocked.blocked_by.push(id.to_string());
                }
            }
        }

        // Bidirectional update for addBlockedBy
        let valid_blocked_by_ids: Vec<String> = add_blocked_by.iter()
            .filter(|dep_id| tasks.contains_key(*dep_id))
            .cloned()
            .collect();

        // Filter target task's blocked_by to only valid IDs
        let invalid_blocked_by_ids: Vec<String> = {
            if let Some(task) = tasks.get(id) {
                task.blocked_by.iter()
                    .filter(|bid| !tasks.contains_key(*bid))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            }
        };
        if let Some(task) = tasks.get_mut(id) {
            for inv in &invalid_blocked_by_ids {
                task.blocked_by.retain(|bid| bid != inv);
            }
        }

        // Update blocking task's blocks for each valid dep
        for dep_id in &valid_blocked_by_ids {
            if let Some(blocker) = tasks.get_mut(dep_id) {
                if !blocker.blocks.contains(&id.to_string()) {
                    blocker.blocks.push(id.to_string());
                }
            }
        }

        // If task is deleted, remove references from other tasks
        let is_deleted = tasks.get(id).map(|t| t.status == WorkTaskStatus::Deleted).unwrap_or(false);
        if is_deleted {
            let task_id = id.to_string();
            for (_, other) in tasks.iter_mut() {
                other.blocks.retain(|b| b != &task_id);
                other.blocked_by.retain(|b| b != &task_id);
            }
        }

        Ok(())
    }

    /// Checks if adding blocker_id as a dependency of task_id would create a cycle.
    /// Searches BOTH blocked_by and blocks edges from blocker_id. If we reach task_id,
    /// the edge creates a cycle.
    fn would_create_cycle(
        tasks: &HashMap<String, WorkTask>,
        task_id: &str,
        blocker_id: &str,
    ) -> bool {
        if task_id == blocker_id {
            return true;
        }
        let mut visited = std::collections::HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(blocker_id.to_string());

        while let Some(current) = queue.pop_front() {
            if current == task_id {
                return true;
            }
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(t) = tasks.get(&current) {
                for b in &t.blocks {
                    queue.push_back(b.clone());
                }
                for b in &t.blocked_by {
                    queue.push_back(b.clone());
                }
            }
        }
        false
    }

    fn task_to_info(&self, task: &WorkTask) -> WorkTaskInfo {
        let created = format_system_time(task.created_at);
        let updated = format_system_time(task.updated_at);
        WorkTaskInfo {
            id: task.id.clone(),
            subject: task.subject.clone(),
            description: task.description.clone(),
            active_form: task.active_form.clone(),
            status: task.status.as_str().to_string(),
            owner: task.owner.clone(),
            metadata: task.metadata.clone(),
            blocks: task.blocks.clone(),
            blocked_by: task.blocked_by.clone(),
            created_at: created,
            updated_at: updated,
        }
    }
}

/// Normalize a dependency ID from a JSON value.
/// Converts integers (f64) to strings, strips '#' prefix.
fn normalize_dep_id(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                (f as i64).to_string()
            } else {
                String::new()
            }
        }
        serde_json::Value::String(s) => {
            s.trim_start_matches('#').to_string()
        }
        _ => String::new(),
    }
}

/// Format a SystemTime as a human-readable string.
fn format_system_time(t: SystemTime) -> String {
    // Use simple formatting since we have chrono available
    let datetime: chrono::DateTime<chrono::Local> = t.into();
    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_metadata(pairs: &[(&str, &str)]) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
        m
    }

    #[test]
    fn test_create_task() {
        let store = WorkTaskStore::new();
        let id = store.create_task("Fix bug", "Fix the authentication bug", "Fixing bug", None);
        assert!(!id.is_empty());
        assert_eq!(id, "1");

        let task = store.get_task(&id).expect("task not found");
        assert_eq!(task.subject, "Fix bug");
        assert_eq!(task.description, "Fix the authentication bug");
        assert_eq!(task.status, WorkTaskStatus::Pending);
        assert_eq!(task.active_form, "Fixing bug");
    }

    #[test]
    fn test_create_task_incrementing_ids() {
        let store = WorkTaskStore::new();
        let id1 = store.create_task("Task 1", "Desc 1", "", None);
        let id2 = store.create_task("Task 2", "Desc 2", "", None);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_create_task_with_metadata() {
        let store = WorkTaskStore::new();
        let metadata = make_metadata(&[("priority", "high")]);
        let id = store.create_task("Fix bug", "Fix it", "", Some(metadata));
        let task = store.get_task(&id).unwrap();
        assert_eq!(task.metadata["priority"], "high");
    }

    #[test]
    fn test_list_tasks() {
        let store = WorkTaskStore::new();
        store.create_task("Task 1", "Desc 1", "", None);
        store.create_task("Task 2", "Desc 2", "", None);
        store.create_task("Task 3", "Desc 3", "", None);

        let tasks = store.list_tasks();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[1].id, "2");
        assert_eq!(tasks[2].id, "3");
    }

    #[test]
    fn test_list_tasks_excludes_deleted() {
        let store = WorkTaskStore::new();
        store.create_task("Task 1", "Desc 1", "", None);
        let id2 = store.create_task("Task 2", "Desc 2", "", None);
        store.create_task("Task 3", "Desc 3", "", None);

        let mut updates = HashMap::new();
        updates.insert("status".to_string(), serde_json::json!("deleted"));
        store.update_task(&id2, &updates).unwrap();

        let tasks = store.list_tasks();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn test_update_task_status() {
        let store = WorkTaskStore::new();
        let id = store.create_task("Fix bug", "Fix it", "", None);

        let mut updates = HashMap::new();
        updates.insert("status".to_string(), serde_json::json!("in_progress"));
        store.update_task(&id, &updates).unwrap();

        let task = store.get_task(&id).unwrap();
        assert_eq!(task.status, WorkTaskStatus::InProgress);

        updates.insert("status".to_string(), serde_json::json!("completed"));
        store.update_task(&id, &updates).unwrap();

        let task = store.get_task(&id).unwrap();
        assert_eq!(task.status, WorkTaskStatus::Completed);
    }

    #[test]
    fn test_update_task_invalid_status() {
        let store = WorkTaskStore::new();
        let id = store.create_task("Fix bug", "Fix it", "", None);

        let mut updates = HashMap::new();
        updates.insert("status".to_string(), serde_json::json!("invalid"));
        let result = store.update_task(&id, &updates);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_task_subject_and_description() {
        let store = WorkTaskStore::new();
        let id = store.create_task("Old Subject", "Old Description", "", None);

        let mut updates = HashMap::new();
        updates.insert("subject".to_string(), serde_json::json!("New Subject"));
        updates.insert("description".to_string(), serde_json::json!("New Description"));
        store.update_task(&id, &updates).unwrap();

        let task = store.get_task(&id).unwrap();
        assert_eq!(task.subject, "New Subject");
        assert_eq!(task.description, "New Description");
    }

    #[test]
    fn test_update_task_owner() {
        let store = WorkTaskStore::new();
        let id = store.create_task("Fix bug", "Fix it", "", None);

        let mut updates = HashMap::new();
        updates.insert("owner".to_string(), serde_json::json!("agent-1"));
        store.update_task(&id, &updates).unwrap();

        let task = store.get_task(&id).unwrap();
        assert_eq!(task.owner, "agent-1");
    }

    #[test]
    fn test_update_task_blocks() {
        let store = WorkTaskStore::new();
        let id1 = store.create_task("Task 1", "Desc 1", "", None);
        let id2 = store.create_task("Task 2", "Desc 2", "", None);

        let mut updates = HashMap::new();
        updates.insert("addBlocks".to_string(), serde_json::json!([id2]));
        store.update_task(&id1, &updates).unwrap();

        let task1 = store.get_task(&id1).unwrap();
        let task2 = store.get_task(&id2).unwrap();

        assert_eq!(task1.blocks.len(), 1);
        assert_eq!(task1.blocks[0], id2);
        assert_eq!(task2.blocked_by.len(), 1);
        assert_eq!(task2.blocked_by[0], id1);
    }

    #[test]
    fn test_update_task_duplicate_blocks() {
        let store = WorkTaskStore::new();
        let id1 = store.create_task("Task 1", "Desc 1", "", None);
        let id2 = store.create_task("Task 2", "Desc 2", "", None);

        let mut updates = HashMap::new();
        updates.insert("addBlocks".to_string(), serde_json::json!([&id2]));
        store.update_task(&id1, &updates).unwrap();
        store.update_task(&id1, &updates).unwrap();

        let task1 = store.get_task(&id1).unwrap();
        assert_eq!(task1.blocks.len(), 1);
    }

    #[test]
    fn test_update_task_deleted_cleans_references() {
        let store = WorkTaskStore::new();
        let id1 = store.create_task("Task 1", "Desc 1", "", None);
        let id2 = store.create_task("Task 2", "Desc 2", "", None);

        let mut updates = HashMap::new();
        updates.insert("addBlocks".to_string(), serde_json::json!([&id2]));
        store.update_task(&id1, &updates).unwrap();

        // Delete task 1
        let mut updates = HashMap::new();
        updates.insert("status".to_string(), serde_json::json!("deleted"));
        store.update_task(&id1, &updates).unwrap();

        let task2 = store.get_task(&id2).unwrap();
        assert!(task2.blocked_by.is_empty());
    }

    #[test]
    fn test_update_task_not_found() {
        let store = WorkTaskStore::new();
        let mut updates = HashMap::new();
        updates.insert("subject".to_string(), serde_json::json!("New"));
        let result = store.update_task("999", &updates);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_task_metadata() {
        let store = WorkTaskStore::new();
        let mut meta = HashMap::new();
        meta.insert("priority".to_string(), serde_json::json!("high"));
        let id = store.create_task("Fix bug", "Fix it", "", Some(meta));

        // Add metadata key
        let mut updates = HashMap::new();
        updates.insert("metadata".to_string(), serde_json::json!({"assignee": "john"}));
        store.update_task(&id, &updates).unwrap();

        let task = store.get_task(&id).unwrap();
        assert_eq!(task.metadata["priority"], "high");
        assert_eq!(task.metadata["assignee"], "john");

        // Delete metadata key
        let mut updates = HashMap::new();
        updates.insert("metadata".to_string(), serde_json::json!({"priority": null}));
        store.update_task(&id, &updates).unwrap();

        let task = store.get_task(&id).unwrap();
        assert!(!task.metadata.contains_key("priority"));
    }

    #[test]
    fn test_update_task_multiple_blocks_and_blocked_by() {
        let store = WorkTaskStore::new();
        let id1 = store.create_task("Task 1", "Desc 1", "", None);
        let id2 = store.create_task("Task 2", "Desc 2", "", None);
        let id3 = store.create_task("Task 3", "Desc 3", "", None);

        let mut updates = HashMap::new();
        updates.insert("addBlocks".to_string(), serde_json::json!([&id2, &id3]));
        store.update_task(&id1, &updates).unwrap();

        let task1 = store.get_task(&id1).unwrap();
        assert_eq!(task1.blocks.len(), 2);

        let task2 = store.get_task(&id2).unwrap();
        assert_eq!(task2.blocked_by.len(), 1);
        assert_eq!(task2.blocked_by[0], id1);

        let task3 = store.get_task(&id3).unwrap();
        assert_eq!(task3.blocked_by.len(), 1);
        assert_eq!(task3.blocked_by[0], id1);
    }

    #[test]
    fn test_update_task_integer_elements_in_array() {
        let store = WorkTaskStore::new();
        store.create_task("Task 1", "Desc 1", "", None);
        store.create_task("Task 2", "Desc 2", "", None);

        // LLM sends [1] (number elements) instead of ["1"]
        let mut updates = HashMap::new();
        updates.insert("addBlockedBy".to_string(), serde_json::json!([1]));
        store.update_task("2", &updates).unwrap();

        let task2 = store.get_task("2").unwrap();
        assert_eq!(task2.blocked_by.len(), 1);
        assert_eq!(task2.blocked_by[0], "1");
    }

    #[test]
    fn test_update_task_non_existent_dependency() {
        let store = WorkTaskStore::new();
        store.create_task("Task 1", "Desc 1", "", None);

        let mut updates = HashMap::new();
        updates.insert("addBlockedBy".to_string(), serde_json::json!(["9999"]));
        store.update_task("1", &updates).unwrap();

        let task1 = store.get_task("1").unwrap();
        assert!(task1.blocked_by.is_empty());
    }

    #[test]
    fn test_update_task_non_existent_blocks() {
        let store = WorkTaskStore::new();
        store.create_task("Task 1", "Desc 1", "", None);

        let mut updates = HashMap::new();
        updates.insert("addBlocks".to_string(), serde_json::json!(["8888"]));
        store.update_task("1", &updates).unwrap();

        let task1 = store.get_task("1").unwrap();
        assert!(task1.blocks.is_empty());
    }

    #[test]
    fn test_update_task_circular_dependency() {
        let store = WorkTaskStore::new();
        let id1 = store.create_task("Task 1", "Desc 1", "", None);
        let id2 = store.create_task("Task 2", "Desc 2", "", None);

        // Task 2 is blocked by Task 1
        let mut updates = HashMap::new();
        updates.insert("addBlockedBy".to_string(), serde_json::json!([&id1]));
        store.update_task(&id2, &updates).unwrap();

        // Now try to make Task 1 blocked by Task 2 — would create cycle
        let mut updates = HashMap::new();
        updates.insert("addBlockedBy".to_string(), serde_json::json!([&id2]));
        store.update_task(&id1, &updates).unwrap();

        let task1 = store.get_task(&id1).unwrap();
        assert!(!task1.blocked_by.contains(&id2));
    }

    #[test]
    fn test_update_task_self_dependency() {
        let store = WorkTaskStore::new();
        let id1 = store.create_task("Task 1", "Desc 1", "", None);

        let mut updates = HashMap::new();
        updates.insert("addBlockedBy".to_string(), serde_json::json!([&id1]));
        store.update_task(&id1, &updates).unwrap();

        let task1 = store.get_task(&id1).unwrap();
        assert!(!task1.blocked_by.contains(&id1));
    }

    #[test]
    fn test_update_task_hash_prefix() {
        let store = WorkTaskStore::new();
        store.create_task("Task 1", "Desc 1", "", None);
        store.create_task("Task 2", "Desc 2", "", None);

        // "#1" should be normalized to "1"
        let mut updates = HashMap::new();
        updates.insert("addBlockedBy".to_string(), serde_json::json!(["#1"]));
        store.update_task("2", &updates).unwrap();

        let task2 = store.get_task("2").unwrap();
        assert_eq!(task2.blocked_by.len(), 1);
        assert_eq!(task2.blocked_by[0], "1");
    }

    #[test]
    fn test_update_task_hash_prefix_blocks() {
        let store = WorkTaskStore::new();
        store.create_task("Task 1", "Desc 1", "", None);
        store.create_task("Task 2", "Desc 2", "", None);

        // "#2" should be normalized to "2"
        let mut updates = HashMap::new();
        updates.insert("addBlocks".to_string(), serde_json::json!(["#2"]));
        store.update_task("1", &updates).unwrap();

        let task1 = store.get_task("1").unwrap();
        assert_eq!(task1.blocks.len(), 1);
        assert_eq!(task1.blocks[0], "2");
    }
}
