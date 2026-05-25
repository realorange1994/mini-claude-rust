//! Session status display utilities.
//! Ported from upstream status.go.

use crate::cost_tracker::format_token_count;

/// StatusReport captures the current session status.
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub model: String,
    pub permission_mode: String,
    pub message_count: usize,
    pub estimated_tokens: i64,
    pub remaining_budget: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
    pub cache_deleted: i64,
    pub cost_display: Option<String>,
    pub turns: u64,
    pub streaming_enabled: bool,
}

impl StatusReport {
    /// Format the status report for display.
    pub fn format(&self) -> String {
        let mut lines = Vec::new();
        lines.push("\n=== Session Status ===".to_string());
        lines.push(format!("Model: {}", self.model));
        lines.push(format!("Mode:  {}", self.permission_mode));
        lines.push(format!(
            "Messages: {} (est. {} tokens)",
            self.message_count,
            format_token_count(self.estimated_tokens)
        ));
        lines.push(format!(
            "Token Budget: {} remaining",
            format_token_count(self.remaining_budget)
        ));
        lines.push(format!("Input Tokens:    {}", format_token_count(self.input_tokens)));
        lines.push(format!(
            "Output Tokens:   {}",
            format_token_count(self.output_tokens)
        ));
        lines.push(format!(
            "Cache Creation:  {}  Cache Read: {}  Cache Deleted: {}",
            format_token_count(self.cache_creation),
            format_token_count(self.cache_read),
            format_token_count(self.cache_deleted)
        ));

        // Cache hit rate
        let total_cache = self.cache_creation + self.cache_read;
        if total_cache > 0 {
            let hit_rate = (self.cache_read as f64 / total_cache as f64) * 100.0;
            lines.push(format!("Cache Hit Rate:  {:.0}%", hit_rate));
        } else {
            lines.push("Cache Hit Rate:  N/A (no cache usage yet)".to_string());
        }

        // Cost tracking
        if let Some(ref cost) = self.cost_display {
            lines.push(format!("Token Usage:     {}", cost));
        }

        lines.push(format!("Turns:           {}", self.turns));

        if self.streaming_enabled {
            lines.push("Streaming:       enabled".to_string());
        } else {
            lines.push("Streaming:       disabled".to_string());
        }

        lines.push("======================".to_string());
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_format_basic() {
        let report = StatusReport {
            model: "claude-sonnet-4-6".to_string(),
            permission_mode: "ask".to_string(),
            message_count: 5,
            estimated_tokens: 2000,
            remaining_budget: 198000,
            input_tokens: 1500,
            output_tokens: 500,
            cache_creation: 300,
            cache_read: 200,
            cache_deleted: 0,
            cost_display: Some("Total: 2.0k tokens".to_string()),
            turns: 3,
            streaming_enabled: true,
        };
        let output = report.format();
        assert!(output.contains("Session Status"));
        assert!(output.contains("claude-sonnet-4-6"));
        assert!(output.contains("Cache Hit Rate:  40%"));
        assert!(output.contains("Streaming:       enabled"));
    }

    #[test]
    fn test_cache_hit_rate_na() {
        let report = StatusReport {
            model: "test".to_string(),
            permission_mode: "auto".to_string(),
            message_count: 1,
            estimated_tokens: 100,
            remaining_budget: 200000,
            input_tokens: 100,
            output_tokens: 50,
            cache_creation: 0,
            cache_read: 0,
            cache_deleted: 0,
            cost_display: None,
            turns: 1,
            streaming_enabled: false,
        };
        let output = report.format();
        assert!(output.contains("N/A"));
    }
}
