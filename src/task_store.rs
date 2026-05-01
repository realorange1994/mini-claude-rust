//! TaskStore -- Runtime task tracking for background bash tasks and sub-agents.
//!
//! Ported from Go's agent_task.go. Manages task lifecycle (pending -> running ->
//! completed/failed/killed) with OS process handles for kill support.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ─── TaskStatus ─────────────────────────────────────────────────────────────

/// Represents the state of a background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Killed => "killed",
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ─── TaskState ──────────────────────────────────────────────────────────────

/// Holds the state of a single running or completed background task.
pub struct TaskState {
    pub id: String,
    pub task_type: String,
    pub status: TaskStatus,
    pub description: String,
    pub output_file: Option<String>,
    /// OS process ID for kill support.
    pub pid: Option<u32>,
    /// When set, the task will be evicted after this instant.
    evict_after: Option<Instant>,
}

impl TaskState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        )
    }

    /// Mark as completed and schedule eviction.
    pub fn complete(&mut self, result: &str) {
        self.status = TaskStatus::Completed;
        self.description = format!("{} [completed: {}]", self.description, result);
        self.evict_after = Some(Instant::now() + Duration::from_secs(30));
    }

    /// Mark as failed and schedule eviction.
    pub fn fail(&mut self, error: &str) {
        self.status = TaskStatus::Failed;
        self.description = format!("{} [failed: {}]", self.description, error);
        self.evict_after = Some(Instant::now() + Duration::from_secs(30));
    }

    /// Mark as killed and schedule eviction.
    pub fn kill(&mut self) {
        self.status = TaskStatus::Killed;
        self.description = format!("{} [killed]", self.description);
        self.evict_after = Some(Instant::now() + Duration::from_secs(30));
    }
}

// ─── TaskStore ──────────────────────────────────────────────────────────────

/// Manages all background tasks for an agent session.
pub struct TaskStore {
    tasks: Mutex<HashMap<String, Arc<Mutex<TaskState>>>>,
}

/// Shared type used across the codebase.
pub type SharedTaskStore = Arc<TaskStore>;

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// Create a new shared instance.
    pub fn new_shared() -> SharedTaskStore {
        Arc::new(Self::new())
    }

    /// Register a new background bash task. Returns the task ID.
    pub fn register_bash_bg_task(
        &self,
        description: String,
        output_file: String,
    ) -> String {
        let task_id = generate_bash_task_id();
        let task = TaskState {
            id: task_id.clone(),
            task_type: "bash_background".to_string(),
            status: TaskStatus::Running,
            description,
            output_file: Some(output_file),
            pid: None,
            evict_after: None,
        };
        let mut tasks = self.tasks.lock().unwrap();
        tasks.insert(task_id.clone(), Arc::new(Mutex::new(task)));
        task_id
    }

    /// Set the PID for a task (must be called right after spawning the process).
    pub fn set_pid(&self, id: &str, pid: u32) {
        let tasks = self.tasks.lock().unwrap();
        if let Some(task_arc) = tasks.get(id) {
            let mut task = task_arc.lock().unwrap();
            task.pid = Some(pid);
        }
    }

    /// Update the output file path for a task.
    pub fn update_output_file(&self, id: &str, output_file: String) {
        let tasks = self.tasks.lock().unwrap();
        if let Some(task_arc) = tasks.get(id) {
            let mut task = task_arc.lock().unwrap();
            task.output_file = Some(output_file);
        }
    }

    /// Get a clone of the shared Arc<Mutex<TaskState>> for a task.
    pub fn get_task(&self, id: &str) -> Option<Arc<Mutex<TaskState>>> {
        let tasks = self.tasks.lock().unwrap();
        tasks.get(id).cloned()
    }

    /// Check if a task is in a terminal state (completed, failed, killed).
    pub fn is_terminal(&self, id: &str) -> bool {
        let tasks = self.tasks.lock().unwrap();
        tasks
            .get(id)
            .map(|t| t.lock().unwrap().is_terminal())
            .unwrap_or(true) // unknown tasks are treated as terminal
    }

    /// Kill a running task by sending a kill signal to its OS process.
    /// Returns Ok(()) if the task was found and killed, or Err with a message.
    pub fn kill_task(&self, id: &str) -> Result<(), String> {
        let task_arc = {
            let tasks = self.tasks.lock().unwrap();
            tasks.get(id).cloned()
        };

        let task_arc = task_arc.ok_or_else(|| format!("Background task {} not found", id))?;
        let mut task = task_arc.lock().unwrap();

        // Guard: if already terminal, don't overwrite
        if task.is_terminal() {
            return Ok(());
        }

        // Kill the OS process using the stored PID
        if let Some(pid) = task.pid {
            kill_process(pid);
        }

        task.kill();
        Ok(())
    }

    /// Mark a task as completed.
    pub fn complete_task(&self, id: &str, result: &str) {
        let tasks = self.tasks.lock().unwrap();
        if let Some(task_arc) = tasks.get(id) {
            let mut task = task_arc.lock().unwrap();
            if !task.is_terminal() {
                task.complete(result);
            }
        }
    }

    /// Mark a task as failed.
    pub fn fail_task(&self, id: &str, error: &str) {
        let tasks = self.tasks.lock().unwrap();
        if let Some(task_arc) = tasks.get(id) {
            let mut task = task_arc.lock().unwrap();
            if !task.is_terminal() {
                task.fail(error);
            }
        }
    }

    /// Remove tasks whose evict_after timestamp has passed.
    /// Also deletes associated output files for bash background tasks.
    pub fn cleanup_evicted(&self) {
        let mut to_remove: Vec<String> = Vec::new();
        let mut to_delete_files: Vec<String> = Vec::new();

        {
            let tasks = self.tasks.lock().unwrap();
            for (id, task_arc) in tasks.iter() {
                let task = task_arc.lock().unwrap();
                if let Some(evict_after) = task.evict_after {
                    if Instant::now() >= evict_after {
                        to_remove.push(id.clone());
                        if let Some(ref output_file) = task.output_file {
                            to_delete_files.push(output_file.clone());
                        }
                    }
                }
            }
        }

        // Delete output files
        for path in &to_delete_files {
            let _ = std::fs::remove_file(path);
        }

        // Remove from store
        if !to_remove.is_empty() {
            let mut tasks = self.tasks.lock().unwrap();
            for id in &to_remove {
                tasks.remove(id);
            }
        }
    }
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Process kill helpers ───────────────────────────────────────────────────

/// Kill a process by PID using platform-specific commands.
fn kill_process(pid: u32) {
    #[cfg(unix)]
    {
        // Send SIGKILL to the process
        // Use std::process::Command to run kill for cross-platform safety
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .output();
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(&["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
}

// ─── Task ID generation ─────────────────────────────────────────────────────

/// Generate a unique task ID in the format "b" + 8 random alphanumeric chars.
fn generate_bash_task_id() -> String {
    use uuid::Uuid;
    let uuid = Uuid::new_v4().to_string();
    // Take first 8 chars from the hyphen-free hex representation
    let hex: String = uuid.chars().filter(|c| *c != '-').take(8).collect();
    format!("b{}", hex)
}

// ─── Output file helpers ────────────────────────────────────────────────────

/// Returns the directory for background bash task output files.
pub fn bash_bg_tasks_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".claude").join("tasks").join("bash")
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_store_register_and_get() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "ls -la".to_string(),
            "/tmp/test.output".to_string(),
        );
        assert!(id.starts_with('b'));
        assert_eq!(id.len(), 9); // 'b' + 8 chars

        let task = store.get_task(&id);
        assert!(task.is_some());
        let task = task.unwrap();
        let task = task.lock().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.task_type, "bash_background");
    }

    #[test]
    fn test_task_store_kill_task() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "sleep 100".to_string(),
            "/tmp/test.output".to_string(),
        );

        // Kill should succeed even without PID set
        let result = store.kill_task(&id);
        assert!(result.is_ok());

        // After kill, task should be in Killed state
        let task = store.get_task(&id).unwrap();
        let task = task.lock().unwrap();
        assert_eq!(task.status, TaskStatus::Killed);
    }

    #[test]
    fn test_task_store_kill_with_pid() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "sleep 100".to_string(),
            "/tmp/test.output".to_string(),
        );

        // Set a fake PID (the kill_process function will try to kill it but that's fine for testing)
        store.set_pid(&id, 999999);

        let result = store.kill_task(&id);
        assert!(result.is_ok());

        let task = store.get_task(&id).unwrap();
        let task = task.lock().unwrap();
        assert_eq!(task.status, TaskStatus::Killed);
        assert_eq!(task.pid, Some(999999));
    }

    #[test]
    fn test_task_store_complete_and_fail() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "echo hello".to_string(),
            "/tmp/test.output".to_string(),
        );

        store.complete_task(&id, "success");
        let task = store.get_task(&id).unwrap();
        let task = task.lock().unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert!(task.is_terminal());
        assert!(task.evict_after.is_some());

        let id2 = store.register_bash_bg_task(
            "false".to_string(),
            "/tmp/test2.output".to_string(),
        );
        drop(task);

        store.fail_task(&id2, "exit code 1");
        let task2 = store.get_task(&id2).unwrap();
        let task2 = task2.lock().unwrap();
        assert_eq!(task2.status, TaskStatus::Failed);
        assert!(task2.is_terminal());
    }

    #[test]
    fn test_task_store_kill_nonexistent() {
        let store = TaskStore::new();
        let result = store.kill_task("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_task_store_cleanup_evicted() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "echo test".to_string(),
            "/tmp/test.output".to_string(),
        );

        store.complete_task(&id, "ok");
        assert!(store.get_task(&id).is_some());

        // Manually set evict_after to past for testing
        {
            let tasks = store.tasks.lock().unwrap();
            if let Some(task_arc) = tasks.get(&id) {
                let mut task = task_arc.lock().unwrap();
                task.evict_after = Some(Instant::now() - Duration::from_secs(1));
            }
        }

        store.cleanup_evicted();
        assert!(store.get_task(&id).is_none());
    }

    #[test]
    fn test_task_store_is_terminal() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "echo hi".to_string(),
            "/tmp/test.output".to_string(),
        );

        assert!(!store.is_terminal(&id));
        store.complete_task(&id, "ok");
        assert!(store.is_terminal(&id));

        // Unknown task is treated as terminal
        assert!(store.is_terminal("unknown"));
    }

    #[test]
    fn test_task_id_format() {
        let id = generate_bash_task_id();
        assert!(id.starts_with('b'));
        assert_eq!(id.len(), 9);
        for c in id.chars().skip(1) {
            assert!(c.is_ascii_alphanumeric());
        }
    }

    #[test]
    fn test_task_store_double_kill() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "sleep 10".to_string(),
            "/tmp/test.output".to_string(),
        );

        // First kill
        let result1 = store.kill_task(&id);
        assert!(result1.is_ok());

        // Second kill should also return Ok (guard: already terminal)
        let result2 = store.kill_task(&id);
        assert!(result2.is_ok());
    }

    #[test]
    fn test_task_store_complete_after_kill() {
        let store = TaskStore::new();
        let id = store.register_bash_bg_task(
            "sleep 10".to_string(),
            "/tmp/test.output".to_string(),
        );

        // Kill first
        store.kill_task(&id).unwrap();

        // Complete after kill should NOT change status (guard)
        store.complete_task(&id, "done");
        let task = store.get_task(&id).unwrap();
        let task = task.lock().unwrap();
        assert_eq!(task.status, TaskStatus::Killed); // still killed, not completed
    }
}
