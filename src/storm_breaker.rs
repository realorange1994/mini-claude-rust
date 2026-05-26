//! StormBreaker — detects and suppresses repeat-loop tool call storms.
//!
//! This matches DeepSeek-Reasonix's StormBreaker pattern: when the LLM calls
//! the same tool with identical arguments multiple times in a row (e.g., reading
//! the same file 3+ times), subsequent calls are suppressed.
//!
//! Key design decisions:
//!   - Mutating calls (edit_file, write_file) clear prior read-only entries
//!     from the window so post-edit verification reads are not falsely flagged
//!   - 3 identical mutating calls in a row still triggers suppression (genuine loop)
//!   - Cheap state-inspection tools are exempt from the check

use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::Mutex;

/// A single entry in the storm detection window.
#[derive(Clone, Debug)]
struct StormEntry {
    name: String,
    args_hash: String, // first 4 bytes (8 hex chars) of SHA-256 of arguments JSON
    mutating: bool,
}

/// StormBreaker configuration.
const DEFAULT_WINDOW: usize = 20;
const DEFAULT_THRESHOLD: usize = 3;

/// StormBreaker detects and suppresses repeat-loop tool call storms.
pub struct StormBreaker {
    mu: Mutex<VecDeque<StormEntry>>,
    window: usize,
    threshold: usize,
}

impl StormBreaker {
    /// Create a new StormBreaker with default settings.
    pub fn new() -> Self {
        Self {
            mu: Mutex::new(VecDeque::with_capacity(DEFAULT_WINDOW)),
            window: DEFAULT_WINDOW,
            threshold: DEFAULT_THRESHOLD,
        }
    }

    /// Inspect a tool call. Returns a reason string if the call should be suppressed,
    /// or empty string if allowed to proceed.
    pub fn inspect(&self, tool_name: &str, input: &serde_json::Value) -> String {
        // Exempt cheap state-inspection tools
        if is_exempt_tool(tool_name) {
            return String::new();
        }

        let mut guard = self.mu.lock().unwrap();
        let args_hash = hash_args(input);
        let mutating = is_mutating_tool(tool_name);

        if mutating {
            // Drop prior read-only entries — the file/shell state just changed,
            // so a verify-read after this should start with a clean slate.
            // Keep mutator entries: 3 identical edits in a row is still a storm.
            let entries: Vec<StormEntry> = guard.iter()
                .filter(|e| e.mutating)
                .cloned()
                .collect();
            *guard = entries.into();
        }

        // Count identical calls in window
        let count = guard.iter()
            .filter(|e| e.name == tool_name && e.args_hash == args_hash)
            .count();

        // If we've seen this call threshold-1 times already, suppress
        if count >= self.threshold - 1 {
            return format!(
                "Storm breaker: {} was called with identical arguments {} times in a row. This appears to be a repeat loop. Try a different approach.",
                tool_name,
                count + 1
            );
        }

        // Record this call
        guard.push_back(StormEntry {
            name: tool_name.to_string(),
            args_hash,
            mutating,
        });

        // Trim window
        while guard.len() > self.window {
            guard.pop_front();
        }

        String::new()
    }

    /// Reset the storm breaker state. Called at turn boundaries.
    pub fn reset(&self) {
        let mut guard = self.mu.lock().unwrap();
        guard.clear();
    }
}

impl Default for StormBreaker {
    fn default() -> Self {
        Self::new()
    }
}

/// Exempt tools that bypass storm detection (cheap state-inspection tools).
fn is_exempt_tool(name: &str) -> bool {
    matches!(name, "list_dir" | "glob" | "tool_search")
}

/// Mutating tools are tools that modify state (writes, deletes, etc.).
fn is_mutating_tool(name: &str) -> bool {
    matches!(name, "edit_file" | "multi_edit" | "write_file" | "fileops" | "exec")
}

/// Compute a short hash of the tool arguments.
fn hash_args(input: &serde_json::Value) -> String {
    let data = serde_json::to_vec(input).unwrap_or_default();
    let hash = Sha256::digest(&data);
    format!("{:02x}{:02x}{:02x}{:02x}", hash[0], hash[1], hash[2], hash[3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_exempt_tools() {
        assert!(is_exempt_tool("list_dir"));
        assert!(is_exempt_tool("glob"));
        assert!(is_exempt_tool("tool_search"));
        assert!(!is_exempt_tool("read_file"));
        assert!(!is_exempt_tool("edit_file"));
    }

    #[test]
    fn test_mutating_tools() {
        assert!(is_mutating_tool("edit_file"));
        assert!(is_mutating_tool("multi_edit"));
        assert!(is_mutating_tool("write_file"));
        assert!(is_mutating_tool("fileops"));
        assert!(is_mutating_tool("exec"));
        assert!(!is_mutating_tool("read_file"));
    }

    #[test]
    fn test_storm_breaker_exempt() {
        let breaker = StormBreaker::new();
        let result = breaker.inspect("list_dir", &json!({"path": "/tmp"}));
        assert!(result.is_empty());
    }

    #[test]
    fn test_storm_breaker_allows_first_calls() {
        let breaker = StormBreaker::new();
        let input = json!({"path": "/tmp/test.txt"});

        // First two calls should be allowed
        assert!(breaker.inspect("read_file", &input).is_empty());
        assert!(breaker.inspect("read_file", &input).is_empty());
    }

    #[test]
    fn test_storm_breaker_suppresses_third() {
        let breaker = StormBreaker::new();
        let input = json!({"path": "/tmp/test.txt"});

        // Third identical call should be suppressed
        let _ = breaker.inspect("read_file", &input);
        let _ = breaker.inspect("read_file", &input);
        let result = breaker.inspect("read_file", &input);

        assert!(!result.is_empty());
        assert!(result.contains("Storm breaker"));
    }

    #[test]
    fn test_storm_breaker_reset() {
        let breaker = StormBreaker::new();
        let input = json!({"path": "/tmp/test.txt"});

        let _ = breaker.inspect("read_file", &input);
        let _ = breaker.inspect("read_file", &input);

        breaker.reset();

        // After reset, should be allowed again
        let result = breaker.inspect("read_file", &input);
        assert!(result.is_empty());
    }

    #[test]
    fn test_mutating_clears_read_only() {
        let breaker = StormBreaker::new();
        let read_input = json!({"path": "/tmp/test.txt"});
        let write_input = json!({"path": "/tmp/test.txt", "content": "hello"});

        // Do two read_file calls
        let _ = breaker.inspect("read_file", &read_input);
        let _ = breaker.inspect("read_file", &read_input);

        // Do a mutating call - should clear read-only entries
        let _ = breaker.inspect("write_file", &write_input);

        // Now another read should be allowed (read-only entries cleared)
        let result = breaker.inspect("read_file", &read_input);
        assert!(result.is_empty(), "read entries should be cleared after mutating call");
    }
}