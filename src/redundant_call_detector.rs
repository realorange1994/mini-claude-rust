//! Redundant call detector for LLM tool calls.
//! Ported from upstream redundant_call_detector.go (99 lines).
//!
//! Tracks recent tool calls per turn to detect redundant patterns:
//! - Reading/editing the same file twice
//! - Searching the same or similar grep pattern twice
//! - Too many sequential read_file calls (3+)

use std::collections::HashMap;

/// Maximum number of recent tool call records to track.
const MAX_RECENT_RECORDS: usize = 20;

/// Maximum number of sequential reads before flagging as excessive.
const MAX_SEQUENTIAL_READS: usize = 3;

/// A single tool call record for redundancy tracking.
#[derive(Debug, Clone)]
struct ToolCallRecord {
    tool_name: String,
    path: String,
    /// Grep pattern (for search tool calls).
    pattern: String,
}

impl ToolCallRecord {
    fn new(tool_name: &str, path: &str, pattern: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            path: path.to_string(),
            pattern: pattern.to_string(),
        }
    }
}

/// Detects redundant tool calls.
pub struct RedundantCallDetector {
    recent_calls: Vec<ToolCallRecord>,
}

impl RedundantCallDetector {
    pub fn new() -> Self {
        Self {
            recent_calls: Vec::with_capacity(MAX_RECENT_RECORDS),
        }
    }

    /// Check if a tool call is redundant. Returns a hint string if so.
    pub fn check_redundant(&self, tool_name: &str, path: &str, pattern: &str) -> Option<String> {
        // Check same-file redundancy
        for record in self.recent_calls.iter().rev() {
            if record.tool_name == tool_name && record.path == path {
                return Some(format!(
                    "Redundant call: {} on '{}' was already used in this turn. Use a different file or approach.",
                    tool_name, path
                ));
            }
            // Check read_file -> edit_file on same path (normal, not redundant)
            // But check edit_file -> edit_file on same path (redundant)
            if tool_name == "edit_file" && record.tool_name == "edit_file" && record.path == path {
                return Some(format!(
                    "Redundant call: edit_file on '{}' was already called. Use multi_edit for multiple edits to the same file.",
                    path
                ));
            }
        }

        // Check grep pattern redundancy
        if (tool_name == "rgrep" || tool_name == "grep" || tool_name == "glob") && !pattern.is_empty() {
            for record in self.recent_calls.iter().rev() {
                if (record.tool_name == "rgrep" || record.tool_name == "grep" || record.tool_name == "glob")
                    && record.pattern == pattern
                {
                    return Some(format!(
                        "Redundant search: the pattern '{}' was already searched. Try a different pattern or refine your search.",
                        pattern
                    ));
                }
            }
        }

        // Check excessive sequential reads
        let recent_reads: usize = self
            .recent_calls
            .iter()
            .rev()
            .take(MAX_SEQUENTIAL_READS + 1)
            .filter(|r| r.tool_name == "read_file")
            .count();
        if recent_reads >= MAX_SEQUENTIAL_READS {
            return Some(format!(
                "Excessive reads: {} sequential read_file calls detected. Consider using rgrep or glob to find relevant files instead.",
                recent_reads
            ));
        }

        None
    }

    /// Record a tool call for future redundancy checks.
    pub fn record(&mut self, tool_name: &str, path: &str, pattern: &str) {
        self.recent_calls
            .push(ToolCallRecord::new(tool_name, path, pattern));

        // Trim to max capacity
        if self.recent_calls.len() > MAX_RECENT_RECORDS {
            let drain_to = self.recent_calls.len() - MAX_RECENT_RECORDS;
            self.recent_calls.drain(..drain_to);
        }
    }

    /// Clear all recent records (e.g., after context compaction).
    pub fn clear(&mut self) {
        self.recent_calls.clear();
    }
}

impl Default for RedundantCallDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_same_file_redundancy() {
        let mut detector = RedundantCallDetector::new();

        // First call is fine
        assert!(detector.check_redundant("read_file", "main.rs", "").is_none());
        detector.record("read_file", "main.rs", "");

        // Second call to same file is redundant
        let hint = detector.check_redundant("read_file", "main.rs", "");
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("Redundant call"));
    }

    #[test]
    fn test_detect_grep_pattern_redundancy() {
        let mut detector = RedundantCallDetector::new();

        assert!(detector.check_redundant("rgrep", "", "TODO").is_none());
        detector.record("rgrep", "", "TODO");

        let hint = detector.check_redundant("rgrep", "", "TODO");
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("Redundant search"));
    }

    #[test]
    fn test_detect_excessive_reads() {
        let mut detector = RedundantCallDetector::new();

        detector.record("read_file", "a.rs", "");
        detector.record("read_file", "b.rs", "");
        detector.record("read_file", "c.rs", "");

        // After 3 reads, next read should flag excessive
        let hint = detector.check_redundant("read_file", "d.rs", "");
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("Excessive reads"));
    }

    #[test]
    fn test_clear() {
        let mut detector = RedundantCallDetector::new();
        detector.record("read_file", "main.rs", "");
        detector.clear();
        assert!(detector.check_redundant("read_file", "main.rs", "").is_none());
    }

    #[test]
    fn test_different_files_not_redundant() {
        let detector = RedundantCallDetector::new();
        assert!(detector.check_redundant("read_file", "a.rs", "").is_none());
        assert!(detector.check_redundant("read_file", "b.rs", "").is_none());
    }
}
