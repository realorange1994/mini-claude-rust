//! Reasoning Retention — optimizes messages for thinking-mode providers.
//!
//! This module contains functions for DeepSeek-Reasonix healing patterns:
//! - reasoningRetention(): strips thinking from non-tool-call messages
//! - thinkingModeStamping(): ensures thinking blocks exist in thinking mode
//! - stampMissingToolCallIDs(): adds synthetic IDs to tool_use blocks
//! - shrinkToolCallArgsByTokens(): shrinks oversized tool call arguments
//!
//! Note: The Rust implementation doesn't store thinking blocks in context,
//! so reasoningRetention and thinkingModeStamping are stubs that return 0.

use serde_json::{Map, Value};

/// reasoningRetention strips reasoning_content from assistant messages
/// that don't have tool_calls. This reduces request size and improves cache
/// hit rate by removing stale reasoning from old turns.
///
/// In Rust, thinking blocks are not stored in context, so this is a no-op
/// that returns (0, 0).
#[allow(unused_variables)]
pub fn reasoning_retention(messages: &mut [Value]) -> (usize, usize) {
    // Rust doesn't store thinking blocks in context - this is a no-op.
    // The API conversion layer handles thinking externally.
    (0, 0)
}

/// thinkingModeStamping ensures all assistant messages have a thinking block
/// when in thinking mode. DeepSeek returns 400 error if thinking/reasoning
/// is missing on a response that previously had it.
///
/// In Rust, this is handled at the API layer, so returns 0.
#[allow(unused_variables)]
pub fn thinking_mode_stamping(messages: &mut [Value], is_thinking_mode: bool) -> usize {
    // Thinking block stamping is handled at the API layer
    0
}

/// stampMissingToolCallIDs adds missing tool_use_id to tool_calls that don't have one.
/// DeepSeek returns 400 error on tool_calls without id field.
///
/// Returns the count of IDs added.
pub fn stamp_missing_tool_call_ids(messages: &mut [Value]) -> usize {
    let mut stamped = 0;
    let mut seq: usize = 0;

    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|v| v.as_str());
        if role != Some("assistant") {
            continue;
        }

        let content = match msg.get_mut("content") {
            Some(Value::Array(arr)) => arr,
            _ => continue,
        };

        for block in content.iter_mut() {
            let block_type = block.get("type").and_then(|v| v.as_str());
            if block_type != Some("tool_use") {
                continue;
            }

            // Check if id already exists
            if block.get("id").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                continue;
            }

            // Add synthetic id
            let id = format!("z-ext-{}", seq);
            block.as_object_mut().unwrap().insert("id".to_string(), Value::String(id));
            seq += 1;
            stamped += 1;
        }
    }

    stamped
}

/// shrinkToolCallArgsByTokens shrinks oversized tool call argument JSON by
/// replacing long string values (>300 chars) with placeholder text.
///
/// Returns (healed_count, chars_saved).
pub fn shrink_tool_call_args_by_tokens(messages: &mut [Value], max_token_chars: usize) -> (usize, usize) {
    const LONG_THRESHOLD: usize = 300;
    let mut healed_count = 0;
    let mut chars_saved = 0;

    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|v| v.as_str());
        if role != Some("assistant") {
            continue;
        }

        let content = match msg.get_mut("content") {
            Some(Value::Array(arr)) => arr,
            _ => continue,
        };

        for block in content.iter_mut() {
            let block_type = block.get("type").and_then(|v| v.as_str());
            if block_type != Some("tool_use") {
                continue;
            }

            // Get input (can be string or object)
            let input = match block.get_mut("input") {
                Some(v) => v,
                None => continue,
            };

            let input_str = match input {
                Value::String(s) => s.clone(),
                Value::Object(_) => serde_json::to_string(input).unwrap_or_default(),
                _ => continue,
            };

            // Skip if small enough
            if input_str.len() <= max_token_chars {
                continue;
            }

            // Shrink long strings
            let (shrunk, saved) = shrink_json_long_strings(&input_str, LONG_THRESHOLD);
            if saved > 0 {
                *input = Value::String(shrunk);
                healed_count += 1;
                chars_saved += saved;
            }
        }
    }

    (healed_count, chars_saved)
}

/// Shrink long strings in JSON to placeholders.
fn shrink_json_long_strings(json_str: &str, threshold: usize) -> (String, usize) {
    let parsed: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return (json_str.to_string(), 0),
    };

    let obj = match &parsed {
        Value::Object(m) => m.clone(),
        _ => return (json_str.to_string(), 0),
    };

    let mut output = Map::new();
    let mut saved = 0;

    for (k, v) in obj {
        if let Value::String(s) = &v {
            if s.len() > threshold {
                let newlines = s.matches('\n').count();
                let placeholder = format!(
                    "[...shrunk: {} chars, {} lines - tool already responded, see result]",
                    s.len(),
                    newlines
                );
                saved += s.len() - placeholder.len();
                output.insert(k, Value::String(placeholder));
                continue;
            }
        }
        output.insert(k, v);
    }

    let result = serde_json::to_string(&Value::Object(output)).unwrap_or_else(|_| json_str.to_string());
    (result, saved)
}

/// Check if any message in the conversation indicates thinking mode
/// (has thinking blocks).
pub fn is_thinking_mode_active(messages: &[Value]) -> bool {
    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str());
        if role != Some("assistant") {
            continue;
        }

        if let Some(Value::Array(content)) = msg.get("content") {
            for block in content {
                let block_type = block.get("type").and_then(|v| v.as_str());
                if block_type == Some("thinking") || block_type == Some("redacted_thinking") {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_stamp_missing_tool_call_ids_none() {
        let mut messages = vec![json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "existing-id",
                "name": "read_file",
                "input": {"path": "/tmp/test.txt"}
            }]
        })];

        let count = stamp_missing_tool_call_ids(&mut messages);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_stamp_missing_tool_call_ids_adds() {
        let mut messages = vec![json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "name": "read_file",
                "input": {"path": "/tmp/test.txt"}
            }]
        })];

        let count = stamp_missing_tool_call_ids(&mut messages);
        assert_eq!(count, 1);
        assert_eq!(messages[0]["content"][0]["id"], "z-ext-0");
    }

    #[test]
    fn test_stamp_multiple_ids() {
        let mut messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "name": "read_file", "input": {}},
                {"type": "tool_use", "name": "grep", "input": {}}
            ]
        })];

        let count = stamp_missing_tool_call_ids(&mut messages);
        assert_eq!(count, 2);
        assert_eq!(messages[0]["content"][0]["id"], "z-ext-0");
        assert_eq!(messages[0]["content"][1]["id"], "z-ext-1");
    }

    #[test]
    fn test_shrink_json_long_strings() {
        let json = r#"{"path": "this is a very long string that exceeds the threshold and should be shrunk to save tokens", "short": "ok"}"#;
        let (shrunk, saved) = shrink_json_long_strings(json, 50);
        assert!(saved > 0);
        assert!(shrunk.contains("[...shrunk:"));
    }

    #[test]
    fn test_shrink_json_long_strings_small() {
        let json = r#"{"path": "short"}"#;
        let (shrunk, saved) = shrink_json_long_strings(json, 50);
        assert_eq!(saved, 0);
        assert_eq!(shrunk, json);
    }

    #[test]
    fn test_is_thinking_mode_active() {
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "hello"}]}),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "let me think..."},
                {"type": "text", "text": "hello"}
            ]}),
        ];
        assert!(is_thinking_mode_active(&messages));
    }

    #[test]
    fn test_is_thinking_mode_inactive() {
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "hello"}]}),
            json!({"role": "assistant", "content": [
                {"type": "text", "text": "hello"}
            ]}),
        ];
        assert!(!is_thinking_mode_active(&messages));
    }
}