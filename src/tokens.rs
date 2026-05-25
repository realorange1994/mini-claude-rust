//! Token counting utilities for API usage tracking.
//! Ported from upstream tokens.ts and Go's tokens.go.

use serde::{Deserialize, Serialize};

/// UsageInfo represents token usage data from an API response.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UsageInfo {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
}

impl UsageInfo {
    /// Total context window tokens from usage data.
    /// Includes input_tokens + cache tokens + output_tokens.
    pub fn total_tokens(&self) -> i64 {
        self.input_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
            + self.output_tokens
    }

    /// Returns the output tokens only.
    pub fn output_only(&self) -> i64 {
        self.output_tokens
    }

    /// Check if this is placeholder usage (all zeros).
    pub fn is_placeholder(&self) -> bool {
        self.input_tokens == 0
            && self.cache_creation_input_tokens == 0
            && self.cache_read_input_tokens == 0
            && self.output_tokens == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_total_tokens() {
        let usage = UsageInfo {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 30,
            cache_read_input_tokens: 20,
        };
        assert_eq!(usage.total_tokens(), 200);
    }

    #[test]
    fn test_is_placeholder() {
        assert!(UsageInfo::default().is_placeholder());
        let usage = UsageInfo {
            input_tokens: 1,
            ..Default::default()
        };
        assert!(!usage.is_placeholder());
    }
}
