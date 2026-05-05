//! Anthropic prompt caching (system_and_3 strategy).
//!
//! Reduces input token costs by ~75% on multi-turn conversations by caching
//! the conversation prefix. Uses 4 cache_control breakpoints:
//!   1. System prompt (stable across all turns)
//!   2-4. Last 3 non-system messages (rolling window)

/// Apply system_and_3 caching strategy to messages.
/// Places up to 4 cache_control breakpoints: system + last 3 non-system messages.
pub fn apply_prompt_caching(messages: &mut [serde_json::Value], ttl: &str) {
    if messages.is_empty() {
        return;
    }

    let marker = match ttl {
        "1h" => serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
        _ => serde_json::json!({"type": "ephemeral"}),
    };

    let mut breakpoints_used = 0;

    // 1. Cache the system prompt (first message if system role)
    if !messages.is_empty() && messages[0].get("role").and_then(|v| v.as_str()) == Some("system") {
        apply_cache_marker(&mut messages[0], &marker);
        breakpoints_used += 1;
    }

    // 2. Cache the last N non-system messages (up to 4-total breakpoints)
    let remaining = 4 - breakpoints_used;
    let non_sys_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(|v| v.as_str()) != Some("system"))
        .map(|(i, _)| i)
        .collect();

    let start = non_sys_indices.len().saturating_sub(remaining);
    for &idx in &non_sys_indices[start..] {
        apply_cache_marker(&mut messages[idx], &marker);
    }
}

/// Apply cache_control to the system prompt block.
pub fn cache_system_prompt(system: &mut serde_json::Value) {
    if let Some(arr) = system.as_array_mut() {
        if let Some(first) = arr.first_mut() {
            if let Some(obj) = first.as_object_mut() {
                obj.insert(
                    "cache_control".to_string(),
                    serde_json::json!({"type": "ephemeral"}),
                );
            }
        }
    }
}

/// Add cache_control to a single message, handling all formats.
fn apply_cache_marker(msg: &mut serde_json::Value, marker: &serde_json::Value) {
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

    // tool role: cache_control goes at message level
    if role == "tool" {
        msg["cache_control"] = marker.clone();
        return;
    }

    let content = msg.get("content").cloned();

    match content {
        None => {
            msg["cache_control"] = marker.clone();
        }
        Some(serde_json::Value::String(s)) if s.is_empty() => {
            msg["cache_control"] = marker.clone();
        }
        Some(serde_json::Value::String(s)) => {
            msg["content"] = serde_json::json!([
                {"type": "text", "text": s, "cache_control": marker}
            ]);
        }
        Some(serde_json::Value::Array(arr)) if !arr.is_empty() => {
            if let Some(last) = msg["content"].as_array_mut().and_then(|a| a.last_mut()) {
                if let Some(obj) = last.as_object_mut() {
                    obj.insert("cache_control".to_string(), marker.clone());
                }
            }
        }
        _ => {
            msg["cache_control"] = marker.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_caches_system_and_last_3() {
        let mut messages = vec![
            serde_json::json!({"role": "system", "content": "You are helpful"}),
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "Hi"}),
            serde_json::json!({"role": "user", "content": "Bye"}),
            serde_json::json!({"role": "assistant", "content": "See ya"}),
        ];

        apply_prompt_caching(&mut messages, "5m");

        // System should have cache_control
        assert!(messages[0].get("content").unwrap().as_array().unwrap()[0]
            .get("cache_control").is_some());

        // Last 3 non-system messages (indices 2,3,4) should have cache_control
        for i in 2..=4 {
            assert!(
                messages[i].get("cache_control").is_some()
                    || messages[i].get("content").unwrap().as_array().unwrap().last().unwrap()
                        .get("cache_control").is_some(),
                "message {} should have cache_control", i
            );
        }

        // First non-system (index 1) should NOT have cache_control
        let msg1 = &messages[1];
        let has_cache = msg1.get("cache_control").is_some()
            || msg1.get("content").and_then(|c| c.as_array())
                .and_then(|a| a.last()).and_then(|b| b.get("cache_control")).is_some();
        assert!(!has_cache, "first user message should not have cache_control");
    }

    #[test]
    fn test_empty_messages() {
        let mut messages: Vec<serde_json::Value> = vec![];
        apply_prompt_caching(&mut messages, "5m");
        assert!(messages.is_empty());
    }
}

#[cfg(test)]
mod prompt_caching_extra_tests {
    use super::*;

    /// Empty vec should remain unchanged after applying prompt caching.
    #[test]
    fn test_apply_prompt_caching_empty() {
        let mut messages: Vec<serde_json::Value> = vec![];
        apply_prompt_caching(&mut messages, "5m");
        assert!(messages.is_empty(), "empty vec should stay empty");
    }

    /// Fewer than 4 messages: system + all non-system messages get cache markers.
    #[test]
    fn test_apply_prompt_caching_short() {
        let mut messages = vec![
            serde_json::json!({"role": "system", "content": "You are helpful"}),
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "Hi"}),
        ];

        apply_prompt_caching(&mut messages, "5m");

        // System message (index 0): content turned into array, last block has cache_control
        let sys_content = messages[0].get("content").unwrap().as_array().unwrap();
        assert!(
            sys_content.last().unwrap().get("cache_control").is_some(),
            "system message should have cache_control"
        );

        // User (index 1) and assistant (index 2): both should have cache_control
        for i in 1..=2 {
            let has_cache = messages[i].get("cache_control").is_some()
                || messages[i]
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.last())
                    .and_then(|b| b.get("cache_control"))
                    .is_some();
            assert!(has_cache, "message at index {} should have cache_control", i);
        }
    }

    /// 6 messages: system + last 3 non-system get markers, middle ones don't.
    #[test]
    fn test_apply_prompt_caching_long() {
        let mut messages = vec![
            serde_json::json!({"role": "system", "content": "You are helpful"}),   // 0 - cached
            serde_json::json!({"role": "user", "content": "Hello"}),                // 1 - NOT cached
            serde_json::json!({"role": "assistant", "content": "Hi"}),              // 2 - NOT cached
            serde_json::json!({"role": "user", "content": "How are you?"}),         // 3 - cached
            serde_json::json!({"role": "assistant", "content": "Fine"}),            // 4 - cached
            serde_json::json!({"role": "user", "content": "Thanks"}),               // 5 - cached
        ];

        apply_prompt_caching(&mut messages, "5m");

        // System (0) and last 3 non-system (3, 4, 5) should have cache_control
        let cached_indices = [0usize, 3, 4, 5];
        for i in cached_indices {
            let has_cache = messages[i].get("cache_control").is_some()
                || messages[i]
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.last())
                    .and_then(|b| b.get("cache_control"))
                    .is_some();
            assert!(has_cache, "message at index {} should have cache_control", i);
        }

        // Middle messages (1, 2) should NOT have cache_control
        let uncached_indices = [1usize, 2];
        for i in uncached_indices {
            let has_cache = messages[i].get("cache_control").is_some()
                || messages[i]
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.last())
                    .and_then(|b| b.get("cache_control"))
                    .is_some();
            assert!(!has_cache, "message at index {} should NOT have cache_control", i);
        }
    }

    /// cache_system_prompt creates a text block with cache_control inside.
    #[test]
    fn test_cache_system_prompt() {
        let mut system = serde_json::json!([
            {"type": "text", "text": "You are helpful"}
        ]);

        cache_system_prompt(&mut system);

        let arr = system.as_array().unwrap();
        let first = &arr[0];
        let cc = first.get("cache_control").unwrap();
        assert_eq!(cc["type"], "ephemeral");
        assert!(cc.get("ttl").is_none(), "default marker should not have ttl");
    }

    /// With 1h TTL, apply_prompt_caching includes the ttl field in markers.
    #[test]
    fn test_cache_system_prompt_ttl() {
        let mut messages = vec![
            serde_json::json!({"role": "system", "content": "You are helpful"}),
        ];

        apply_prompt_caching(&mut messages, "1h");

        // System message content should have cache_control with ttl field
        let sys_content = messages[0].get("content").unwrap().as_array().unwrap();
        let cc = sys_content
            .last()
            .unwrap()
            .get("cache_control")
            .unwrap();
        assert_eq!(cc["type"], "ephemeral");
        assert_eq!(cc["ttl"], "1h", "1h TTL should include ttl field");
    }
}
