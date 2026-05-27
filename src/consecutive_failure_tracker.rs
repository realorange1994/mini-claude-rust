//! Consecutive failure tracking for tool call validation.
//!
//! Ported from `go:consecutive_failure_tracker.go`.
//!
//! When the same tool call (same tool + same args) fails validation or is rejected
//! by a gate two times in a row, returns a sharper error telling the model NOT to
//! retry with identical args. This saves an entire wasted API round-trip.

use serde_json::Value;
use std::collections::HashMap;

/// Tracks consecutive identical tool call failures to prevent wasted turns.
pub struct ConsecutiveCallTracker {
    /// Per-tool fingerprint of the last validation failure.
    last_malformed: HashMap<String, String>,
    /// Per-tool rejection for gate failures: toolName -> "reason:fingerprint".
    last_gate_rejection: HashMap<String, String>,
}

impl ConsecutiveCallTracker {
    pub fn new() -> Self {
        Self {
            last_malformed: HashMap::new(),
            last_gate_rejection: HashMap::new(),
        }
    }

    /// Compute a short hash of tool arguments for fingerprinting.
    fn fingerprint_args(args: &Value) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let json = serde_json::to_string(args).unwrap_or_default();
        let mut hasher = DefaultHasher::new();
        json.hash(&mut hasher);
        let h = hasher.finish();
        format!("{:016x}", h)
    }

    /// Check if a tool call's validation just failed.
    /// Returns an error hint if this is the second consecutive identical failure.
    pub fn check_malformed_call(
        &mut self,
        tool_name: &str,
        args: &Value,
        detail: &str,
    ) -> String {
        let fp = Self::fingerprint_args(args);
        let prev = self.last_malformed.get(tool_name).cloned();
        self.last_malformed.insert(tool_name.to_string(), fp.clone());

        if let Some(prev_fp) = prev {
            if prev_fp == fp && !prev_fp.is_empty() {
                return format!(
                    "{}: same call just failed validation ({}) \
                     — DO NOT retry with identical args. \
                     Either fix the call (read the schema in the tool spec) \
                     or pick a different tool.",
                    tool_name, detail
                );
            }
        }
        String::new()
    }

    /// Check if a tool call was just rejected by a gate.
    /// Returns an error hint if this is the second consecutive identical rejection.
    pub fn check_gate_rejection(
        &mut self,
        tool_name: &str,
        args: &Value,
        result: &str,
    ) -> String {
        let reason = rejection_reason(tool_name, result);
        if reason.is_empty() {
            // Not a rejection, clear tracking.
            self.last_gate_rejection.remove(tool_name);
            return String::new();
        }

        let fp = Self::fingerprint_args(args);
        let key = format!("{}:{}", reason, fp);
        let prev = self.last_gate_rejection.get(tool_name).cloned();
        self.last_gate_rejection.insert(tool_name.to_string(), key.clone());

        if let Some(prev_key) = prev {
            if prev_key == key && !prev_key.is_empty() {
                return format!(
                    "{}: same call was just rejected by {} — \
                     do not retry identical args. {}",
                    tool_name,
                    reason,
                    rejection_recovery_hint(&reason)
                );
            }
        }
        String::new()
    }

    /// Reset tracker state.
    pub fn clear(&mut self) {
        self.last_malformed.clear();
        self.last_gate_rejection.clear();
    }
}

/// Extract the rejection reason from a tool result.
fn rejection_reason(tool_name: &str, result: &str) -> &'static str {
    let lower = result.to_lowercase();

    // Check for edit-gate rejection.
    if (tool_name == "edit_file" || tool_name == "write_file")
        && lower.contains("rejected this edit")
    {
        return "edit-gate";
    }
    // Check for shell-gate rejection.
    if tool_name == "exec"
        && (lower.contains("rejected")
            || lower.contains("not allowed")
            || lower.contains("forbidden"))
    {
        return "shell-gate";
    }
    // Check for read-before-edit rejection.
    if (tool_name == "edit_file" || tool_name == "multi_edit")
        && (lower.contains("read")
            && (lower.contains("first") || lower.contains("before")))
    {
        return "read-before-edit";
    }
    // Check for engineering-lifecycle rejection.
    if lower.contains("engineering")
        && (lower.contains("lifecycle") || lower.contains("checkpoint") || lower.contains("evidence"))
    {
        return "engineering-lifecycle";
    }
    ""
}

/// Return tool-specific recovery guidance.
fn rejection_recovery_hint(reason: &str) -> &'static str {
    match reason {
        "edit-gate" => {
            "Do not re-emit the same edit. Try a genuinely different edit \
             or ask the user how to proceed."
        }
        "read-before-edit" => {
            "Call read_file on the target path first, then re-issue the edit."
        }
        "shell-gate" => {
            "Do not retry the same command. Use an allowlisted/read-only command, \
             wait for approval, or ask the user how to proceed."
        }
        "engineering-lifecycle" => {
            "Switch to read-only exploration, submit or revise the plan, \
             or choose a different tool call."
        }
        _ => "Choose a different tool call or ask the user how to proceed.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_malformed_no_error() {
        let mut tracker = ConsecutiveCallTracker::new();
        let args = serde_json::json!({"path": "foo.rs", "content": "test"});
        let result = tracker.check_malformed_call("edit_file", &args, "missing content");
        assert!(result.is_empty());
    }

    #[test]
    fn test_second_malformed_returns_error() {
        let mut tracker = ConsecutiveCallTracker::new();
        let args = serde_json::json!({"path": "foo.rs", "content": "test"});

        let r1 = tracker.check_malformed_call("edit_file", &args, "missing content");
        assert!(r1.is_empty());

        let r2 = tracker.check_malformed_call("edit_file", &args, "missing content");
        assert!(r2.contains("DO NOT retry"));
        assert!(r2.contains("edit_file"));
    }

    #[test]
    fn test_different_args_no_error() {
        let mut tracker = ConsecutiveCallTracker::new();
        let args1 = serde_json::json!({"path": "foo.rs", "content": "test1"});
        let args2 = serde_json::json!({"path": "foo.rs", "content": "test2"});

        let r1 = tracker.check_malformed_call("edit_file", &args1, "missing content");
        assert!(r1.is_empty());

        let r2 = tracker.check_malformed_call("edit_file", &args2, "missing content");
        assert!(r2.is_empty(), "different args should not trigger error");
    }

    #[test]
    fn test_gate_rejection() {
        let mut tracker = ConsecutiveCallTracker::new();
        let args = serde_json::json!({"path": "foo.rs", "edits": []});

        let r1 = tracker.check_gate_rejection("edit_file", &args, "rejected this edit");
        assert!(r1.is_empty());

        let r2 = tracker.check_gate_rejection("edit_file", &args, "rejected this edit");
        assert!(r2.contains("edit-gate"));
        assert!(r2.contains("do not retry identical"));
    }

    #[test]
    fn test_clear_resets_state() {
        let mut tracker = ConsecutiveCallTracker::new();
        let args = serde_json::json!({"path": "foo.rs", "content": "test"});

        tracker.check_malformed_call("edit_file", &args, "missing content");
        tracker.clear();

        // After clear, this should be treated as first failure.
        let r = tracker.check_malformed_call("edit_file", &args, "missing content");
        assert!(r.is_empty());
    }
}
