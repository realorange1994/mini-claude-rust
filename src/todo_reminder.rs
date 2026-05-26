//! TODO reminder injection for agent context.
//! Ported from upstream todo_reminder.go (91 lines).
//!
//! When the agent hasn't created/used tasks in a while, injects a reminder
//! to use the task list for tracking multi-step work.

/// Tracks tool usage to decide when to inject TODO reminders.
pub struct TodoReminderTracker {
    /// Number of consecutive non-task tool calls since last task-related call.
    non_task_count: usize,
    /// Threshold of non-task calls before injecting a reminder.
    threshold: usize,
}

/// Tool names that are considered "task-related".
const TASK_TOOLS: &[&str] = &["todo_write", "todo_read", "task_create", "task_update", "task_list", "task_get"];

/// Default threshold before injecting reminder.
const DEFAULT_THRESHOLD: usize = 5;

impl TodoReminderTracker {
    pub fn new() -> Self {
        Self {
            non_task_count: 0,
            threshold: DEFAULT_THRESHOLD,
        }
    }

    /// Create with a custom threshold.
    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            non_task_count: 0,
            threshold,
        }
    }

    /// Record that a tool was called. Returns true if a TODO reminder should be injected.
    pub fn record_tool_call(&mut self, tool_name: &str) -> bool {
        if TASK_TOOLS.contains(&tool_name) {
            self.non_task_count = 0;
            return false;
        }

        self.non_task_count += 1;
        self.non_task_count >= self.threshold
    }

    /// Reset the counter (e.g., after injecting a reminder).
    pub fn reset(&mut self) {
        self.non_task_count = 0;
    }

    /// Get the current non-task count.
    pub fn non_task_count(&self) -> usize {
        self.non_task_count
    }
}

impl Default for TodoReminderTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate a TODO reminder message for injection into the assistant's context.
pub fn todo_reminder_message() -> String {
    "You haven't used the task list recently. For multi-step work, consider using TaskCreate to track your progress and show the user what you're working on.".to_string()
}

/// Check if a tool call result suggests the task is complex enough
/// to warrant TODO tracking (e.g., multi-file changes).
pub fn is_complex_task_hint(tool_name: &str, result: &str) -> bool {
    if tool_name == "edit_file" || tool_name == "write_file" {
        // If the result mentions multiple files or locations
        let lower = result.to_lowercase();
        lower.contains("multiple") || lower.contains("several") || lower.contains("3 or more")
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_reminder_initially() {
        let mut tracker = TodoReminderTracker::new();
        assert!(!tracker.record_tool_call("read_file")); // 1
        assert!(!tracker.record_tool_call("read_file")); // 2
    }

    #[test]
    fn test_reminder_after_threshold() {
        let mut tracker = TodoReminderTracker::with_threshold(3);
        assert!(!tracker.record_tool_call("read_file"));   // 1
        assert!(!tracker.record_tool_call("grep"));        // 2
        assert!(tracker.record_tool_call("edit_file"));    // 3 -> trigger
    }

    #[test]
    fn test_task_tool_resets_counter() {
        let mut tracker = TodoReminderTracker::with_threshold(3);
        assert!(!tracker.record_tool_call("read_file"));   // 1
        assert!(!tracker.record_tool_call("read_file"));   // 2
        assert!(!tracker.record_tool_call("task_create")); // reset
        assert!(!tracker.record_tool_call("read_file"));   // 1
        assert!(!tracker.record_tool_call("read_file"));   // 2
        assert!(tracker.record_tool_call("read_file"));    // 3 -> trigger
    }

    #[test]
    fn test_reset() {
        let mut tracker = TodoReminderTracker::with_threshold(2);
        assert!(!tracker.record_tool_call("read_file")); // 1
        tracker.reset();
        assert!(!tracker.record_tool_call("read_file")); // 1
    }

    #[test]
    fn test_reminder_message_not_empty() {
        assert!(!todo_reminder_message().is_empty());
    }

    #[test]
    fn test_is_complex_task_hint() {
        assert!(is_complex_task_hint("edit_file", "Changed multiple locations"));
        assert!(!is_complex_task_hint("edit_file", "Changed one line"));
        assert!(!is_complex_task_hint("read_file", "multiple lines"));
    }
}
