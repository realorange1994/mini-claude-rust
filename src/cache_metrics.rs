//! Cache metrics tracking for prompt cache hit/miss tokens per API call.
//!
//! Ported from `go:cache_metrics.go` (CacheMetrics, ReadTracker, TrimTrailingToolCalls).

use std::sync::{Arc, RwLock};

/// Tracks prompt cache hit/miss tokens per API call.
pub struct CacheMetrics {
    prompt_tokens: std::sync::atomic::AtomicI64,
    cache_hit_tokens: std::sync::atomic::AtomicI64,
    cache_miss_tokens: std::sync::atomic::AtomicI64,
    total_completion_tokens: std::sync::atomic::AtomicI64,
    turn_count: std::sync::atomic::AtomicI64,

    // Cumulative stats (persisted across session).
    cumulative_cache_hit_tokens: std::sync::atomic::AtomicI64,
    cumulative_cache_miss_tokens: std::sync::atomic::AtomicI64,
    cumulative_completion_tokens: std::sync::atomic::AtomicI64,
}

impl CacheMetrics {
    pub fn new() -> Self {
        Self {
            prompt_tokens: std::sync::atomic::AtomicI64::new(0),
            cache_hit_tokens: std::sync::atomic::AtomicI64::new(0),
            cache_miss_tokens: std::sync::atomic::AtomicI64::new(0),
            total_completion_tokens: std::sync::atomic::AtomicI64::new(0),
            turn_count: std::sync::atomic::AtomicI64::new(0),
            cumulative_cache_hit_tokens: std::sync::atomic::AtomicI64::new(0),
            cumulative_cache_miss_tokens: std::sync::atomic::AtomicI64::new(0),
            cumulative_completion_tokens: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// Record usage from an API response.
    pub fn record(&self, prompt_tokens: i64, cache_hit_tokens: i64, cache_miss_tokens: i64, completion_tokens: i64) {
        use std::sync::atomic::Ordering::Relaxed;

        self.prompt_tokens.store(prompt_tokens, Relaxed);
        self.cache_hit_tokens.store(cache_hit_tokens, Relaxed);
        self.cache_miss_tokens.store(cache_miss_tokens, Relaxed);
        self.total_completion_tokens.store(completion_tokens, Relaxed);
        self.turn_count.fetch_add(1, Relaxed);

        self.cumulative_cache_hit_tokens.fetch_add(cache_hit_tokens, Relaxed);
        self.cumulative_cache_miss_tokens.fetch_add(cache_miss_tokens, Relaxed);
        self.cumulative_completion_tokens.fetch_add(completion_tokens, Relaxed);
    }

    /// Returns the ratio of cache-hit tokens to total cache-eligible tokens.
    pub fn cache_hit_ratio(&self) -> f64 {
        use std::sync::atomic::Ordering::Relaxed;

        let hit = self.cache_hit_tokens.load(Relaxed);
        let miss = self.cache_miss_tokens.load(Relaxed);
        let total = hit + miss;
        if total == 0 {
            return 0.0;
        }
        hit as f64 / total as f64
    }

    /// Returns cumulative cache hit ratio across all turns.
    pub fn cumulative_cache_hit_ratio(&self) -> f64 {
        use std::sync::atomic::Ordering::Relaxed;

        let hit = self.cumulative_cache_hit_tokens.load(Relaxed);
        let miss = self.cumulative_cache_miss_tokens.load(Relaxed);
        let total = hit + miss;
        if total == 0 {
            return 0.0;
        }
        hit as f64 / total as f64
    }

    /// Estimates USD savings from cache hits.
    /// Uses DeepSeek pricing: cache hit ~0.0028 USD/M, cache miss ~0.14 USD/M.
    pub fn cache_savings_usd(&self) -> f64 {
        use std::sync::atomic::Ordering::Relaxed;

        const CACHE_HIT_RATE: f64 = 0.0028 / 1_000_000.0;
        const CACHE_MISS_RATE: f64 = 0.14 / 1_000_000.0;

        let hit_tokens = self.cumulative_cache_hit_tokens.load(Relaxed) as f64;
        let miss_tokens = self.cumulative_cache_miss_tokens.load(Relaxed) as f64;

        let hit_cost = hit_tokens * CACHE_HIT_RATE;
        let miss_cost = miss_tokens * CACHE_MISS_RATE;
        miss_cost - hit_cost
    }

    /// Returns current turn's cache statistics: (prompt, hit, miss, completion).
    pub fn stats(&self) -> (i64, i64, i64, i64) {
        use std::sync::atomic::Ordering::Relaxed;

        (
            self.prompt_tokens.load(Relaxed),
            self.cache_hit_tokens.load(Relaxed),
            self.cache_miss_tokens.load(Relaxed),
            self.total_completion_tokens.load(Relaxed),
        )
    }

    /// Returns cumulative statistics across all turns.
    pub fn cumulative_stats(&self) -> (i64, i64, i64) {
        use std::sync::atomic::Ordering::Relaxed;

        (
            self.cumulative_cache_hit_tokens.load(Relaxed),
            self.cumulative_cache_miss_tokens.load(Relaxed),
            self.cumulative_completion_tokens.load(Relaxed),
        )
    }

    pub fn turn_count(&self) -> i64 {
        self.turn_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl std::fmt::Display for CacheMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::sync::atomic::Ordering::Relaxed;

        let hit = self.cache_hit_tokens.load(Relaxed);
        let miss = self.cache_miss_tokens.load(Relaxed);
        let total = hit + miss;
        let ratio = if total > 0 {
            hit as f64 / total as f64
        } else {
            0.0
        };
        let completion = self.total_completion_tokens.load(Relaxed);

        write!(
            f,
            "cache: {:.1}% hit ({}/{} tokens), completion: {} tokens",
            ratio * 100.0, hit, total, completion
        )
    }
}

/// Tracks files read via read_file/list_directory.
/// Edit operations consult this before proceeding. Cleared on fold/compaction.
pub struct ReadTracker {
    inner: RwLock<ReadTrackerInner>,
}

struct ReadTrackerInner {
    read_files: std::collections::HashSet<String>,
    read_dirs: std::collections::HashSet<String>,
    epoch: usize,
}

impl ReadTracker {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(ReadTrackerInner {
                read_files: std::collections::HashSet::new(),
                read_dirs: std::collections::HashSet::new(),
                epoch: 0,
            }),
        }
    }

    /// Marks a file as read.
    pub fn mark_read(&self, path: &str) {
        let path = normalize_path(path);
        self.inner.write().unwrap().read_files.insert(path);
    }

    /// Marks a directory as read (via list_directory).
    pub fn mark_dir_read(&self, path: &str) {
        let path = normalize_path(path);
        self.inner.write().unwrap().read_dirs.insert(path);
    }

    /// Returns true if the file was read in the current epoch.
    pub fn was_read(&self, path: &str) -> bool {
        let path = normalize_path(path);
        self.inner.read().unwrap().read_files.contains(&path)
    }

    /// Returns true if the directory was listed in the current epoch.
    pub fn was_dir_read(&self, path: &str) -> bool {
        let path = normalize_path(path);
        self.inner.read().unwrap().read_dirs.contains(&path)
    }

    /// Clears all tracked reads (called after compaction).
    pub fn reset(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.read_files.clear();
        inner.read_dirs.clear();
        inner.epoch += 1;
    }

    /// Returns the current epoch number.
    pub fn epoch(&self) -> usize {
        self.inner.read().unwrap().epoch
    }
}

/// Normalizes a file path for comparison.
fn normalize_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.trim_end_matches('/');
    path.to_lowercase()
}

/// Drops unpaired assistant messages with tool_calls before generating a forced summary.
/// Returns true if a message was trimmed.
pub fn trim_trailing_tool_calls(messages: &mut Vec<serde_json::Value>) -> bool {
    if messages.is_empty() {
        return false;
    }

    let last_idx = messages.len() - 1;
    let last_msg = &messages[last_idx];

    // Check if it's an assistant message with tool_calls.
    let is_assistant = last_msg
        .get("role")
        .and_then(|v| v.as_str())
        .map(|s| s == "assistant")
        .unwrap_or(false);

    if !is_assistant {
        return false;
    }

    // Check for tool_calls in content blocks.
    let has_tool_calls = last_msg
        .get("content")
        .and_then(|v| v.as_array())
        .map(|blocks| {
            blocks.iter().any(|b| {
                b.get("type")
                    .and_then(|t| t.as_str())
                    .map(|s| s == "tool_use")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if !has_tool_calls {
        return false;
    }

    // Check if there's a matching tool result.
    let has_tool_result = messages.iter().rev().skip(1).any(|msg| {
        msg.get("role")
            .and_then(|v| v.as_str())
            .map(|s| s == "tool")
            .unwrap_or(false)
    });

    // If there's no tool result, trim this message.
    if !has_tool_result {
        messages.pop();
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_metrics_basic() {
        let metrics = CacheMetrics::new();
        metrics.record(1000, 800, 200, 500);

        assert_eq!(metrics.turn_count(), 1);
        assert!((metrics.cache_hit_ratio() - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_cache_metrics_cumulative() {
        let metrics = CacheMetrics::new();
        metrics.record(1000, 800, 200, 500);
        metrics.record(1000, 900, 100, 500);

        assert_eq!(metrics.turn_count(), 2);
        assert!((metrics.cumulative_cache_hit_ratio() - 1700.0 / 1900.0).abs() < 0.01);
    }

    #[test]
    fn test_cache_savings_usd() {
        let metrics = CacheMetrics::new();
        metrics.record(1_000_000, 900_000, 100_000, 500_000);

        let savings = metrics.cache_savings_usd();
        // Should be positive since cache hits save money
        assert!(savings > 0.0);
    }

    #[test]
    fn test_read_tracker_epoch() {
        let tracker = ReadTracker::new();
        tracker.mark_read("foo.rs");
        tracker.mark_dir_read("src/");

        assert!(tracker.was_read("foo.rs"));
        assert!(tracker.was_dir_read("src/"));
        assert!(!tracker.was_read("bar.rs"));

        tracker.reset();
        assert!(!tracker.was_read("foo.rs"));
        assert!(tracker.epoch() == 1);
    }

    #[test]
    fn test_trim_trailing_tool_calls() {
        // Assistant with tool_use but no matching tool result.
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_1", "name": "read", "input": {}}
                ]
            }),
        ];

        let trimmed = trim_trailing_tool_calls(&mut messages);
        assert!(trimmed);
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_trim_trailing_tool_calls_with_result() {
        // Assistant with tool_use and matching tool result.
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_1", "name": "read", "input": {}}
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "ok"}]
            }),
        ];

        let trimmed = trim_trailing_tool_calls(&mut messages);
        assert!(!trimmed);
        assert_eq!(messages.len(), 3);
    }
}
