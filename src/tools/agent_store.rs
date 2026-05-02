//! AgentTaskStore -- Thread-safe task store tracking background sub-agent tasks.
//!
//! Mirrors the Go implementation's AgentTaskStore. Tracks task lifecycle
//! (pending -> running -> completed/failed/killed) with output buffering
//! and cancellation support via CancellationToken.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

// ─── TaskStatus ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentTaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

impl AgentTaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentTaskStatus::Pending => "pending",
            AgentTaskStatus::Running => "running",
            AgentTaskStatus::Completed => "completed",
            AgentTaskStatus::Failed => "failed",
            AgentTaskStatus::Killed => "killed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AgentTaskStatus::Completed | AgentTaskStatus::Failed | AgentTaskStatus::Killed
        )
    }
}

impl std::fmt::Display for AgentTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl AgentTaskStatus {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(AgentTaskStatus::Pending),
            "running" => Some(AgentTaskStatus::Running),
            "completed" => Some(AgentTaskStatus::Completed),
            "failed" => Some(AgentTaskStatus::Failed),
            "killed" => Some(AgentTaskStatus::Killed),
            _ => None,
        }
    }
}

// ─── AgentTask ──────────────────────────────────────────────────────────────

/// Maximum output buffer size (50 KB).
const MAX_OUTPUT_SIZE: usize = 50 * 1024;

/// Inner mutable state of an AgentTask, protected by a Mutex.
struct AgentTaskInner {
    status: AgentTaskStatus,
    end_time: Option<Instant>,
    cancel_handle: Option<CancellationToken>,
    transcript_path: String,
    tools_used: u32,
    duration_ms: u64,
    output: String,
}

/// Tracks a single background sub-agent task.
/// Immutable fields are directly accessible; mutable state is protected by Mutex.
pub struct AgentTask {
    // Immutable after creation
    pub id: String,
    pub task_type: String,
    pub description: String,
    pub subagent_type: String,
    pub model: String,
    pub prompt: String,
    pub start_time: Instant,
    pub parent_id: String,
    inner: Mutex<AgentTaskInner>,
}

impl AgentTask {
    /// Append text to the output buffer with a 50 KB cap.
    /// When the cap is exceeded, a truncation marker is inserted.
    pub fn write_output(&self, s: &str) {
        let mut inner = self.inner.lock().unwrap();

        // Fast path: no truncation needed
        if inner.output.len() + s.len() <= MAX_OUTPUT_SIZE {
            inner.output.push_str(s);
            return;
        }

        // Cap exceeded. Keep up to 1/4 of existing content, add truncation marker,
        // then append new content (trimmed if still over cap).
        let quarter = MAX_OUTPUT_SIZE / 4;
        let prefix: String = if inner.output.len() > quarter {
            inner.output.chars().take(quarter).collect()
        } else {
            inner.output.clone()
        };

        let truncated = inner.output.len() - prefix.len();
        let marker = format!("\n... ({} chars truncated) ...\n", truncated);

        let mut new_content = prefix + &marker;
        let remaining = MAX_OUTPUT_SIZE - new_content.len();
        if remaining > 0 && remaining < s.len() {
            new_content.push_str(&s.chars().take(remaining).collect::<String>());
        } else {
            new_content.push_str(s);
        }

        inner.output = new_content;
    }

    /// Return a copy of the output buffer.
    pub fn get_output(&self) -> String {
        self.inner.lock().unwrap().output.clone()
    }

    /// Check if the task is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        self.inner.lock().unwrap().status.is_terminal()
    }

    /// Getters for mutable fields
    pub fn status(&self) -> AgentTaskStatus {
        self.inner.lock().unwrap().status
    }
    pub fn end_time(&self) -> Option<Instant> {
        self.inner.lock().unwrap().end_time
    }
    pub fn cancel_handle(&self) -> Option<CancellationToken> {
        self.inner.lock().unwrap().cancel_handle.clone()
    }
    pub fn transcript_path(&self) -> String {
        self.inner.lock().unwrap().transcript_path.clone()
    }
    pub fn tools_used(&self) -> u32 {
        self.inner.lock().unwrap().tools_used
    }
    pub fn duration_ms(&self) -> u64 {
        self.inner.lock().unwrap().duration_ms
    }

    /// Set status to Completed
    pub fn set_completed(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.status.is_terminal() {
            return;
        }
        inner.status = AgentTaskStatus::Completed;
        inner.end_time = Some(Instant::now());
        inner.duration_ms = self.start_time.elapsed().as_millis() as u64;
    }

    /// Set status to Failed
    pub fn set_failed(&self, err: &str) {
        let mut inner = self.inner.lock().unwrap();
        if inner.status.is_terminal() {
            return;
        }
        inner.status = AgentTaskStatus::Failed;
        inner.end_time = Some(Instant::now());
        inner.duration_ms = self.start_time.elapsed().as_millis() as u64;
        inner.output.push_str(&format!("\n[ERROR] {}\n", err));
    }

    /// Set status to Killed and trigger cancellation
    pub fn set_killed(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.status.is_terminal() {
            return false;
        }
        if let Some(ref cancel) = inner.cancel_handle {
            cancel.cancel();
        }
        inner.status = AgentTaskStatus::Killed;
        inner.end_time = Some(Instant::now());
        inner.duration_ms = self.start_time.elapsed().as_millis() as u64;
        true
    }

    /// Set cancel_handle (called when the task starts running)
    pub fn set_cancel_handle(&self, cancel: CancellationToken) {
        let mut inner = self.inner.lock().unwrap();
        inner.status = AgentTaskStatus::Running;
        inner.cancel_handle = Some(cancel);
    }

    /// Update tools_used and duration_ms
    pub fn set_stats(&self, tools_used: u32, duration_ms: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.tools_used = tools_used;
        inner.duration_ms = duration_ms;
    }

    /// Set transcript_path
    pub fn set_transcript_path(&self, path: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.transcript_path = path.to_string();
    }
}

// ─── AgentTaskStore ─────────────────────────────────────────────────────────

/// Thread-safe store for managing background agent tasks.
pub struct AgentTaskStore {
    tasks: RwLock<HashMap<String, Arc<AgentTask>>>,
}

pub type SharedAgentTaskStore = Arc<AgentTaskStore>;

impl AgentTaskStore {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
        }
    }

    pub fn new_shared() -> SharedAgentTaskStore {
        Arc::new(Self::new())
    }

    /// Generate an 8-character hex ID.
    fn generate_id() -> String {
        use uuid::Uuid;
        let uuid = Uuid::new_v4().to_string();
        uuid.chars().filter(|c| *c != '-').take(8).collect()
    }

    /// Create a new task with an auto-generated ID.
    pub fn create(
        &self,
        description: &str,
        subagent_type: &str,
        prompt: &str,
        model: &str,
    ) -> Arc<AgentTask> {
        let id = Self::generate_id();
        self.create_with_id(&id, description, subagent_type, prompt, model)
    }

    /// Create a new task with a specific ID.
    pub fn create_with_id(
        &self,
        id: &str,
        description: &str,
        subagent_type: &str,
        prompt: &str,
        model: &str,
    ) -> Arc<AgentTask> {
        let task = Arc::new(AgentTask {
            id: id.to_string(),
            task_type: "local_agent".to_string(),
            description: description.to_string(),
            subagent_type: subagent_type.to_string(),
            model: model.to_string(),
            prompt: prompt.to_string(),
            start_time: Instant::now(),
            parent_id: String::new(),
            inner: Mutex::new(AgentTaskInner {
                status: AgentTaskStatus::Pending,
                end_time: None,
                cancel_handle: None,
                transcript_path: String::new(),
                tools_used: 0,
                duration_ms: 0,
                output: String::new(),
            }),
        });

        let mut tasks = self.tasks.write().unwrap();
        tasks.insert(id.to_string(), Arc::clone(&task));
        task
    }

    /// Mark a task as running and store its cancellation token.
    pub fn start(&self, id: &str, cancel: CancellationToken) {
        let tasks = self.tasks.read().unwrap();
        if let Some(task) = tasks.get(id) {
            task.set_cancel_handle(cancel);
        }
    }

    /// Mark a task as completed.
    pub fn complete(&self, id: &str) {
        let tasks = self.tasks.read().unwrap();
        if let Some(task) = tasks.get(id) {
            task.set_completed();
        }
    }

    /// Mark a task as failed, appending the error to its output.
    pub fn fail(&self, id: &str, err: &str) {
        let tasks = self.tasks.read().unwrap();
        if let Some(task) = tasks.get(id) {
            task.set_failed(err);
        }
    }

    /// Kill a running agent by triggering its CancellationToken.
    /// Returns true if the task was found and killed.
    pub fn kill(&self, id: &str) -> bool {
        let tasks = self.tasks.read().unwrap();
        if let Some(task) = tasks.get(id) {
            task.set_killed()
        } else {
            false
        }
    }

    /// Get a task by ID.
    pub fn get(&self, id: &str) -> Option<Arc<AgentTask>> {
        let tasks = self.tasks.read().unwrap();
        tasks.get(id).cloned()
    }

    /// List all tasks, newest first (by start_time).
    pub fn list(&self) -> Vec<Arc<AgentTask>> {
        let tasks = self.tasks.read().unwrap();
        let mut list: Vec<Arc<AgentTask>> = tasks.values().cloned().collect();
        list.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        list
    }

    /// List tasks filtered by status.
    pub fn list_by_status(&self, status: AgentTaskStatus) -> Vec<Arc<AgentTask>> {
        let tasks = self.tasks.read().unwrap();
        let mut list: Vec<Arc<AgentTask>> = tasks
            .values()
            .filter(|t| t.status() == status)
            .cloned()
            .collect();
        list.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        list
    }

    /// Count of all tasks.
    pub fn count(&self) -> usize {
        let tasks = self.tasks.read().unwrap();
        tasks.len()
    }

    /// Update tools_used and duration_ms for a task.
    pub fn update_stats(&self, id: &str, tools_used: u32, duration_ms: u64) {
        let tasks = self.tasks.read().unwrap();
        if let Some(task) = tasks.get(id) {
            task.set_stats(tools_used, duration_ms);
        }
    }
}

impl Default for AgentTaskStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_get() {
        let store = AgentTaskStore::new();
        let task = store.create("test task", "general", "do something", "test-model");
        assert_eq!(task.id.len(), 8);
        assert_eq!(task.task_type, "local_agent");
        assert_eq!(task.status(), AgentTaskStatus::Pending);

        let got = store.get(&task.id).unwrap();
        assert_eq!(got.id, task.id);
    }

    #[test]
    fn test_create_with_id() {
        let store = AgentTaskStore::new();
        let task = store.create_with_id("abcd1234", "test", "", "", "");
        assert_eq!(task.id, "abcd1234");
        assert!(store.get("abcd1234").is_some());
    }

    #[test]
    fn test_start_sets_running() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        let cancel = CancellationToken::new();
        store.start(&task.id, cancel);
        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status(), AgentTaskStatus::Running);
        assert!(task.cancel_handle().is_some());
    }

    #[test]
    fn test_complete_sets_completed() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        let cancel = CancellationToken::new();
        store.start(&task.id, cancel);
        store.complete(&task.id);
        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status(), AgentTaskStatus::Completed);
        assert!(task.is_terminal());
        assert!(task.end_time().is_some());
    }

    #[test]
    fn test_fail_sets_failed_and_appends_error() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        let cancel = CancellationToken::new();
        store.start(&task.id, cancel);
        store.fail(&task.id, "something went wrong");
        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status(), AgentTaskStatus::Failed);
        assert!(task.get_output().contains("[ERROR] something went wrong"));
    }

    #[test]
    fn test_kill_cancels_and_marks_killed() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        let cancel = CancellationToken::new();
        store.start(&task.id, cancel.clone());
        assert!(!cancel.is_cancelled());

        let killed = store.kill(&task.id);
        assert!(killed);
        assert!(cancel.is_cancelled());

        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status(), AgentTaskStatus::Killed);
    }

    #[test]
    fn test_kill_nonexistent() {
        let store = AgentTaskStore::new();
        assert!(!store.kill("nonexistent"));
    }

    #[test]
    fn test_kill_already_terminal() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        let cancel = CancellationToken::new();
        store.start(&task.id, cancel);
        store.complete(&task.id);
        assert!(!store.kill(&task.id)); // already terminal
    }

    #[test]
    fn test_fail_already_terminal() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        store.complete(&task.id);
        store.fail(&task.id, "too late");
        let task = store.get(&task.id).unwrap();
        assert_eq!(task.status(), AgentTaskStatus::Completed); // unchanged
    }

    #[test]
    fn test_list_newest_first() {
        let store = AgentTaskStore::new();
        let _t1 = store.create("first", "", "", "");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _t2 = store.create("second", "", "", "");

        let list = store.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].description, "second");
        assert_eq!(list[1].description, "first");
    }

    #[test]
    fn test_list_by_status() {
        let store = AgentTaskStore::new();
        let t1 = store.create("pending", "", "", "");
        let t2 = store.create("running", "", "", "");
        store.start(&t2.id, CancellationToken::new());
        let t3 = store.create("completed", "", "", "");
        store.complete(&t3.id);

        assert_eq!(store.list_by_status(AgentTaskStatus::Pending).len(), 1);
        assert_eq!(store.list_by_status(AgentTaskStatus::Running).len(), 1);
        assert_eq!(store.list_by_status(AgentTaskStatus::Completed).len(), 1);
        assert_eq!(store.list_by_status(AgentTaskStatus::Failed).len(), 0);
    }

    #[test]
    fn test_count() {
        let store = AgentTaskStore::new();
        assert_eq!(store.count(), 0);
        store.create("a", "", "", "");
        assert_eq!(store.count(), 1);
        store.create("b", "", "", "");
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn test_write_output_and_get_output() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        task.write_output("line 1\n");
        task.write_output("line 2\n");
        let output = task.get_output();
        assert!(output.contains("line 1\n"));
        assert!(output.contains("line 2\n"));
    }

    #[test]
    fn test_output_cap_at_50kb() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        let big = "x".repeat(30 * 1024);
        task.write_output(&big);
        task.write_output(&big);
        let output = task.get_output();
        assert!(output.len() <= MAX_OUTPUT_SIZE);
        assert!(output.contains("chars truncated"));
    }

    #[test]
    fn test_status_display() {
        assert_eq!(AgentTaskStatus::Pending.to_string(), "pending");
        assert_eq!(AgentTaskStatus::Running.to_string(), "running");
        assert_eq!(AgentTaskStatus::Killed.to_string(), "killed");
    }

    #[test]
    fn test_status_from_str() {
        assert_eq!(AgentTaskStatus::from_str("pending"), Some(AgentTaskStatus::Pending));
        assert_eq!(AgentTaskStatus::from_str("running"), Some(AgentTaskStatus::Running));
        assert_eq!(AgentTaskStatus::from_str("bogus"), None);
    }

    #[test]
    fn test_update_stats() {
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        store.update_stats(&task.id, 42, 1234);
        let task = store.get(&task.id).unwrap();
        assert_eq!(task.tools_used(), 42);
        assert_eq!(task.duration_ms(), 1234);
    }

    #[test]
    fn test_output_survives_status_change() {
        // Key test: writing output during the running phase should still be
        // available after the task is marked as completed.
        let store = AgentTaskStore::new();
        let task = store.create("task", "", "", "");
        let cancel = CancellationToken::new();
        store.start(&task.id, cancel);
        task.write_output("Hello from agent\n");
        store.complete(&task.id);
        let task = store.get(&task.id).unwrap();
        assert!(task.get_output().contains("Hello from agent"));
    }
}
