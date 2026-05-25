//! API message normalization for KV cache reuse (Hermes-style)
//!
//! Normalizes API messages before sending to improve Anthropic prefix cache hit rate:
//! 1. Sort JSON keys in tool_call input by alphabetical order
//! 2. Normalize whitespace in tool_result content (collapse multiple blank lines)
//! 3. These normalizations make identical logical content produce identical API payloads,
//!    which is critical for Anthropic's prefix caching to work effectively.

use serde_json::{json, Value};

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

// ─── Advanced API Message Normalization ───────────────────────────────────────

/// Full normalization pipeline for API messages (matching Go NormalizeAPIMessages).
/// The normalization order matters for cache stability.
pub fn normalize_api_messages_full(messages: &[Value]) -> Vec<Value> {
    let mut msgs = messages.to_vec();
    msgs = hoist_tool_results(&msgs);
    msgs = enforce_role_alternation(&msgs);
    msgs = hoist_tool_results(&msgs); // re-hoist after merge
    msgs = ensure_tool_result_pairing(&msgs);
    msgs = filter_empty_messages(&msgs);
    msgs = strip_images_from_error_tool_results(&msgs);
    msgs = strip_empty_text_blocks(&msgs);
    // Apply existing normalizations (sort keys, whitespace)
    msgs = normalize_api_messages(&msgs);
    msgs
}

/// Hoist tool_result blocks to the front of each user message's content array.
/// This ensures a stable, deterministic ordering regardless of how content blocks
/// were originally appended, which is critical for KV cache prefix stability.
pub fn hoist_tool_results(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "user" {
                return msg.clone();
            }
            let content = match msg.get("content").and_then(|c| c.as_array()) {
                Some(arr) if arr.len() > 1 => arr,
                _ => return msg.clone(),
            };

            // Check if there are tool_result blocks not already at the front
            let has_tool_result = content
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
            if !has_tool_result {
                return msg.clone();
            }

            // Partition: tool_results first, then everything else
            let mut tool_results = Vec::new();
            let mut others = Vec::new();
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    tool_results.push(block.clone());
                } else {
                    others.push(block.clone());
                }
            }

            let mut result = msg.clone();
            let mut new_content = tool_results;
            new_content.extend(others);
            result["content"] = Value::Array(new_content);
            result
        })
        .collect()
}

/// Ensures messages alternate between user and assistant roles.
/// Consecutive same-role messages are merged. If the first message is from
/// the assistant, a synthetic user message is prepended.
pub fn enforce_role_alternation(messages: &[Value]) -> Vec<Value> {
    if messages.is_empty() {
        return messages.to_vec();
    }

    let mut result: Vec<Value> = Vec::with_capacity(messages.len());

    // If the first message is from assistant, prepend a synthetic user message
    if messages[0].get("role").and_then(|r| r.as_str()) == Some("assistant") {
        result.push(json!({
            "role": "user",
            "content": [{"type": "text", "text": "[System: conversation starts with assistant response]"}]
        }));
    }

    for msg in messages {
        if result.is_empty() {
            result.push(msg.clone());
            continue;
        }

        let last_role = result
            .last()
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str())
            .unwrap_or("");
        let msg_role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        if msg_role == last_role {
            // Merge consecutive same-role messages by combining content blocks
            let last = result.last_mut().unwrap();
            let last_content = last.get("content").cloned().unwrap_or(Value::Null);
            let msg_content = msg.get("content").cloned().unwrap_or(Value::Null);

            let merged = match (last_content, msg_content.clone()) {
                (Value::Array(a), Value::Array(b)) => {
                    let mut combined = a;
                    combined.extend(b);
                    Value::Array(combined)
                }
                (Value::String(a), Value::String(b)) => Value::String(format!("{}\n\n{}", a, b)),
                (Value::Array(a), Value::String(b)) => {
                    let mut combined = a;
                    combined.push(json!({"type": "text", "text": b}));
                    Value::Array(combined)
                }
                (Value::String(a), Value::Array(b)) => {
                    let mut combined = vec![json!({"type": "text", "text": a})];
                    combined.extend(b);
                    Value::Array(combined)
                }
                _ => msg_content,
            };
            last["content"] = merged;
        } else {
            result.push(msg.clone());
        }
    }

    result
}

/// Ensures every tool_use has a matching tool_result and vice versa.
/// - Forward pass: insert synthetic error tool_result for orphaned tool_use blocks.
/// - Reverse pass: strip tool_result blocks whose tool_use_id doesn't match any tool_use.
pub fn ensure_tool_result_pairing(messages: &[Value]) -> Vec<Value> {
    if messages.is_empty() {
        return messages.to_vec();
    }

    // Collect all tool_use IDs from assistant messages
    let mut all_tool_use_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                        if !id.is_empty() {
                            all_tool_use_ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    // Collect all tool_result IDs from user messages
    let mut all_tool_result_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    if let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) {
                        if !id.is_empty() {
                            all_tool_result_ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    // Forward pass: insert synthetic tool_results for orphaned tool_uses
    let mut result: Vec<Value> = Vec::with_capacity(messages.len());
    let mut seen_tool_use_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        if role == "assistant" {
            if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                let mut new_content: Vec<Value> = Vec::new();
                let mut orphaned_ids: Vec<String> = Vec::new();

                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                            if !id.is_empty() && seen_tool_use_ids.contains(id) {
                                continue; // dedup
                            }
                            if !id.is_empty() {
                                seen_tool_use_ids.insert(id.to_string());
                            }
                            if !id.is_empty() && !all_tool_result_ids.contains(id) {
                                orphaned_ids.push(id.to_string());
                            }
                        }
                    }
                    new_content.push(block.clone());
                }

                let mut result_msg = msg.clone();
                result_msg["content"] = Value::Array(new_content);
                result.push(result_msg);

                if !orphaned_ids.is_empty() {
                    let synthetic_blocks: Vec<Value> = orphaned_ids
                        .iter()
                        .map(|id| {
                            json!({
                                "type": "tool_result",
                                "tool_use_id": id,
                                "is_error": true,
                                "content": [{"type": "text", "text": "Tool execution was interrupted"}]
                            })
                        })
                        .collect();
                    result.push(json!({
                        "role": "user",
                        "content": synthetic_blocks
                    }));
                }
            } else {
                result.push(msg.clone());
            }
        } else {
            result.push(msg.clone());
        }
    }

    // Reverse pass: strip orphaned tool_result blocks and dedup
    let mut seen_result_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for msg in &mut result {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()).cloned() {
            let filtered: Vec<Value> = content
                .into_iter()
                .filter(|block| {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        return true;
                    }
                    let id = block
                        .get("tool_use_id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("");
                    if id.is_empty() {
                        return true;
                    }
                    if !all_tool_use_ids.contains(id) {
                        return false;
                    }
                    if seen_result_ids.contains(id) {
                        return false; // dedup
                    }
                    seen_result_ids.insert(id.to_string());
                    true
                })
                .collect();
            msg["content"] = Value::Array(filtered);
        }
    }

    result
}

/// Removes or fixes messages that would cause API 400 errors:
/// - Whitespace-only assistant messages are removed.
/// - Assistant messages with only empty content blocks get a placeholder.
pub fn filter_empty_messages(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter(|msg| {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "assistant" {
                return true;
            }
            // Check if assistant message is whitespace-only
            if is_whitespace_only_assistant(msg) {
                return false;
            }
            true
        })
        .map(|msg| {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role == "assistant" && has_only_empty_content(msg) {
                let mut result = msg.clone();
                result["content"] = json!([{"type": "text", "text": "[thinking...]"}]);
                return result;
            }
            msg.clone()
        })
        .collect()
}

/// Checks if an assistant message is whitespace-only.
fn is_whitespace_only_assistant(msg: &Value) -> bool {
    let content = msg.get("content");
    match content {
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(arr)) => {
            if arr.is_empty() {
                return true;
            }
            arr.iter().all(|block| {
                block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t.trim().is_empty())
            })
        }
        None | Some(Value::Null) => true,
        _ => false,
    }
}

/// Checks if an assistant message has only empty content blocks.
fn has_only_empty_content(msg: &Value) -> bool {
    let content = match msg.get("content").and_then(|c| c.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return false,
    };
    content.iter().all(|block| {
        block
            .get("text")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.trim().is_empty())
    })
}

/// Strips image blocks from error tool_results (API requirement).
pub fn strip_images_from_error_tool_results(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "user" {
                return msg.clone();
            }
            let content = match msg.get("content").and_then(|c| c.as_array()) {
                Some(arr) => arr,
                _ => return msg.clone(),
            };

            let mut modified = false;
            let new_content: Vec<Value> = content
                .iter()
                .map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        && block.get("is_error").is_some()
                    {
                        if let Some(inner) = block.get("content").and_then(|c| c.as_array()) {
                            let filtered: Vec<Value> = inner
                                .iter()
                                .filter(|b| {
                                    b.get("type").and_then(|t| t.as_str()) != Some("image")
                                })
                                .cloned()
                                .collect();
                            if filtered.len() != inner.len() {
                                modified = true;
                                let mut new_block = block.clone();
                                new_block["content"] = Value::Array(filtered);
                                return new_block;
                            }
                        }
                    }
                    block.clone()
                })
                .collect();

            if modified {
                let mut result = msg.clone();
                result["content"] = Value::Array(new_content);
                result
            } else {
                msg.clone()
            }
        })
        .collect()
}

/// Removes empty text blocks from message content arrays.
pub fn strip_empty_text_blocks(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let content = match msg.get("content").and_then(|c| c.as_array()) {
                Some(arr) => arr,
                _ => return msg.clone(),
            };

            let filtered: Vec<Value> = content
                .iter()
                .filter(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        return !text.is_empty();
                    }
                    true
                })
                .cloned()
                .collect();

            if filtered.len() != content.len() {
                let mut result = msg.clone();
                result["content"] = Value::Array(filtered);
                result
            } else {
                msg.clone()
            }
        })
        .collect()
}

/// Strips the SystemInjectedPrefix from a message's content.
/// The prefix is only used internally for breakpoint placement decisions.
pub fn strip_system_injected(msg: &mut Value) {
    let prefix = crate::context::SYSTEM_INJECTED_PREFIX;
    if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
        if s.starts_with(prefix) {
            msg["content"] = Value::String(s[prefix.len()..].to_string());
        }
    } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()).cloned() {
        if let Some(first) = arr.first() {
            if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                if text.starts_with(prefix) {
                    let mut new_arr = arr.clone();
                    if let Some(first) = new_arr.first_mut() {
                        if let Some(obj) = first.as_object_mut() {
                            obj.insert(
                                "text".to_string(),
                                Value::String(text[prefix.len()..].to_string()),
                            );
                        }
                    }
                    msg["content"] = Value::Array(new_arr);
                }
            }
        }
    }
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

#[cfg(test)]
mod normalize_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_normalize_whitespace() {
        // Empty string
        assert_eq!(normalize_whitespace(""), "");

        // Single line (no trailing newline)
        assert_eq!(normalize_whitespace("hello"), "hello");

        // 3+ blank lines collapsed to at most 1 blank line (2 newlines = 1 visual blank line)
        let input = "line1\n\n\n\nline2";
        let result = normalize_whitespace(input);
        assert_eq!(result, "line1\n\nline2", "3+ blank lines should collapse to 1 blank line");

        // Trailing whitespace stripped from lines
        let input = "hello   \nworld  ";
        let result = normalize_whitespace(input);
        assert_eq!(result, "hello\nworld", "trailing whitespace should be stripped");

        // Trailing blank lines removed entirely
        let input = "content\n\n\n";
        let result = normalize_whitespace(input);
        assert_eq!(result, "content", "trailing blank lines should be removed");
    }

    #[test]
    fn test_sort_json_keys() {
        // Flat map: keys sorted alphabetically
        let input = json!({"z": 1, "a": 2, "m": 3});
        let sorted = sort_json_keys(&input);
        let keys: Vec<_> = sorted.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["a", "m", "z"]);

        // Nested map: inner keys also sorted
        let input = json!({"z": {"c": 1, "a": 2}, "a": 3});
        let sorted = sort_json_keys(&input);
        let outer_keys: Vec<_> = sorted.as_object().unwrap().keys().collect();
        assert_eq!(outer_keys, vec!["a", "z"]);
        let inner_keys: Vec<_> = sorted["z"].as_object().unwrap().keys().collect();
        assert_eq!(inner_keys, vec!["a", "c"]);

        // Array containing maps: each map's keys sorted
        let input = json!([{"b": 1, "a": 2}, 42, "hello"]);
        let sorted = sort_json_keys(&input);
        let first_keys: Vec<_> = sorted[0].as_object().unwrap().keys().collect();
        assert_eq!(first_keys, vec!["a", "b"]);
        assert_eq!(sorted[1], json!(42));
        assert_eq!(sorted[2], json!("hello"));

        // Non-object values unchanged
        assert_eq!(sort_json_keys(&json!(42)), json!(42));
        assert_eq!(sort_json_keys(&json!("hello")), json!("hello"));
        assert_eq!(sort_json_keys(&json!(null)), json!(null));
        assert_eq!(sort_json_keys(&json!(true)), json!(true));
    }

    #[test]
    fn test_normalize_api_messages() {
        // Empty vec
        let result = normalize_api_messages(&[]);
        assert!(result.is_empty());

        // Assistant message with tool_use: input keys should be sorted
        let messages = vec![json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "tool-1",
                "name": "edit_file",
                "input": {"file_path": "/src/main.rs", "old_string": "foo", "new_string": "bar"}
            }]
        })];
        let result = normalize_api_messages(&messages);
        let input_keys: Vec<_> = result[0]["content"][0]["input"]
            .as_object().unwrap().keys().collect();
        assert_eq!(input_keys, vec!["file_path", "new_string", "old_string"],
            "tool_use input keys should be sorted alphabetically");

        // User message with tool_result: whitespace in content should be normalized
        let messages = vec![json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "tool-1",
                "content": [{
                    "type": "text",
                    "text": "output\n\n\n\nmore"
                }]
            }]
        })];
        let result = normalize_api_messages(&messages);
        let text = result[0]["content"][0]["content"][0]["text"]
            .as_str().unwrap();
        assert_eq!(text, "output\n\nmore",
            "tool_result whitespace should be normalized (3+ blank lines collapsed)");
    }
}
