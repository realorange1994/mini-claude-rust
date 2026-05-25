//! Anthropic prompt caching (system_and_3 strategy).
//!
//! Reduces input token costs by ~75% on multi-turn conversations by caching
//! the conversation prefix. Uses 4 cache_control breakpoints:
//!   1. System prompt (stable across all turns)
//!   2-4. Last 3 non-system messages (rolling window)
//!
//! Also provides category-based cache break detection, cache_reference
//! mechanism, content normalization/hoisting for prefix stability,
//! boundary-cached system prompts with global scope, and pinned cache edits.

use std::collections::HashMap;
use std::sync::Mutex;

/// Apply system_and_3 caching strategy to messages.
/// Places up to 4 cache_control breakpoints: system + last 3 non-system messages.
pub fn apply_prompt_caching(messages: &mut [serde_json::Value], ttl: &str) {
    if messages.is_empty() {
        return;
    }

    // Normalize and hoist for prefix stability.
    for msg in messages.iter_mut() {
        normalize_content_to_array(msg);
        hoist_tool_result_cache(msg);
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

// ─── Configurable Cache Breakpoint Strategy ───────────────────────────────────

use serde_json::Value;

/// Controls the KV cache breakpoint strategy.
#[derive(Debug, Clone, Copy)]
pub struct CacheBreakpointConfig {
    pub max_breakpoints: usize,
    pub skip_cache_write: bool,
}

impl CacheBreakpointConfig {
    pub fn rolling() -> Self {
        Self { max_breakpoints: 2, skip_cache_write: false }
    }
    pub fn system_and_3() -> Self {
        Self { max_breakpoints: 4, skip_cache_write: false }
    }
}

impl Default for CacheBreakpointConfig {
    fn default() -> Self { Self::rolling() }
}

fn is_system_injected(msg: &Value) -> bool {
    let prefix = crate::context::SYSTEM_INJECTED_PREFIX;
    let content = msg.get("content");
    match content {
        Some(Value::String(s)) => s.starts_with(prefix),
        Some(Value::Array(arr)) if !arr.is_empty() => {
            arr.first()
                .and_then(|b| b.get("text").and_then(|t| t.as_str()))
                .is_some_and(|t| t.starts_with(prefix))
        }
        _ => false,
    }
}

fn strip_system_injected(msg: &mut Value) {
    let prefix = crate::context::SYSTEM_INJECTED_PREFIX;
    let content = msg.get("content").cloned();
    match content {
        Some(Value::String(s)) if s.starts_with(prefix) => {
            msg["content"] = Value::String(s[prefix.len()..].to_string());
        }
        Some(Value::Array(arr)) if !arr.is_empty() => {
            if let Some(text) = arr.first().and_then(|b| b.get("text").and_then(|t| t.as_str())) {
                if text.starts_with(prefix) {
                    let mut new_arr = arr.clone();
                    if let Some(first) = new_arr.first_mut() {
                        if let Some(obj) = first.as_object_mut() {
                            obj.insert("text".to_string(), Value::String(text[prefix.len()..].to_string()));
                        }
                    }
                    msg["content"] = Value::Array(new_arr);
                }
            }
        }
        _ => {}
    }
}

/// Applies configurable cache breakpoint strategy to messages.
pub fn apply_prompt_caching_with_config(
    messages: &mut [Value],
    config: CacheBreakpointConfig,
    ttl: &str,
) {
    if messages.is_empty() {
        return;
    }

    // Phase 1: Content normalization for prefix stability.
    // Convert string content to array format to prevent shape mutations
    // when cache breakpoints shift across turns.
    for msg in messages.iter_mut() {
        normalize_content_to_array(msg);
    }

    // Phase 2: Hoist cache_control from inner tool_result blocks to message level.
    // Prevents nested content shape mutations that break KV cache.
    for msg in messages.iter_mut() {
        hoist_tool_result_cache(msg);
    }

    let marker = match ttl {
        "1h" => serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
        _ => serde_json::json!({"type": "ephemeral"}),
    };

    let non_sys_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            let is_system = m.get("role").and_then(|v| v.as_str()) == Some("system");
            !is_system && !is_system_injected(m)
        })
        .map(|(i, _)| i)
        .collect();

    let count = config.max_breakpoints.min(non_sys_indices.len());
    let adjusted_count = if config.skip_cache_write && count > 1 {
        count - 1
    } else {
        count
    };
    let adjusted_start = non_sys_indices.len().saturating_sub(adjusted_count);

    for &idx in &non_sys_indices[adjusted_start..] {
        apply_cache_marker(&mut messages[idx], &marker);
    }

    // Phase 4: Add cache_reference to tool results before the cache breakpoint.
    if let Some(&last_idx) = non_sys_indices.last() {
        add_cache_reference(messages, last_idx);
    }

    for msg in messages.iter_mut() {
        strip_system_injected(msg);
    }
}

/// Apply cache_control to the system prompt block.
pub fn apply_system_prompt_caching(system: &mut Value, ttl: &str) {
    let marker = match ttl {
        "1h" => serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
        _ => serde_json::json!({"type": "ephemeral"}),
    };

    if let Some(arr) = system.as_array_mut() {
        if let Some(block) = arr.last_mut() {
            if let Some(obj) = block.as_object_mut() {
                obj.insert("cache_control".to_string(), marker);
            }
        }
    } else if system.as_str().is_some() {
        let s = system.as_str().unwrap();
        *system = serde_json::json!([{
            "type": "text",
            "text": s,
            "cache_control": marker
        }]);
    }
}

// ─── Content Normalization ────────────────────────────────────────────────

/// Convert string content to array format `[{"type":"text","text":"..."}]`.
/// Prevents string-to-array flips when cache breakpoints shift across turns,
/// which would break KV cache prefix stability.
pub fn normalize_content_to_array(msg: &mut Value) {
    let Some(content) = msg.get_mut("content") else { return };

    if content.is_string() {
        let text = content.as_str().unwrap_or("").to_string();
        *content = serde_json::json!([{
            "type": "text",
            "text": text
        }]);
    }
}

/// Hoist cache_control from inner text sub-blocks to the tool_result level.
/// Converts nested `tool_result.content = [{type:"text", text:"...", cache_control:{...}}]`
/// into `tool_result = {cache_control:{...}, content: "..."}` (flat string).
/// This prevents content shape mutations that break KV cache prefix stability.
pub fn hoist_tool_result_cache(msg: &mut Value) {
    let Some(content) = msg.get_mut("content") else { return };
    let Some(arr) = content.as_array_mut() else { return };

    for block in arr.iter_mut() {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }

        let Some(inner_content) = block.get_mut("content") else { continue };
        let Some(inner_arr) = inner_content.as_array_mut() else { continue };

        if inner_arr.len() == 1 {
            let inner = &inner_arr[0];
            if inner.get("cache_control").is_some()
                && inner.get("type").and_then(|t| t.as_str()) == Some("text")
            {
                let cache_ctrl = inner.get("cache_control").cloned();
                let text = inner.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();

                if let Some(cc) = cache_ctrl {
                    block["cache_control"] = cc;
                }
                block["content"] = serde_json::json!(text);
            }
        }
    }
}

/// Add cache_reference (using tool_use_id) to tool_result blocks that are
/// before the last cache_control marker. This maintains KV cache continuity
/// across turns by ensuring tool results reference their tool_use blocks.
pub fn add_cache_reference(messages: &mut [Value], last_cache_idx: usize) {
    for (i, msg) in messages.iter_mut().enumerate() {
        if i >= last_cache_idx {
            continue;
        }

        let Some(content) = msg.get_mut("content") else { continue };
        let Some(arr) = content.as_array_mut() else { continue };

        for block in arr.iter_mut() {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }
            if block.get("cache_reference").is_some() {
                continue;
            }
            if let Some(tool_use_id) = block.get("tool_use_id").and_then(|t| t.as_str()) {
                block["cache_reference"] = serde_json::json!(tool_use_id);
            }
        }
    }
}

/// Return the last content block in a message, for cache_control detection.
pub fn get_last_block_content(msg: &Value) -> Option<&Value> {
    let content = msg.get("content")?;
    let arr = content.as_array()?;
    arr.last()
}

// ─── Cache Change Categories ────────────────────────────────────────────

/// Categories of changes that can affect the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheChangeCategory {
    ToolResult,
    Thinking,
    Image,
    Pdf,
    Attachment,
    SystemPrompt,
    Compaction,
    Edit,
    UserMessage,
    ToolUse,
    Normalization,
    Other,
}

impl CacheChangeCategory {
    /// Parse a category from its string name.
    pub fn from_str_name(s: &str) -> CacheChangeCategory {
        match s {
            "tool_result" => CacheChangeCategory::ToolResult,
            "thinking" => CacheChangeCategory::Thinking,
            "image" => CacheChangeCategory::Image,
            "pdf" => CacheChangeCategory::Pdf,
            "attachment" => CacheChangeCategory::Attachment,
            "system_prompt" => CacheChangeCategory::SystemPrompt,
            "compaction" => CacheChangeCategory::Compaction,
            "edit" => CacheChangeCategory::Edit,
            "user_message" => CacheChangeCategory::UserMessage,
            "tool_use" => CacheChangeCategory::ToolUse,
            "normalization" => CacheChangeCategory::Normalization,
            _ => CacheChangeCategory::Other,
        }
    }

    /// Return the string name of this category.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            CacheChangeCategory::ToolResult => "tool_result",
            CacheChangeCategory::Thinking => "thinking",
            CacheChangeCategory::Image => "image",
            CacheChangeCategory::Pdf => "pdf",
            CacheChangeCategory::Attachment => "attachment",
            CacheChangeCategory::SystemPrompt => "system_prompt",
            CacheChangeCategory::Compaction => "compaction",
            CacheChangeCategory::Edit => "edit",
            CacheChangeCategory::UserMessage => "user_message",
            CacheChangeCategory::ToolUse => "tool_use",
            CacheChangeCategory::Normalization => "normalization",
            CacheChangeCategory::Other => "other",
        }
    }
}

/// Per-category token impact weight for category-based break prediction.
pub fn cache_change_weight(category: CacheChangeCategory) -> f64 {
    match category {
        CacheChangeCategory::Compaction => 1.0,
        CacheChangeCategory::SystemPrompt => 0.9,
        CacheChangeCategory::ToolResult => 0.7,
        CacheChangeCategory::Thinking => 0.6,
        CacheChangeCategory::Image => 0.8,
        CacheChangeCategory::Pdf => 0.8,
        CacheChangeCategory::Attachment => 0.6,
        CacheChangeCategory::Edit => 0.5,
        CacheChangeCategory::UserMessage => 0.3,
        CacheChangeCategory::ToolUse => 0.2,
        CacheChangeCategory::Normalization => 0.1,
        CacheChangeCategory::Other => 0.4,
    }
}

/// Infer the cache break dimension from change categories.
pub fn infer_dimension_from_changes(categories: &[CacheChangeCategory]) -> &'static str {
    if categories.is_empty() {
        return "unknown";
    }
    if categories.contains(&CacheChangeCategory::Compaction) { return "compaction"; }
    if categories.contains(&CacheChangeCategory::SystemPrompt) { return "system"; }
    if categories.contains(&CacheChangeCategory::ToolResult) { return "tool_result"; }
    if categories.contains(&CacheChangeCategory::Thinking) { return "thinking"; }
    if categories.contains(&CacheChangeCategory::Image) || categories.contains(&CacheChangeCategory::Pdf) { return "media"; }
    if categories.contains(&CacheChangeCategory::Edit) { return "edit"; }
    if categories.contains(&CacheChangeCategory::UserMessage) { return "user"; }
    if categories.contains(&CacheChangeCategory::ToolUse) { return "tool_use"; }
    "other"
}

// ─── Cache Break Detector ────────────────────────────────────────────────

/// Diagnostic capture for a cache break event.
#[derive(Debug, Clone)]
pub struct CacheBreak {
    pub token_drop: i64,
    pub pct_drop: f64,
    pub dimension: String,
    pub categories: Vec<CacheChangeCategory>,
    pub timestamp: std::time::Instant,
}

/// Category-based cache break detector with prediction + token-based fallback.
pub struct CacheBreakDetector {
    baseline: Mutex<Option<i64>>,
    pending_changes: Mutex<Vec<CacheChangeCategory>>,
    estimated_impact: Mutex<f64>,
    break_count: Mutex<usize>,
    latch: Mutex<bool>,
    post_compaction_guard: Mutex<bool>,
    break_events: Mutex<Vec<CacheBreak>>,
}

impl CacheBreakDetector {
    pub fn new() -> Self {
        Self {
            baseline: Mutex::new(None),
            pending_changes: Mutex::new(Vec::new()),
            estimated_impact: Mutex::new(0.0),
            break_count: Mutex::new(0),
            latch: Mutex::new(false),
            post_compaction_guard: Mutex::new(false),
            break_events: Mutex::new(Vec::new()),
        }
    }

    /// Record a change in a specific category.
    pub fn record_change(&self, category: CacheChangeCategory) {
        let weight = cache_change_weight(category);
        let mut pending = self.pending_changes.lock().unwrap_or_else(|e| e.into_inner());
        let mut impact = self.estimated_impact.lock().unwrap_or_else(|e| e.into_inner());
        pending.push(category);
        *impact += weight * 1000.0;
    }

    /// Update the baseline with cache_read tokens from a successful API response.
    pub fn update_baseline(&self, cache_read_tokens: i64) {
        let mut baseline = self.baseline.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending = self.pending_changes.lock().unwrap_or_else(|e| e.into_inner());
        let mut impact = self.estimated_impact.lock().unwrap_or_else(|e| e.into_inner());
        let mut latch = self.latch.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = self.post_compaction_guard.lock().unwrap_or_else(|e| e.into_inner());
        *baseline = Some(cache_read_tokens);
        pending.clear();
        *impact = 0.0;
        *latch = false;
        *guard = false;
    }

    /// Detect a cache break using category-based prediction + token-based fallback.
    pub fn detect_break(&self, current_cache_read: i64) -> Option<CacheBreak> {
        let baseline_val = *self.baseline.lock().unwrap_or_else(|e| e.into_inner());
        let latch_val = *self.latch.lock().unwrap_or_else(|e| e.into_inner());
        let guard_val = *self.post_compaction_guard.lock().unwrap_or_else(|e| e.into_inner());

        // Post-compaction: always detect
        if guard_val {
            let categories: Vec<_> = self.pending_changes.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let dimension = infer_dimension_from_changes(&categories).to_string();
            let token_drop = baseline_val.map(|b| b - current_cache_read).unwrap_or(0);
            self.record_break_internal(&categories, token_drop, &dimension);
            return Some(CacheBreak {
                token_drop,
                pct_drop: if let Some(b) = baseline_val { if b > 0 { (token_drop as f64 / b as f64) * 100.0 } else { 0.0 } } else { 0.0 },
                dimension,
                categories,
                timestamp: std::time::Instant::now(),
            });
        }

        if latch_val { return None; }
        let baseline = baseline_val?;

        // Category-based prediction
        let impact = *self.estimated_impact.lock().unwrap_or_else(|e| e.into_inner());
        if baseline > 0 && impact / (baseline as f64) > 0.10 {
            let categories: Vec<_> = self.pending_changes.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let dimension = infer_dimension_from_changes(&categories).to_string();
            let token_drop = baseline - current_cache_read;
            self.record_break_internal(&categories, token_drop, &dimension);
            return Some(CacheBreak {
                token_drop,
                pct_drop: (token_drop as f64 / baseline as f64) * 100.0,
                dimension,
                categories,
                timestamp: std::time::Instant::now(),
            });
        }

        // Token-based fallback
        let token_drop = baseline - current_cache_read;
        if baseline > 0 {
            let pct_drop = (token_drop as f64 / baseline as f64) * 100.0;
            if pct_drop > 10.0 && token_drop > 5000 {
                let categories: Vec<_> = self.pending_changes.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let dimension = infer_dimension_from_changes(&categories).to_string();
                self.record_break_internal(&categories, token_drop, &dimension);
                return Some(CacheBreak {
                    token_drop, pct_drop, dimension, categories,
                    timestamp: std::time::Instant::now(),
                });
            }
        }
        None
    }

    fn record_break_internal(&self, categories: &[CacheChangeCategory], token_drop: i64, dimension: &str) {
        let mut count = self.break_count.lock().unwrap_or_else(|e| e.into_inner());
        let mut latch = self.latch.lock().unwrap_or_else(|e| e.into_inner());
        let mut events = self.break_events.lock().unwrap_or_else(|e| e.into_inner());
        *count += 1;
        *latch = true;
        let baseline_val = *self.baseline.lock().unwrap_or_else(|e| e.into_inner());
        let pct_drop = if let Some(b) = baseline_val { if b > 0 { (token_drop as f64 / b as f64) * 100.0 } else { 0.0 } } else { 0.0 };
        events.push(CacheBreak {
            token_drop, pct_drop,
            dimension: dimension.to_string(),
            categories: categories.to_vec(),
            timestamp: std::time::Instant::now(),
        });
        if pct_drop > 10.0 && token_drop > 5000 {
            // Write diagnostic via the public method (self.write_diagnostic_file)
            let baseline_val = *self.baseline.lock().unwrap_or_else(|e| e.into_inner());
            let current_val = baseline_val.unwrap_or(0).saturating_sub(token_drop);
            let detail = format!("token_drop={}, pct_drop={:.1}%, dimension={}", token_drop, pct_drop, dimension);
            self.write_diagnostic_file(baseline_val, current_val, &detail);
        }
    }

    /// Write a cache break diagnostic file. Returns the file path if written.
    pub fn write_diagnostic_file(&self, baseline: Option<i64>, current: i64, detail: &str) -> String {
        let baseline_val = baseline.unwrap_or(0);
        let token_drop = baseline_val.saturating_sub(current).max(0);
        let pct_drop = if baseline_val > 0 { (token_drop as f64 / baseline_val as f64) * 100.0 } else { 0.0 };

        let categories = self.pending_changes.lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let dimension = infer_dimension_from_changes(&categories);
        let cat_names: Vec<&str> = categories.iter().map(|c| c.as_str_name()).collect();

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let content = format!(
            "Cache Break Diagnostic\n======================\n\
             Timestamp: {}\nToken drop: {}\nPercentage drop: {:.1}%\n\
             Dimension: {}\nCategories: {}\nDetail: {}\n",
            timestamp, token_drop, pct_drop, dimension, cat_names.join(", "), detail
        );
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join(format!("cache_break_{}.log", timestamp));
        if std::fs::write(&path, &content).is_ok() {
            return path.to_string_lossy().to_string();
        }
        String::new()
    }

    pub fn break_count(&self) -> usize {
        *self.break_count.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn reset_baseline(&self) {
        let mut baseline = self.baseline.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending = self.pending_changes.lock().unwrap_or_else(|e| e.into_inner());
        let mut impact = self.estimated_impact.lock().unwrap_or_else(|e| e.into_inner());
        *baseline = None;
        pending.clear();
        *impact = 0.0;
    }

    pub fn mark_post_compaction(&self) {
        let mut guard = self.post_compaction_guard.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending = self.pending_changes.lock().unwrap_or_else(|e| e.into_inner());
        *guard = true;
        pending.clear();
        pending.push(CacheChangeCategory::Compaction);
    }

    pub fn last_baseline(&self) -> Option<i64> {
        *self.baseline.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for CacheBreakDetector {
    fn default() -> Self { Self::new() }
}

// ─── System Prompt Caching with Scope ──────────────────────────────────

/// Format a system prompt as a cached block with global scope.
pub fn format_cached_system_prompt(system_text: &str) -> Value {
    serde_json::json!([{
        "type": "text",
        "text": system_text,
        "cache_control": {
            "type": "ephemeral",
            "scope": "global"
        }
    }])
}

/// Format a boundary-cached system prompt with separate caching scopes.
pub fn format_boundary_cached_system_prompt(system_text: &str, boundary_marker: &str) -> Value {
    if let Some(split_idx) = system_text.find(boundary_marker) {
        let static_part = &system_text[..split_idx];
        let dynamic_part = &system_text[split_idx..];
        serde_json::json!([
            {
                "type": "text",
                "text": static_part,
                "cache_control": {
                    "type": "ephemeral",
                    "scope": "global"
                }
            },
            {
                "type": "text",
                "text": dynamic_part,
                "cache_control": {
                    "type": "ephemeral"
                }
            }
        ])
    } else {
        format_cached_system_prompt(system_text)
    }
}

// ─── Pinned Cache Edits ─────────────────────────────────────────────────

/// A pinned cache edit: a tool_result block with cache_control that
/// should persist across API calls to preserve KV cache positions.
#[derive(Debug, Clone)]
pub struct PinnedCacheEdit {
    pub message_idx: usize,
    pub block_idx: usize,
    pub tool_use_id: String,
    pub content: Value,
}

/// Re-insert pinned cache edits at their original positions in the message stream.
pub fn apply_pinned_cache_edits(messages: &mut [Value], edits: &[PinnedCacheEdit]) {
    for edit in edits {
        if edit.message_idx >= messages.len() { continue; }
        let msg = &mut messages[edit.message_idx];
        let Some(content) = msg.get_mut("content") else { continue };
        let Some(arr) = content.as_array_mut() else { continue };
        if edit.block_idx >= arr.len() { continue; }
        let block = &mut arr[edit.block_idx];
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }
        if block.get("tool_use_id").and_then(|t| t.as_str()) != Some(&edit.tool_use_id) { continue; }
        if block.get("cache_control").is_none() {
            block["cache_control"] = edit.content.get("cache_control").cloned().unwrap_or(serde_json::json!({"type": "ephemeral"}));
        }
    }
}

/// Extract pinned cache edits from the current message stream.
pub fn extract_pinned_cache_edits(messages: &[Value]) -> Vec<PinnedCacheEdit> {
    let mut edits = Vec::new();
    for (msg_idx, msg) in messages.iter().enumerate() {
        let Some(content) = msg.get("content") else { continue };
        let Some(arr) = content.as_array() else { continue };
        for (block_idx, block) in arr.iter().enumerate() {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }
            if block.get("cache_control").is_none() { continue; }
            if let Some(tool_use_id) = block.get("tool_use_id").and_then(|t| t.as_str()) {
                edits.push(PinnedCacheEdit {
                    message_idx: msg_idx,
                    block_idx,
                    tool_use_id: tool_use_id.to_string(),
                    content: block.clone(),
                });
            }
        }
    }
    edits
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
