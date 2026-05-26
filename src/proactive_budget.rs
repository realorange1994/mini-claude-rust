//! Context window budget monitoring with behavioral hints.
//! Ported from upstream proactive_budget.go (114 lines).
//!
//! Monitors context window usage and injects behavioral hints when
//! approaching limits to avoid 400 "context too long" errors.

/// Tracks context window usage and injects behavioral hints.
pub struct ProactiveBudgetManager {
    context_window: i64,
}

impl ProactiveBudgetManager {
    /// Create a new budget manager for the given context window size.
    pub fn new(context_window: i64) -> Self {
        Self { context_window }
    }

    /// Return a behavioral hint based on current usage.
    /// Returns empty string if no hint needed.
    ///
    /// Thresholds:
    ///   > 90%: URGENT hint (context nearly full)
    ///   > 75%: Concise hint (context getting full)
    ///   > 50%: Mild hint (consider being concise)
    pub fn budget_hint(&self, current_tokens: i64) -> &'static str {
        if self.context_window <= 0 {
            return "";
        }
        let usage_percent = current_tokens as f64 / self.context_window as f64 * 100.0;

        if usage_percent > 90.0 {
            "URGENT: The context window is nearly full (>90%). Use minimal, targeted tool calls. Do NOT read entire files. Prefer grep/search to find specific lines. Keep tool call arguments short."
        } else if usage_percent > 75.0 {
            "Note: The context window is getting full (>75%). Be concise in your tool call arguments and prefer smaller, targeted edits over large writes."
        } else if usage_percent > 50.0 {
            "Consider being concise in your tool call arguments to keep the context window manageable."
        } else {
            ""
        }
    }

    /// Return true if proactive compaction should be triggered (75% usage).
    pub fn should_proactive_compact(&self, current_tokens: i64) -> bool {
        if self.context_window <= 0 {
            return false;
        }
        let usage_percent = current_tokens as f64 / self.context_window as f64;
        usage_percent > 0.75
    }
}

/// Generate a self-correction hint based on a tool error.
///
/// Patterns:
///   - File not found → suggest searching for the file
///   - Grep no results → suggest broadening the pattern
///   - Edit failed (old_string not found) → suggest reading the file first
///   - Permission denied → suggest alternative approach
pub fn tool_error_self_correction_hint(tool_name: &str, err_msg: &str, params: &std::collections::HashMap<String, serde_json::Value>) -> String {
    let msg = err_msg.to_lowercase();

    // File not found errors
    if msg.contains("no such file") || msg.contains("file not found") || msg.contains("does not exist") {
        if let Some(path) = params.get("file_path").and_then(|v| v.as_str()) {
            return format!(
                "The file '{}' does not exist. Try using search_files or glob to find the correct path, or check if you need to create it first with write_file.",
                path
            );
        }
        return "The file does not exist. Try using search_files or glob to find the correct path.".to_string();
    }

    // Grep/search no results
    if (tool_name == "grep" || tool_name == "search_files" || tool_name == "rgrep")
        && (msg.contains("no matches") || msg.contains("0 results") || msg.contains("nothing found"))
    {
        if let Some(pattern) = params.get("pattern").and_then(|v| v.as_str()) {
            return format!(
                "No results for pattern '{}'. Try broadening the search: use a simpler pattern, remove special characters, or search in a different directory.",
                pattern
            );
        }
        return "No search results. Try broadening the pattern or searching in a different directory.".to_string();
    }

    // Edit failed (old_string not found)
    if tool_name == "edit_file" && (msg.contains("not found") || msg.contains("does not match")) {
        if let Some(path) = params.get("file_path").and_then(|v| v.as_str()) {
            return format!(
                "The old_string was not found in '{}'. Read the file first to see its current content, then use the exact text that exists in the file.",
                path
            );
        }
        return "The old_string was not found. Read the file first to see its current content.".to_string();
    }

    // Permission denied
    if msg.contains("permission denied") || msg.contains("access denied") {
        return "Permission denied. Try using a different approach or check if you need elevated permissions.".to_string();
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_hint_low_usage() {
        let mgr = ProactiveBudgetManager::new(200_000);
        assert_eq!(mgr.budget_hint(50_000), ""); // 25%
        assert_eq!(mgr.budget_hint(100_000), ""); // 50%
    }

    #[test]
    fn test_budget_hint_medium_usage() {
        let mgr = ProactiveBudgetManager::new(200_000);
        let hint = mgr.budget_hint(120_000); // 60%
        assert!(!hint.is_empty());
        assert!(hint.contains("concise"));
    }

    #[test]
    fn test_budget_hint_high_usage() {
        let mgr = ProactiveBudgetManager::new(200_000);
        let hint = mgr.budget_hint(160_000); // 80%
        assert!(!hint.is_empty());
        assert!(hint.contains("75%"));
    }

    #[test]
    fn test_budget_hint_urgent() {
        let mgr = ProactiveBudgetManager::new(200_000);
        let hint = mgr.budget_hint(190_000); // 95%
        assert!(!hint.is_empty());
        assert!(hint.contains("URGENT"));
    }

    #[test]
    fn test_should_proactive_compact() {
        let mgr = ProactiveBudgetManager::new(200_000);
        assert!(!mgr.should_proactive_compact(100_000)); // 50%
        assert!(!mgr.should_proactive_compact(140_000)); // 70%
        assert!(mgr.should_proactive_compact(160_000)); // 80%
    }

    #[test]
    fn test_tool_error_hint_file_not_found() {
        let params = std::collections::HashMap::from([
            ("file_path".to_string(), serde_json::json!("missing.txt")),
        ]);
        let hint = tool_error_self_correction_hint("read_file", "file not found", &params);
        assert!(!hint.is_empty());
        assert!(hint.contains("missing.txt"));
    }

    #[test]
    fn test_tool_error_hint_grep_no_results() {
        let params = std::collections::HashMap::from([
            ("pattern".to_string(), serde_json::json!("fnord_12345")),
        ]);
        let hint = tool_error_self_correction_hint("grep", "0 results found", &params);
        assert!(!hint.is_empty());
        assert!(hint.contains("fnord_12345"));
    }

    #[test]
    fn test_tool_error_hint_edit_not_found() {
        let params = std::collections::HashMap::from([
            ("file_path".to_string(), serde_json::json!("main.rs")),
        ]);
        let hint = tool_error_self_correction_hint("edit_file", "old_string not found", &params);
        assert!(!hint.is_empty());
        assert!(hint.contains("main.rs"));
        assert!(hint.contains("Read the file first"));
    }
}
