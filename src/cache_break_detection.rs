//! Cache break detection for conversation messages.
//!
//! Detects when cached context is broken by detecting
//! mismatches between expected and actual message sequences.

use std::collections::HashMap;

/// Result of a cache break check
#[derive(Debug, Clone)]
pub struct CacheBreakResult {
    pub is_broken: bool,
    pub reason: String,
    pub cached_count: usize,
    pub expected_count: usize,
}

/// Check if a conversation's cache is broken by comparing
/// the cached prefix with the current messages.
pub fn check_cache_break(
    cached_messages: &[serde_json::Value],
    current_messages: &[serde_json::Value],
) -> CacheBreakResult {
    let cached_count = cached_messages.len();
    let expected_count = current_messages.len();

    // If we have more current messages than cached, that's expected (new messages)
    if expected_count <= cached_count {
        // Check if the prefix matches
        for i in 0..expected_count.min(cached_count) {
            let cached = &cached_messages[i];
            let current = &current_messages[i];
            if cached != current {
                return CacheBreakResult {
                    is_broken: true,
                    reason: format!("Message {} differs between cached and current", i),
                    cached_count,
                    expected_count,
                };
            }
        }
    }

    // Check if any cached messages were modified
    let min_len = cached_count.min(expected_count);
    for i in 0..min_len {
        if cached_messages[i] != current_messages[i] {
            return CacheBreakResult {
                is_broken: true,
                reason: format!("Cache break at message index {}", i),
                cached_count,
                expected_count,
            };
        }
    }

    CacheBreakResult {
        is_broken: false,
        reason: String::new(),
        cached_count,
        expected_count,
    }
}

/// Estimate the number of tokens that can be served from cache.
pub fn estimate_cache_hit_tokens(
    cached_messages: &[serde_json::Value],
    current_messages: &[serde_json::Value],
    avg_tokens_per_message: i64,
) -> i64 {
    let mut hit_messages = 0;
    let min_len = cached_messages.len().min(current_messages.len());
    for i in 0..min_len {
        if cached_messages[i] == current_messages[i] {
            hit_messages += 1;
        } else {
            break;
        }
    }
    hit_messages as i64 * avg_tokens_per_message
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_cache_not_broken() {
        let cached = vec![json!("a"), json!("b"), json!("c")];
        let current = vec![json!("a"), json!("b"), json!("c"), json!("d")];
        let result = check_cache_break(&cached, &current);
        assert!(!result.is_broken);
    }

    #[test]
    fn test_cache_broken_modified() {
        let cached = vec![json!("a"), json!("b"), json!("c")];
        let current = vec![json!("a"), json!("X"), json!("c")];
        let result = check_cache_break(&cached, &current);
        assert!(result.is_broken);
    }

    #[test]
    fn test_cache_hit_tokens() {
        let cached = vec![json!("a"), json!("b"), json!("c")];
        let current = vec![json!("a"), json!("b"), json!("d")];
        let tokens = estimate_cache_hit_tokens(&cached, &current, 100);
        assert_eq!(tokens, 200); // 2 matching messages * 100 tokens
    }
}
