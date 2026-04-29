//! API message normalization for KV cache reuse (Hermes-style)
//!
//! Normalizes API messages before sending to improve Anthropic prefix cache hit rate:
//! 1. Sort JSON keys in tool_call input by alphabetical order
//! 2. Normalize whitespace in tool_result content (collapse multiple blank lines)
//! 3. These normalizations make identical logical content produce identical API payloads,
//!    which is critical for Anthropic's prefix caching to work effectively.

use serde_json::Value;

/// Normalize a list of API messages for KV cache reuse.
/// Returns a new vector with normalized messages.
pub fn normalize_api_messages(messages: &[Value]) -> Vec<Value> {
    messages.iter().map(normalize_message).collect()
}

/// Normalize a single API message
fn normalize_message(msg: &Value) -> Value {
    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

    match role {
        "assistant" => {
            // Normalize tool_use blocks: sort input JSON keys
            if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                let normalized_content: Vec<Value> = content.iter().map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        normalize_tool_use_block(block)
                    } else {
                        block.clone()
                    }
                }).collect();
                let mut result = msg.clone();
                result["content"] = Value::Array(normalized_content);
                return result;
            }
            msg.clone()
        }
        "user" => {
            // Normalize tool_result content: collapse whitespace
            if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                let normalized_content: Vec<Value> = content.iter().map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        normalize_tool_result_block(block)
                    } else {
                        block.clone()
                    }
                }).collect();
                let mut result = msg.clone();
                result["content"] = Value::Array(normalized_content);
                return result;
            }
            msg.clone()
        }
        _ => msg.clone(),
    }
}

/// Normalize a tool_use block: sort input JSON keys alphabetically
fn normalize_tool_use_block(block: &Value) -> Value {
    let input = block.get("input");
    match input {
        Some(Value::Object(map)) => {
            // Sort keys and rebuild the object
            let sorted: Vec<(String, Value)> = map.iter()
                .map(|(k, v)| (k.clone(), sort_json_keys(v)))
                .collect();
            // serde_json::Map preserves insertion order, so inserting in sorted order works
            let mut sorted_map = serde_json::Map::new();
            for (k, v) in sorted {
                sorted_map.insert(k, v);
            }
            let mut result = block.clone();
            result["input"] = Value::Object(sorted_map);
            result
        }
        _ => block.clone(),
    }
}

/// Recursively sort JSON object keys
fn sort_json_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted_map = serde_json::Map::new();
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(k, _)| *k);
            for (k, v) in entries {
                sorted_map.insert(k.clone(), sort_json_keys(v));
            }
            Value::Object(sorted_map)
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(sort_json_keys).collect())
        }
        _ => value.clone(),
    }
}

/// Normalize a tool_result block: collapse excessive whitespace in text content
fn normalize_tool_result_block(block: &Value) -> Value {
    let content = block.get("content");
    match content {
        Some(Value::Array(arr)) => {
            let normalized: Vec<Value> = arr.iter().map(|c| {
                if c.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = c.get("text").and_then(|t| t.as_str()) {
                        let normalized_text = normalize_whitespace(text);
                        let mut result = c.clone();
                        result["text"] = Value::String(normalized_text);
                        return result;
                    }
                }
                c.clone()
            }).collect();
            let mut result = block.clone();
            result["content"] = Value::Array(normalized);
            result
        }
        _ => block.clone(),
    }
}

/// Normalize whitespace: collapse 3+ consecutive blank lines into 2,
/// trim trailing whitespace from lines
fn normalize_whitespace(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut consecutive_blank = 0;

    for line in lines {
        let trimmed = line.trim_end();

        if trimmed.is_empty() {
            consecutive_blank += 1;
            if consecutive_blank <= 1 {
                result.push(trimmed.to_string());
            }
            // Skip 2nd+ consecutive blank line (keep at most 1 blank line)
        } else {
            consecutive_blank = 0;
            result.push(trimmed.to_string());
        }
    }

    // Remove trailing blank lines
    while result.last().is_some_and(|l| l.is_empty()) {
        result.pop();
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sort_json_keys() {
        let input = json!({"z": 1, "a": 2, "m": 3});
        let sorted = sort_json_keys(&input);
        let keys: Vec<_> = sorted.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["a", "m", "z"]);
    }

    #[test]
    fn test_sort_json_keys_nested() {
        let input = json!({"z": {"c": 1, "a": 2}, "a": 3});
        let sorted = sort_json_keys(&input);
        let outer_keys: Vec<_> = sorted.as_object().unwrap().keys().collect();
        assert_eq!(outer_keys, vec!["a", "z"]);
        let inner_keys: Vec<_> = sorted["z"].as_object().unwrap().keys().collect();
        assert_eq!(inner_keys, vec!["a", "c"]);
    }

    #[test]
    fn test_normalize_whitespace() {
        let input = "line1\n\n\n\nline2\n   \nline3\n\n";
        let normalized = normalize_whitespace(input);
        assert!(!normalized.contains("\n\n\n"), "Should collapse 3+ blank lines, got: {:?}", normalized);
        assert!(normalized.ends_with("line3"), "Should trim trailing blank lines, got: {:?}", normalized);
    }

    #[test]
    fn test_normalize_tool_use_block() {
        let block = json!({
            "type": "tool_use",
            "id": "tool-1",
            "name": "edit_file",
            "input": {"file_path": "src/main.rs", "old_string": "foo", "new_string": "bar"}
        });
        let normalized = normalize_tool_use_block(&block);
        let input_keys: Vec<_> = normalized["input"].as_object().unwrap().keys().collect();
        assert_eq!(input_keys, vec!["file_path", "new_string", "old_string"],
            "Input keys should be sorted alphabetically");
    }

    #[test]
    fn test_normalize_api_messages() {
        let messages = vec![json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "tool-1",
                "name": "exec",
                "input": {"command": "ls", "cwd": "/home"}
            }]
        })];
        let normalized = normalize_api_messages(&messages);
        let input_keys: Vec<_> = normalized[0]["content"][0]["input"]
            .as_object().unwrap().keys().collect();
        assert_eq!(input_keys, vec!["command", "cwd"]);
    }
}
