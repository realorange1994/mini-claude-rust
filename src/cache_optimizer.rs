//! Cache optimization for tool results and message pairing.
//!
//! Ported from `go:cache_optimizer.go`.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Truncates text to fit within approximately maxTokens.
/// Rough estimate: ~4 chars per token.
pub fn truncate_for_tokens(text: &str, max_tokens: usize) -> String {
    let approx_max_chars = max_tokens * 4;
    if text.len() <= approx_max_chars {
        return text.to_string();
    }

    let truncated = &text[..approx_max_chars];
    let last_newline = truncated.rfind('\n');
    let last_period = truncated.rfind(". ");

    let break_point = match (last_newline, last_period) {
        (Some(nl), Some(period)) if period > nl && period > approx_max_chars.saturating_sub(200) => period + 1,
        (Some(nl), _) => nl,
        (_, Some(period)) if period > approx_max_chars.saturating_sub(200) => period + 1,
        _ => 0,
    };

    let min_break = std::cmp::max(100, approx_max_chars / 4);
    let result = if break_point > min_break {
        &truncated[..break_point]
    } else {
        &truncated[..std::cmp::max(100, approx_max_chars.saturating_sub(50))]
    };

    format!("{}\n\n[... tool output truncated for token budget; use Read to get full content]", result)
}

/// Provides a bounded token count estimate.
/// Uses simple heuristic: ~4 chars per token for English, ~2 for CJK.
pub fn count_tokens_bounded(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let cjk_count = text.chars().filter(|&r| {
        (r >= '\u{4E00}' && r <= '\u{9FFF}') || // CJK Unified Ideographs
        (r >= '\u{3040}' && r <= '\u{309F}') || // Hiragana
        (r >= '\u{30A0}' && r <= '\u{30FF}')    // Katakana
    }).count();

    let non_cjk = text.len() - cjk_count;
    (non_cjk / 4) + cjk_count
}

/// Shrinks oversized tool results to stay within token budgets.
/// This prevents oversized tool outputs from causing prompt cache misses.
///
/// Returns (healed_count, tokens_saved, chars_saved).
pub fn shrink_oversized_tool_results_by_tokens(
    messages: &mut Vec<Value>,
    max_tokens: usize,
) -> (usize, usize, usize) {
    let mut healed_count = 0;
    let mut tokens_saved = 0;
    let mut chars_saved = 0;

    for msg in messages.iter_mut() {
        let is_user = msg.get("role").and_then(|v| v.as_str()) == Some("user");
        if !is_user {
            continue;
        }

        let content = msg.get_mut("content");
        let content_arr = match content.and_then(|v| v.as_array_mut()) {
            Some(arr) => arr,
            None => continue,
        };

        // Collect indices of text blocks that need truncation
        let mut truncations: Vec<(usize, String, usize, usize)> = Vec::new();

        for (idx, block) in content_arr.iter().enumerate() {
            let block_type = block.get("type").and_then(|v| v.as_str());
            if block_type != Some("text") {
                continue;
            }

            let text = match block.get("text").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => continue,
            };

            if text.len() <= max_tokens {
                continue;
            }

            let before_tokens = count_tokens_bounded(text);
            if before_tokens <= max_tokens {
                continue;
            }

            let truncated = truncate_for_tokens(text, max_tokens);
            let after_tokens = count_tokens_bounded(&truncated);

            if after_tokens >= before_tokens {
                continue;
            }

            let char_diff = text.len().saturating_sub(truncated.len());
            truncations.push((idx, truncated, before_tokens.saturating_sub(after_tokens), char_diff));
        }

        // Apply truncations
        for (idx, truncated, t_saved, c_saved) in truncations {
            if let Some(block) = content_arr.get_mut(idx) {
                block["text"] = Value::String(truncated);
                healed_count += 1;
                tokens_saved += t_saved;
                chars_saved += c_saved;
            }
        }
    }

    (healed_count, tokens_saved, chars_saved)
}

/// Fixes tool call pairing in messages.
/// Drops both unpaired assistant tool_calls and stray tool messages.
/// Returns (filtered_messages, dropped_assistant_calls, dropped_stray_tools).
pub fn fix_tool_call_pairing(messages: Vec<Value>) -> (Vec<Value>, usize, usize) {
    let mut out = Vec::with_capacity(messages.len());
    let mut dropped_assistant_calls = 0;
    let mut dropped_stray_tools = 0;
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        // Check if this is an assistant message with tool_calls
        if msg.get("role").and_then(|v| v.as_str()) == Some("assistant") {
            if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
                let tool_calls: Vec<_> = content.iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                    .cloned()
                    .collect();

                if !tool_calls.is_empty() {
                    // Stamp missing IDs before validation
                    let mut stamped_calls = Vec::new();
                    let mut seq = 0;
                    for call in &tool_calls {
                        let mut call = call.clone();
                        if call.get("id").and_then(|v| v.as_str()).map(|s| s.is_empty()).unwrap_or(true) {
                            let new_id = format!("z-ext-{}", seq);
                            seq += 1;
                            call["id"] = Value::String(new_id);
                        }
                        stamped_calls.push(call);
                    }

                    // Build set of needed tool call IDs
                    let needed_ids: std::collections::HashSet<String> = stamped_calls.iter()
                        .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .collect();

                    // Look for matching tool results in subsequent user-role messages
                    let mut candidates = Vec::new();
                    let mut needed = needed_ids.clone();
                    let mut j = i + 1;

                    while j < messages.len() && !needed.is_empty() {
                        let next_msg = &messages[j];
                        if next_msg.get("role").and_then(|v| v.as_str()) != Some("user") {
                            break;
                        }

                        // Check if this user message contains tool_result blocks
                        let mut has_tool_results = false;
                        let mut matched_ids = Vec::new();

                        if let Some(content) = next_msg.get("content").and_then(|v| v.as_array()) {
                            for block in content {
                                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                                    has_tool_results = true;
                                    if let Some(tool_use_id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                                        if needed.contains(tool_use_id) {
                                            matched_ids.push(tool_use_id.to_string());
                                        }
                                    }
                                }
                            }
                        }

                        if !has_tool_results {
                            break;
                        }

                        if matched_ids.is_empty() {
                            break;
                        }

                        for id in &matched_ids {
                            needed.remove(id.as_str());
                        }
                        candidates.push(next_msg.clone());
                        j += 1;
                    }

                    // If we found all needed tool results, keep the pair
                    if needed.is_empty() {
                        out.push(msg.clone());
                        out.extend(candidates);
                        i = j;
                        continue;
                    } else {
                        // Drop unpaired tool_calls and their partial results
                        dropped_assistant_calls += 1;
                        dropped_stray_tools += candidates.len();
                        i = j;
                        continue;
                    }
                }
            }
        }

        // Check if this is a stray user message containing tool_result blocks
        if msg.get("role").and_then(|v| v.as_str()) == Some("user") {
            if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
                let mut has_tool_result = false;
                let mut has_valid_id = false;

                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        has_tool_result = true;
                        if block.get("tool_use_id").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                            has_valid_id = true;
                            break;
                        }
                    }
                }

                if has_tool_result && !has_valid_id {
                    dropped_stray_tools += 1;
                    i += 1;
                    continue;
                }
            }
        }

        out.push(msg.clone());
        i += 1;
    }

    (out, dropped_assistant_calls, dropped_stray_tools)
}

/// Extract pinned constraints from system prompt to preserve them across compaction.
/// Pattern: # HIGH PRIORITY constraints, # User memory, # Project memory.
pub fn extract_pinned_constraints(system_prompt: &str) -> String {
    let headers = [
        "HIGH PRIORITY constraints",
        "User memory",
        "Project memory",
    ];

    let mut results = Vec::new();
    let mut current = Vec::new();
    let mut active_header = false;

    for line in system_prompt.lines() {
        let trimmed = line.trim();
        let is_header = trimmed.starts_with("# ");

        let header_matched = if is_header {
            headers.iter().any(|h| line.contains(h))
        } else {
            false
        };

        if header_matched {
            if !current.is_empty() {
                results.push(current.join("\n"));
            }
            active_header = true;
            current = vec![line.to_string()];
        } else if active_header {
            if is_header {
                results.push(current.join("\n"));
                active_header = false;
                current.clear();
            } else if !trimmed.is_empty() {
                current.push(line.to_string());
            }
        }
    }

    if !current.is_empty() {
        results.push(current.join("\n"));
    }

    results.join("\n\n")
}

/// Strips hallucinated tool-call markup from model output.
/// DeepSeek R1 can hallucinate DSML-style function call markup.
pub fn strip_hallucinated_tool_markup(content: &str) -> String {
    let mut result = content.to_string();

    // Handle DSML blocks: <|DSML|function_calls>...</|function_calls|>
    loop {
        if let Some(start) = result.find("<|DSML|") {
            if let Some(end_tag) = result[start..].find("<|/function_calls|>") {
                let end = start + end_tag + "<|/function_calls|>".len();
                result = result[..start].to_string() + &result[end..];
            } else if let Some(end_tag) = result[start..].find("|>") {
                let end = start + end_tag + 2;
                result = result[..start].to_string() + &result[end..];
            } else {
                break;
            }
        } else {
            break;
        }
    }

    // Handle [TOOL_CALL]...[/TOOL_CALL]
    loop {
        if let Some(start) = result.find("[TOOL_CALL]") {
            if let Some(end) = result[start..].find("[/TOOL_CALL]") {
                let end = start + end + "[/TOOL_CALL]".len();
                result = result[..start].to_string() + &result[end..];
            } else {
                break;
            }
        } else {
            break;
        }
    }

    // Handle <function_call>...</function_call>
    loop {
        if let Some(start) = result.find("<function_call>") {
            if let Some(end) = result[start..].find("</function_call>") {
                let end = start + end + "</function_call>".len();
                result = result[..start].to_string() + &result[end..];
            } else {
                break;
            }
        } else {
            break;
        }
    }

    result
}

/// Computes SHA-256 fingerprint of system + tools + fewshots.
/// Used to detect cache drift that would cause cache misses.
pub fn compute_prefix_fingerprint(
    system: &str,
    tool_schemas: &HashMap<String, String>,
    fewshots: &[Value],
) -> String {
    let mut data = serde_json::Map::new();
    data.insert("system".to_string(), Value::String(system.to_string()));
    data.insert("tools".to_string(), serde_json::to_value(tool_schemas).unwrap_or_default());
    data.insert("shots".to_string(), serde_json::to_value(fewshots).unwrap_or_default());

    let json_bytes = serde_json::to_vec(&data).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&json_bytes);
    let hash = hasher.finalize();
    hash[..16].iter().map(|b| format!("{:02x}", b)).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_for_tokens() {
        let long_text = "hello ".repeat(1000);
        let truncated = truncate_for_tokens(&long_text, 100);
        assert!(truncated.len() < long_text.len());
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn test_truncate_short_text() {
        let short = "hello world";
        let truncated = truncate_for_tokens(short, 1000);
        assert_eq!(truncated, "hello world");
    }

    #[test]
    fn test_count_tokens_bounded() {
        let english = "hello world this is a test";
        let cjk = "你好世界";

        let en_tokens = count_tokens_bounded(english);
        let cjk_tokens = count_tokens_bounded(cjk);

        assert!(en_tokens > 0);
        assert!(cjk_tokens > 0);
        assert_eq!(cjk_tokens, cjk.len() as usize); // each CJK char = 1 token
    }

    #[test]
    fn test_strip_hallucinated_tool_markup() {
        let input = "Some text <|DSML|function_calls><|invoke name=\"test\"/><|/function_calls|> more text";
        let cleaned = strip_hallucinated_tool_markup(input);
        assert!(!cleaned.contains("<|DSML|"));
        assert!(!cleaned.contains("function_calls"));
        assert!(cleaned.contains("Some text"));
        assert!(cleaned.contains("more text"));
    }

    #[test]
    fn test_strip_tool_call_markup() {
        let input = "text [TOOL_CALL]read_file(...)[/TOOL_CALL] more";
        let cleaned = strip_hallucinated_tool_markup(input);
        assert_eq!(cleaned, "text  more");
    }

    #[test]
    fn test_extract_pinned_constraints() {
        let prompt = r#"Some intro text.

# HIGH PRIORITY constraints
- Always use read_file before edit
- Never use rm -rf

# User memory
- User prefers Rust

# Other section
This should not be extracted."#;

        let extracted = extract_pinned_constraints(prompt);
        assert!(extracted.contains("HIGH PRIORITY constraints"));
        assert!(extracted.contains("User memory"));
        assert!(!extracted.contains("Other section"));
    }

    #[test]
    fn test_fix_tool_call_pairing_basic() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_1", "name": "read", "input": {}}
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "call_1", "text": "ok"}]
            }),
        ];

        let (result, dropped_calls, dropped_stray) = fix_tool_call_pairing(messages);
        assert_eq!(result.len(), 3);
        assert_eq!(dropped_calls, 0);
        assert_eq!(dropped_stray, 0);
    }

    #[test]
    fn test_fix_tool_call_pairing_unpaired() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_1", "name": "read", "input": {}}
                ]
            }),
        ];

        let (result, dropped_calls, dropped_stray) = fix_tool_call_pairing(messages);
        assert_eq!(result.len(), 1);
        assert_eq!(dropped_calls, 1);
        assert_eq!(dropped_stray, 0);
    }

    #[test]
    fn test_compute_prefix_fingerprint() {
        let system = "You are a helpful assistant.";
        let mut schemas = HashMap::new();
        schemas.insert("read_file".to_string(), "schema1".to_string());
        let fewshots = vec![serde_json::json!({"role": "user", "content": "example"})];

        let fp = compute_prefix_fingerprint(system, &schemas, &fewshots);
        assert_eq!(fp.len(), 32);

        // Same inputs should produce same fingerprint
        let fp2 = compute_prefix_fingerprint(system, &schemas, &fewshots);
        assert_eq!(fp, fp2);
    }

    #[test]
    fn test_shrink_oversized_tool_results() {
        let long_text = "line\n".repeat(500);
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": long_text}
            ]
        })];

        let (healed, tokens, chars) = shrink_oversized_tool_results_by_tokens(&mut messages, 100);
        assert_eq!(healed, 1);
        assert!(tokens > 0);
        assert!(chars > 0);
    }
}
