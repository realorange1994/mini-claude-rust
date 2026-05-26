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

/// Extract token count from the last assistant message with valid usage data.
/// Returns 0 if no such message exists.
/// Ported from Go's tokenCountFromLastAPIResponse.
pub fn extract_token_count_from_messages(messages: &[serde_json::Value]) -> i64 {
    for msg in messages.iter().rev() {
        if let Some(usage) = get_token_usage(msg) {
            return usage.total_tokens();
        }
    }
    0
}

/// Extract only output_tokens from the last assistant message with usage data.
/// Ported from Go's messageTokenCountFromLastAPIResponse.
pub fn extract_message_token_count_from_last_response(messages: &[serde_json::Value]) -> i64 {
    for msg in messages.iter().rev() {
        if let Some(usage) = get_token_usage(msg) {
            return usage.output_only();
        }
    }
    0
}

/// Get the current usage from the last assistant message with non-placeholder data.
/// Skips placeholder/placeholder usage (all zeros).
/// Ported from Go's getCurrentUsage.
pub fn get_current_usage(messages: &[serde_json::Value]) -> Option<UsageInfo> {
    for msg in messages.iter().rev() {
        if let Some(usage) = get_token_usage(msg) {
            if !usage.is_placeholder() {
                return Some(usage);
            }
        }
    }
    None
}

/// Check if the most recent assistant message's total token count exceeds the threshold.
/// Ported from Go's doesMostRecentAssistantMessageExceed200k.
pub fn does_most_recent_assistant_message_exceed(messages: &[serde_json::Value], threshold: i64) -> bool {
    for msg in messages.iter().rev() {
        if let Some(usage) = get_token_usage(msg) {
            return usage.total_tokens() > threshold;
        }
        // Found an assistant message but no usage data
        return false;
    }
    false
}

/// Extract usage data from an assistant message.
/// Returns None for non-assistant messages, synthetic model messages, or messages without usage.
/// Ported from Go's getTokenUsage.
fn get_token_usage(msg: &serde_json::Value) -> Option<UsageInfo> {
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
    if role != "assistant" {
        return None;
    }

    // Skip synthetic messages
    if let Some(model) = msg.get("model").and_then(|v| v.as_str()) {
        if model == "<synthetic>" {
            return None;
        }
    }

    let usage = msg.get("usage")?;
    serde_json::from_value(usage.clone()).ok()
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

    #[test]
    fn test_extract_token_count_from_messages() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hello"}], "usage": {"input_tokens": 10, "output_tokens": 5, "cache_creation_input_tokens": 3, "cache_read_input_tokens": 2}}),
        ];
        let total = extract_token_count_from_messages(&messages);
        assert_eq!(total, 20); // 10 + 3 + 2 + 5

        // Output only
        let output_only = extract_message_token_count_from_last_response(&messages);
        assert_eq!(output_only, 5);
    }

    #[test]
    fn test_does_most_recent_assistant_message_exceed_threshold() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hello"}], "usage": {"input_tokens": 150000, "output_tokens": 60000, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}),
        ];
        assert!(does_most_recent_assistant_message_exceed(&messages, 200000));

        let messages_small = vec![
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hello"}], "usage": {"input_tokens": 100, "output_tokens": 50, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}),
        ];
        assert!(!does_most_recent_assistant_message_exceed(&messages_small, 200000));
    }
}
