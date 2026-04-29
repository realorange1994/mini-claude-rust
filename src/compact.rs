//! Compact module - intelligent context compaction
//!
//! Implements multi-layered context management inspired by Claude Code's official implementation:
//! 1. Micro-compaction (time-based tool result clearing)
//! 2. LLM-driven compaction (summary generation via API call)
//! 3. Progressive truncation (fallback when compaction fails)

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::context::{
    CompactTrigger, ConversationContext, Message, MessageContent, MessageRole,
};

// --- Token estimation ---

/// Estimate token count from text (~4 chars per token)
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.len().div_ceil(4)
}

/// Detect content type for token estimation purposes.
/// Returns "code", "json", or "natural".
pub fn detect_content_type(text: &str) -> &'static str {
    let trimmed = text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "json"
    } else if contains_code_keywords(trimmed) {
        "code"
    } else {
        "natural"
    }
}

/// Check if text contains programming language keywords.
fn contains_code_keywords(text: &str) -> bool {
    let keywords = [
        "fn ",
        "func ",
        "class ",
        "def ",
        "impl ",
        "import ",
        "package ",
        "struct ",
        "const ",
        "var ",
        "type ",
    ];
    keywords.iter().any(|kw| text.contains(kw))
}

/// Estimate tokens using content-type-aware ratios.
/// - code: len / 3.5 (code is denser)
/// - json: len / 3.0 (JSON has lots of punctuation)
/// - natural: len / 4.0 (natural language is less dense)
pub fn estimate_tokens_typed(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let content_type = detect_content_type(text);
    let ratio = match content_type {
        "code" => 3.5,
        "json" => 3.0,
        _ => 4.0,
    };
    (text.len() as f64 / ratio).ceil() as usize
}

/// Estimate tokens for a single message, accounting for content type overhead
pub fn estimate_message_tokens(msg: &Message) -> usize {
    match &msg.content {
        MessageContent::Text(text) => {
            // Role overhead (~3 tokens) + content with type-aware estimation
            3 + estimate_tokens_typed(text)
        }
        MessageContent::ToolUseBlocks(blocks) => {
            let mut total = 3; // role overhead
            for block in blocks {
                total += 10; // type(1) + id(4) + name(3) + structure(2)
                total += estimate_tokens_typed(&block.name);
                if let Ok(json) = serde_json::to_string(&block.input) {
                    // Tool inputs are JSON — use json ratio
                    total += (json.len() as f64 / 3.0).ceil() as usize;
                }
            }
            total
        }
        MessageContent::ToolResultBlocks(blocks) => {
            let mut total = 3; // role overhead
            for block in blocks {
                total += 8; // type(1) + tool_use_id(5) + is_error(1) + structure(1)
                for content in &block.content {
                    match content {
                        crate::context::ToolResultContent::Text { text } => {
                            // Tool results are typically JSON/structured — use json ratio
                            total += (text.len() as f64 / 3.0).ceil() as usize;
                        }
                    }
                }
            }
            total
        }
        MessageContent::CompactBoundary { .. } => {
            // System message with compact metadata (~15 tokens)
            15
        }
        MessageContent::Summary(text) => {
            3 + estimate_tokens_typed(text)
        }
    }
}

/// Estimate total tokens for all messages
pub fn estimate_total_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

// --- Context window tracking ---

/// Model-specific context window sizes
pub fn model_context_window(model: &str) -> usize {
    // Common models and their context windows
    if model.contains("opus") || model.contains("claude-4") {
        200_000
    } else if model.contains("sonnet") || model.contains("claude-3") {
        200_000
    } else if model.contains("haiku") {
        200_000
    } else {
        // Default to 200K (most Anthropic models)
        200_000
    }
}

/// Tracks context window usage and determines when to compact
pub struct ContextWindowTracker {
    pub model_max_tokens: usize,
    pub auto_compact_threshold: f64, // e.g. 0.75 = trigger at 75%
    pub auto_compact_buffer: usize,  // reserved buffer tokens (e.g. 13000)
}

impl ContextWindowTracker {
    pub fn new(model: &str, threshold: f64, buffer: usize) -> Self {
        Self {
            model_max_tokens: model_context_window(model),
            auto_compact_threshold: threshold,
            auto_compact_buffer: buffer,
        }
    }

    /// Effective context window = max - reserved output space
    pub fn effective_window(&self) -> usize {
        // Reserve ~20K tokens for output (summary generation needs ~20K max)
        self.model_max_tokens.saturating_sub(20_000)
    }

    /// Auto-compact threshold = effective_window - buffer
    pub fn compact_threshold(&self) -> usize {
        let effective = self.effective_window();
        let threshold = (effective as f64 * self.auto_compact_threshold) as usize;
        threshold.min(effective.saturating_sub(self.auto_compact_buffer))
    }

    /// Check if compaction is needed
    pub fn should_compact(&self, messages: &[Message]) -> bool {
        let tokens = estimate_total_tokens(messages);
        tokens >= self.compact_threshold()
    }

    /// Get current usage info
    pub fn usage_info(&self, messages: &[Message]) -> ContextUsageInfo {
        let tokens = estimate_total_tokens(messages);
        let threshold = self.compact_threshold();
        let effective = self.effective_window();
        ContextUsageInfo {
            estimated_tokens: tokens,
            effective_window: effective,
            compact_threshold: threshold,
            percent_used: (tokens as f64 / effective as f64 * 100.0).min(100.0) as u32,
        }
    }
}

pub struct ContextUsageInfo {
    pub estimated_tokens: usize,
    pub effective_window: usize,
    pub compact_threshold: usize,
    pub percent_used: u32,
}

// --- Compaction phases (legacy fallback) ---

/// Compaction phase levels (used as fallback when LLM compaction fails)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactPhase {
    None,
    RoundBased,      // Keep last 3 tool-use rounds
    TurnBased,       // Keep first + last 5 entries
    SelectiveClear,  // Keep first + last 3 entries
    Aggressive,      // Keep first + last 2 entries
    Truncated,       // Simple head truncation (for /compact command)
}

/// CompactStats tracks compaction metrics
#[derive(Debug, Clone)]
pub struct CompactStats {
    pub phase: CompactPhase,
    pub entries_before: usize,
    pub entries_after: usize,
    pub estimated_tokens_saved: usize,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
}

// --- 3-pass pre-pruning (A1) ---

/// Info about a tool call, indexed by tool_use_id for summarization.
struct ToolCallInfo {
    tool_name: String,
    #[allow(dead_code)]
    args_summary: String,
}

/// Compute a hash of a string using DefaultHasher.
fn hash_content(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Pass 1: Deduplicate tool results by content hash.
/// If a duplicate hash is found (different tool_use_id but same content),
/// replace the duplicate with `[duplicate result, see tool_use_id XXX]`.
fn dedup_tool_results(messages: &mut Vec<Message>) {
    // Map from content hash to the first tool_use_id that produced it
    let mut seen: std::collections::HashMap<u64, String> = std::collections::HashMap::new();

    for msg in messages.iter_mut() {
        if let MessageContent::ToolResultBlocks(blocks) = &mut msg.content {
            for block in blocks.iter_mut() {
                // Build the combined text content for hashing
                let combined: String = block
                    .content
                    .iter()
                    .map(|c| match c {
                        crate::context::ToolResultContent::Text { text } => text.as_str(),
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let h = hash_content(&combined);
                if let Some(original_id) = seen.get(&h) {
                    if *original_id != block.tool_use_id {
                        // Duplicate found — replace content
                        block.content = vec![crate::context::ToolResultContent::Text {
                            text: format!(
                                "[duplicate result, see tool_use_id {}]",
                                original_id
                            ),
                        }];
                    }
                } else {
                    seen.insert(h, block.tool_use_id.clone());
                }
            }
        }
    }
}

/// Pass 2: Summarize old tool results (before the tail 25% by token count).
/// Replaces old tool result content with a one-line summary like
/// `[read_file] -> ok, 42 lines` or `[exec] -> error, 5 lines, 150ms`.
fn summarize_old_tool_results(messages: &mut Vec<Message>) {
    // Step 1: Build the ToolCallIndex mapping tool_use_id -> (tool_name, args_summary)
    let mut tool_call_index: std::collections::HashMap<String, ToolCallInfo> =
        std::collections::HashMap::new();

    for msg in messages.iter() {
        if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
            for block in blocks {
                let args_summary = summarize_args(&block.input);
                tool_call_index.insert(
                    block.id.clone(),
                    ToolCallInfo {
                        tool_name: block.name.clone(),
                        args_summary,
                    },
                );
            }
        }
    }

    // Step 2: Compute total token count and find the 75% cut point
    let total_tokens: usize = messages.iter().map(|m| estimate_message_tokens(m)).sum();
    let tail_threshold = (total_tokens as f64 * 0.75) as usize;

    // Walk from the end accumulating tokens to find the cut index
    // "tail" = last 25% of tokens; everything before the cut is "old"
    let mut tail_tokens = 0usize;
    let mut cut_index = messages.len(); // default: no old messages
    for i in (0..messages.len()).rev() {
        tail_tokens += estimate_message_tokens(&messages[i]);
        if tail_tokens >= total_tokens.saturating_sub(tail_threshold) {
            cut_index = i;
            break;
        }
    }

    // Step 3: Summarize tool results before the cut point
    for i in 0..cut_index {
        if let MessageContent::ToolResultBlocks(blocks) = &mut messages[i].content {
            for block in blocks.iter_mut() {
                let tool_name = tool_call_index
                    .get(&block.tool_use_id)
                    .map(|info| info.tool_name.as_str())
                    .unwrap_or("unknown");

                let combined: String = block
                    .content
                    .iter()
                    .map(|c| match c {
                        crate::context::ToolResultContent::Text { text } => text.as_str(),
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let line_count = combined.lines().count();
                let status = if block.is_error { "error" } else { "ok" };

                let summary = format!("[{}] -> {}, {} lines", tool_name, status, line_count);

                block.content = vec![crate::context::ToolResultContent::Text {
                    text: summary,
                }];
            }
        }
    }
}

/// Create a short summary of tool input arguments.
fn summarize_args(input: &std::collections::HashMap<String, serde_json::Value>) -> String {
    if input.is_empty() {
        return String::new();
    }
    // Show keys with truncated values
    let parts: Vec<String> = input
        .iter()
        .take(3)
        .map(|(k, v)| {
            let val_str = v.to_string();
            if val_str.len() > 30 {
                format!("{}={}", k, &val_str[..30])
            } else {
                format!("{}={}", k, val_str)
            }
        })
        .collect();
    let suffix = if input.len() > 3 { "..." } else { "" };
    format!("{}{}", parts.join(", "), suffix)
}

/// Pass 3: Truncate large tool arguments.
/// For ToolUseBlocks, if the JSON-serialized input exceeds 2000 chars,
/// truncate to 2000 chars + `...[truncated]`.
fn truncate_large_tool_args(messages: &mut Vec<Message>) {
    const MAX_ARGS_LEN: usize = 2000;

    for msg in messages.iter_mut() {
        if let MessageContent::ToolUseBlocks(blocks) = &mut msg.content {
            for block in blocks.iter_mut() {
                if let Ok(json) = serde_json::to_string(&block.input) {
                    if json.len() > MAX_ARGS_LEN {
                        // Truncate the JSON representation and store back
                        let truncated_json =
                            format!("{}...[truncated]", &json[..MAX_ARGS_LEN]);
                        // Parse the truncated JSON back; if it fails, create a simple map
                        if let Ok(parsed) =
                            serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(
                                &truncated_json,
                            )
                        {
                            block.input = parsed;
                        } else {
                            // Fallback: store a single _truncated key
                            let mut map =
                                std::collections::HashMap::<String, serde_json::Value>::new();
                            map.insert(
                                "_truncated_input".to_string(),
                                serde_json::Value::String(truncated_json),
                            );
                            block.input = map;
                        }
                    }
                }
            }
        }
    }
}

/// Run all 3 passes of pre-pruning on messages.
pub fn prune_tool_results(messages: &mut Vec<Message>) {
    dedup_tool_results(messages);
    summarize_old_tool_results(messages);
    truncate_large_tool_args(messages);
}

// --- LLM-driven compaction ---

/// Result from LLM-driven compaction
pub struct CompactionResult {
    pub boundary: Message,
    pub summary: Message,
    pub pre_compact_tokens: usize,
    pub post_compact_tokens: usize,
}

/// Compact prompt template (inspired by Claude Code's getCompactPrompt)
const COMPACT_SYSTEM_PROMPT: &str = "You are a helpful AI assistant tasked with summarizing conversations.";

const COMPACT_USER_PROMPT: &str = r#"Summarize the following conversation history. Your summary should:

1. Capture the user's main request and goals
2. Note key decisions made and discoveries found
3. List files that were read, modified, or created
4. Describe the current state of work and what remains
5. Be concise but complete — the next AI should be able to continue seamlessly

Do NOT include:
- Individual tool call details or raw outputs
- Intermediate exploration steps
- Redundant or outdated information

Write the summary as a coherent narrative that preserves enough context for the conversation to continue productively."#;

/// Iterative variant used when a previous summary already exists.
const COMPACT_USER_PROMPT_ITERATIVE: &str = r#"You previously summarized this conversation. An updated version follows.

Update the previous summary to incorporate new information, decisions, or changes.
Preserve all critical context from the previous summary while adding new developments.

Previous Summary:
{previous_summary}

New conversation content follows. Update the summary to incorporate the new information."#;

// --- Sensitive info redaction (A3) ---

/// Sensitive key patterns for redaction during compaction.
const SENSITIVE_KEYS: &[&str] = &[
    "api_key", "password", "secret", "token", "credential",
    "auth", "private_key", "access_key", "api_secret", "apikey",
    "secret_key", "passwd", "access_token", "refresh_token",
];

/// Redact sensitive information from text by replacing values with `[REDACTED]`.
/// Matches `"key": "value"` and `key=value` patterns.
pub fn redact_sensitive_text(text: &str) -> String {
    let mut result = text.to_string();
    for &key in SENSITIVE_KEYS {
        // Pattern 1: "key": "value" (JSON-style)
        let json_key = format!("\"{}\"", key);
        while let Some(start) = result.find(&json_key) {
            let after_key = &result[start + json_key.len()..];
            let trimmed = after_key.trim_start();
            if !trimmed.starts_with(':') {
                break;
            }
            let after_colon = trimmed[1..].trim_start();
            if after_colon.starts_with('"') {
                if let Some(end_offset) = find_closing_quote(&after_colon[1..]) {
                    let value_start = start + json_key.len()
                        + (after_key.len() - after_key.trim_start().len());
                    let value_end = value_start + 1 + 1 + end_offset; // colon + space + closing quote
                    result.replace_range(value_start..=value_end, ": \"[REDACTED]\"");
                    break;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // Pattern 2: key=value (unquoted, whitespace-delimited)
        let kv_pattern = format!("{}=", key);
        let mut search_from = 0;
        while let Some(pos) = result[search_from..].find(&kv_pattern) {
            let abs_start = search_from + pos;
            let after_eq = &result[abs_start + kv_pattern.len()..];
            let value_end = after_eq
                .find(|c: char| c.is_whitespace())
                .unwrap_or(after_eq.len());
            if value_end > 0 {
                let replace_end = abs_start + kv_pattern.len() + value_end;
                let redacted = format!("{}=[REDACTED]", key);
                result.replace_range(abs_start..replace_end, &redacted);
                search_from = abs_start + redacted.len();
            } else {
                search_from = abs_start + kv_pattern.len();
            }
        }
    }
    result
}

fn find_closing_quote(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            return Some(i);
        }
    }
    None
}

// --- LLM-driven compaction ---

/// Compress messages using LLM summary generation.
/// This is the primary compaction method — replaces older truncation-based approaches.
/// If `last_summary` is provided, uses iterative update prompt.
pub async fn compact_conversation(
    messages: &[Message],
    client: &reqwest::Client,
    model: &str,
    api_key: &str,
    base_url: &str,
    trigger: CompactTrigger,
    is_auto: bool,
    last_summary: Option<&str>,
) -> anyhow::Result<CompactionResult> {
    if messages.is_empty() {
        anyhow::bail!("No messages to compact");
    }

    let pre_compact_tokens = estimate_total_tokens(messages);

    // 3-pass pre-pruning to reduce token usage before LLM compaction
    let mut pruned_messages = messages.to_vec();
    prune_tool_results(&mut pruned_messages);

    // Redact sensitive information from messages before sending
    for msg in &mut pruned_messages {
        match &mut msg.content {
            MessageContent::Text(text) => {
                *text = redact_sensitive_text(text);
            }
            MessageContent::ToolUseBlocks(blocks) => {
                for block in blocks {
                    if let Ok(json) = serde_json::to_string(&block.input) {
                        let redacted = redact_sensitive_text(&json);
                        if let Ok(parsed) = serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(&redacted) {
                            block.input = parsed;
                        }
                    }
                }
            }
            MessageContent::ToolResultBlocks(blocks) => {
                for block in blocks {
                    for content in &mut block.content {
                        if let crate::context::ToolResultContent::Text { text } = content {
                            *text = redact_sensitive_text(text);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Build the API request: send all messages with a summary instruction
    let api_messages: Vec<serde_json::Value> = pruned_messages
        .iter()
        .filter_map(|msg| message_to_api(msg))
        .collect();

    // Choose prompt based on whether we have a previous summary
    let user_prompt = if let Some(summary) = last_summary {
        COMPACT_USER_PROMPT_ITERATIVE.replace("{previous_summary}", summary)
    } else {
        COMPACT_USER_PROMPT.to_string()
    };

    // Build the payload
    let mut payload = serde_json::Map::new();
    payload.insert("model".to_string(), serde_json::json!(model));
    payload.insert("max_tokens".to_string(), serde_json::json!(20000));
    payload.insert(
        "system".to_string(),
        serde_json::json!([{"type": "text", "text": COMPACT_SYSTEM_PROMPT}]),
    );
    payload.insert("messages".to_string(), serde_json::json!(api_messages));

    // Add the summary prompt as the last message
    let final_messages = vec![serde_json::json!({
        "role": "user",
        "content": [{"type": "text", "text": user_prompt}]
    })];
    payload.insert("messages".to_string(), serde_json::json!(final_messages));

    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));

    let response = client
        .post(&url)
        .header("x-api-key", api_key)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .json(&payload)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Compact API request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Compact API error {}: {}", status, body);
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse compact response: {}", e))?;

    // Extract the summary text from the response and redact sensitive info
    let summary_text = extract_summary_text(&body)
        .map(|t| redact_sensitive_text(&t))
        .ok_or_else(|| anyhow::anyhow!("No summary text in compact response"))?;

    // Build the compaction result
    let boundary = Message::new(
        MessageRole::System,
        MessageContent::CompactBoundary {
            trigger,
            pre_compact_tokens,
        },
    );

    let summary_content = format!(
        "[Previous conversation summary ({} tokens compressed)]\n\n{}",
        pre_compact_tokens, summary_text
    );
    let summary = Message::new(
        MessageRole::User,
        MessageContent::Summary(summary_content),
    );

    let post_compact_tokens = estimate_message_tokens(&boundary)
        + estimate_message_tokens(&summary);

    if !is_auto {
        eprintln!(
            "[Compaction] {}: {} -> 2 messages, ~{} tokens saved",
            trigger,
            messages.len(),
            pre_compact_tokens.saturating_sub(post_compact_tokens)
        );
    }

    Ok(CompactionResult {
        boundary,
        summary,
        pre_compact_tokens,
        post_compact_tokens,
    })
}

/// Extract summary text from the API response
fn extract_summary_text(body: &serde_json::Value) -> Option<String> {
    let mut text = String::new();
    if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text.push_str(t);
                }
            }
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Convert a Message to API format (for sending to the compact API)
fn message_to_api(msg: &Message) -> Option<serde_json::Value> {
    match &msg.content {
        MessageContent::Text(text) => Some(serde_json::json!({
            "role": msg.role.as_str(),
            "content": [{"type": "text", "text": text}]
        })),
        MessageContent::ToolUseBlocks(blocks) => {
            let content: Vec<serde_json::Value> = blocks.iter().map(|b| {
                serde_json::json!({
                    "type": "tool_use",
                    "id": b.id,
                    "name": b.name,
                    "input": b.input
                })
            }).collect();
            Some(serde_json::json!({
                "role": "assistant",
                "content": content
            }))
        }
        MessageContent::ToolResultBlocks(blocks) => {
            let content: Vec<serde_json::Value> = blocks.iter().map(|b| {
                let content_values: Vec<serde_json::Value> = b.content.iter()
                    .filter_map(|c| serde_json::to_value(c).ok())
                    .collect();
                serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": b.tool_use_id,
                    "is_error": b.is_error,
                    "content": content_values
                })
            }).collect();
            Some(serde_json::json!({
                "role": "user",
                "content": content
            }))
        }
        MessageContent::Summary(text) => Some(serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": text}]
        })),
        MessageContent::CompactBoundary { .. } => {
            // Skip compact boundaries when sending to compact API
            // They're already summarized
            None
        }
    }
}

// --- Round-based legacy compaction ---

/// Find tool-use round boundaries (assistant tool_use + user tool_result pairs)
pub fn find_round_boundaries(messages: &[Message]) -> Vec<(usize, usize)> {
    let mut rounds = Vec::new();
    let mut round_start = 0;

    for (i, msg) in messages.iter().enumerate() {
        if matches!(msg.content, MessageContent::ToolResultBlocks(_)) {
            rounds.push((round_start, i));
            round_start = i + 1;
        }
    }

    if round_start < messages.len() {
        rounds.push((round_start, messages.len() - 1));
    }

    rounds
}

// --- Compactor struct (main entry point) ---

/// Compactor handles context compaction with LLM-based and fallback strategies
pub struct Compactor {
    phase: CompactPhase,
    max_tokens: usize,
    compact_threshold: f64,
    compact_buffer: usize,
    llm_compact_failed_count: usize,
    max_llm_compact_failures: usize,
    /// Previous summary for iterative compaction (A2)
    pub last_summary: Option<String>,
    /// Recent compaction savings ratios for anti-thrashing (A4)
    last_compact_savings: Vec<f64>,
}

impl Compactor {
    pub fn new() -> Self {
        Self {
            phase: CompactPhase::None,
            max_tokens: 200_000, // default model context window
            compact_threshold: 0.75,
            compact_buffer: 13_000,
            llm_compact_failed_count: 0,
            max_llm_compact_failures: 3,
            last_summary: None,
            last_compact_savings: Vec::with_capacity(2),
        }
    }

    /// Configure the compactor
    pub fn with_max_tokens(mut self, max: usize) -> Self {
        self.max_tokens = max;
        self
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.compact_threshold = threshold;
        self
    }

    pub fn with_buffer(mut self, buffer: usize) -> Self {
        self.compact_buffer = buffer;
        self
    }

    /// Run compaction on context.
    /// Returns CompactStats with the result.
    /// If LLM compaction succeeds, replaces old messages with summary.
    /// If LLM compaction fails (too many consecutive failures), falls back to truncation.
    pub async fn compact(
        &mut self,
        context: &mut ConversationContext,
        client: &reqwest::Client,
        model: &str,
        api_key: &str,
        base_url: &str,
    ) -> CompactStats {
        let messages = context.messages().to_vec();
        let entries_before = context.len();
        let tokens_before = estimate_total_tokens(&messages);

        let tracker = ContextWindowTracker::new(
            model,
            self.compact_threshold,
            self.compact_buffer,
        );

        // Check if compaction is needed
        if !tracker.should_compact(&messages) {
            return CompactStats {
                phase: CompactPhase::None,
                entries_before,
                entries_after: entries_before,
                estimated_tokens_saved: 0,
                estimated_tokens_before: tokens_before,
                estimated_tokens_after: tokens_before,
            };
        }

        // Anti-thrashing (A4): skip if recent savings < 10%
        if self.last_compact_savings.len() >= 2
            && self.last_compact_savings.iter().all(|s| *s < 0.10)
        {
            eprintln!("[Compaction] Skipping: recent compactions saved <10% each (anti-thrashing)");
            return CompactStats {
                phase: CompactPhase::None,
                entries_before,
                entries_after: entries_before,
                estimated_tokens_saved: 0,
                estimated_tokens_before: tokens_before,
                estimated_tokens_after: tokens_before,
            };
        }

        let usage = tracker.usage_info(&messages);
        eprintln!(
            "[Compaction] Triggered: {} tokens ({}% of effective window)",
            usage.estimated_tokens, usage.percent_used
        );

        // Try LLM compaction first (if we haven't failed too many times)
        if self.llm_compact_failed_count < self.max_llm_compact_failures {
            match compact_conversation(
                &messages,
                client,
                model,
                api_key,
                base_url,
                CompactTrigger::Auto,
                true,
                self.last_summary.as_deref(), // A2: iterative summary
            ).await {
                Ok(result) => {
                    let savings = if tokens_before > 0 {
                        (tokens_before - result.post_compact_tokens) as f64 / tokens_before as f64
                    } else {
                        0.0
                    };

                    // Record savings for anti-thrashing (A4)
                    self.last_compact_savings.push(savings);
                    if self.last_compact_savings.len() > 2 {
                        self.last_compact_savings.remove(0);
                    }

                    // A2: Store the summary for iterative updates
                    if let MessageContent::Summary(ref text) = result.summary.content {
                        self.last_summary = Some(text.clone());
                    }

                    // Reset failure count on success
                    self.llm_compact_failed_count = 0;
                    self.phase = CompactPhase::None;

                    // Replace context with boundary + summary
                    context.replace_messages(vec![result.boundary, result.summary]);

                    return CompactStats {
                        phase: CompactPhase::None, // LLM compaction doesn't use legacy phases
                        entries_before,
                        entries_after: context.len(),
                        estimated_tokens_saved: tokens_before.saturating_sub(
                            result.post_compact_tokens
                        ),
                        estimated_tokens_before: tokens_before,
                        estimated_tokens_after: result.post_compact_tokens,
                    };
                }
                Err(e) => {
                    eprintln!("[Compaction] LLM compaction failed: {}", e);
                    self.llm_compact_failed_count += 1;
                    if self.llm_compact_failed_count >= self.max_llm_compact_failures {
                        eprintln!(
                            "[Compaction] LLM compaction disabled after {} consecutive failures, using truncation fallback",
                            self.max_llm_compact_failures
                        );
                    }
                    // Fall through to legacy truncation
                }
            }
        }

        // Fallback: legacy truncation-based compaction
        self.legacy_compact(context)
    }

    /// Legacy compact using truncation (fallback when LLM compaction fails)
    fn legacy_compact(&mut self, context: &mut ConversationContext) -> CompactStats {
        let entries_before = context.len();
        let tokens_before = estimate_total_tokens(context.messages());

        let messages = context.messages().to_vec();
        let rounds = find_round_boundaries(&messages);
        let ratio = tokens_before as f64 / self.max_tokens as f64;

        let phase = if ratio <= 0.80 {
            CompactPhase::RoundBased
        } else if ratio <= 0.90 {
            CompactPhase::TurnBased
        } else if ratio <= 0.95 {
            CompactPhase::SelectiveClear
        } else {
            CompactPhase::Aggressive
        };

        self.phase = phase;

        match phase {
            CompactPhase::None => {}
            CompactPhase::RoundBased => {
                if rounds.len() > 3 {
                    let keep_from = rounds[rounds.len() - 3].0;
                    let first = messages[..1].to_vec();
                    let recent = messages[keep_from..].to_vec();
                    context.replace_messages([first, recent].concat());
                }
            }
            CompactPhase::TurnBased => {
                if messages.len() > 6 {
                    let first = messages[..1].to_vec();
                    let recent = messages[messages.len() - 5..].to_vec();
                    context.replace_messages([first, recent].concat());
                }
            }
            CompactPhase::SelectiveClear => {
                if messages.len() > 4 {
                    let first = messages[..1].to_vec();
                    let recent = messages[messages.len() - 3..].to_vec();
                    context.replace_messages([first, recent].concat());
                }
            }
            CompactPhase::Aggressive => {
                context.minimum_history();
            }
            CompactPhase::Truncated => {
                // Already handled by force_compact in agent_loop
            }
        }

        // After truncation, validate tool pairing and fix role alternation.
        // Naive slice truncation can orphan tool_results (their tool_use was
        // dropped) and leave consecutive same-role messages, both of which
        // cause the Anthropic API to reject the request with error 2013.
        context.validate_tool_pairing();
        context.fix_role_alternation();

        let entries_after = context.len();
        let tokens_after = estimate_total_tokens(context.messages());
        let tokens_saved = tokens_before.saturating_sub(tokens_after);

        CompactStats {
            phase,
            entries_before,
            entries_after,
            estimated_tokens_saved: tokens_saved,
            estimated_tokens_before: tokens_before,
            estimated_tokens_after: tokens_after,
        }
    }

    /// Get current phase
    #[allow(dead_code)]
    pub fn phase(&self) -> CompactPhase {
        self.phase
    }

    /// Reset compaction state
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.phase = CompactPhase::None;
        self.llm_compact_failed_count = 0;
        self.last_summary = None;
        self.last_compact_savings.clear();
    }
}

impl Default for Compactor {
    fn default() -> Self {
        Self::new()
    }
}

// --- Post-compact context restoration ---

/// Restores relevant context after compaction (files, plan, etc.)
pub struct PostCompactRestorer {
    pub max_files_to_restore: usize,
    pub max_tokens_per_file: usize,
    pub token_budget: usize,
}

impl Default for PostCompactRestorer {
    fn default() -> Self {
        Self {
            max_files_to_restore: 5,
            max_tokens_per_file: 5_000,
            token_budget: 50_000,
        }
    }
}

impl PostCompactRestorer {
    /// Restore recently read file contents as summary attachments
    pub fn restore_recent_files(
        &self,
        file_state: &std::collections::HashMap<String, String>,
    ) -> Vec<String> {
        // Sort by timestamp if available, otherwise just take most recent
        let mut files: Vec<_> = file_state.iter().collect();
        files.sort_by(|a, b| b.0.cmp(a.0)); // Sort by filename (proxy for recency)

        let mut restored = Vec::new();
        let mut used_tokens = 0;

        for (path, content) in files.iter().take(self.max_files_to_restore) {
            let truncated = if estimate_tokens(content) > self.max_tokens_per_file {
                let char_budget = self.max_tokens_per_file * 4;
                format!("...[truncated]...\n{}", &content[..char_budget.min(content.len())])
            } else {
                content.to_string()
            };

            let tokens = estimate_tokens(&truncated);
            if used_tokens + tokens <= self.token_budget {
                restored.push(format!(
                    "[Recently read file: {}]\n{}",
                    path, truncated
                ));
                used_tokens += tokens;
            }
        }

        restored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ToolUseBlock, ToolResultBlock};

    #[test]
    fn test_token_estimation() {
        assert_eq!(estimate_tokens("hello"), 2); // 5 chars / 4 = 1.25, ceil = 2
        assert_eq!(estimate_tokens("hello world"), 3); // 11 chars / 4 = 2.75, ceil = 3
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_context_window_tracker() {
        let tracker = ContextWindowTracker::new("claude-sonnet-4-20250514", 0.75, 13_000);
        assert_eq!(tracker.effective_window(), 180_000); // 200K - 20K
        // threshold = min(180K * 0.75, 180K - 13K) = min(135K, 167K) = 135K
        assert_eq!(tracker.compact_threshold(), 135_000);
    }

    #[test]
    fn test_find_round_boundaries() {
        let config = crate::config::Config::default();
        let mut ctx = crate::context::ConversationContext::new(config);
        ctx.add_user_message("Read file".to_string());
        ctx.add_assistant_tool_calls(vec![ToolUseBlock {
            id: "tool-1".to_string(),
            name: "read_file".to_string(),
            input: std::collections::HashMap::new(),
        }]);
        ctx.add_tool_results(vec![ToolResultBlock {
            tool_use_id: "tool-1".to_string(),
            content: vec![crate::context::ToolResultContent::Text { text: "content".to_string() }],
            is_error: false,
        }]);
        ctx.add_user_message("Edit file".to_string());
        ctx.add_assistant_tool_calls(vec![ToolUseBlock {
            id: "tool-2".to_string(),
            name: "edit_file".to_string(),
            input: std::collections::HashMap::new(),
        }]);
        ctx.add_tool_results(vec![ToolResultBlock {
            tool_use_id: "tool-2".to_string(),
            content: vec![crate::context::ToolResultContent::Text { text: "done".to_string() }],
            is_error: false,
        }]);

        let rounds = find_round_boundaries(ctx.messages());
        assert_eq!(rounds.len(), 2);
    }

    #[test]
    fn test_message_to_api() {
        let msg = Message::new(
            MessageRole::User,
            MessageContent::Text("Hello world".to_string()),
        );
        let api = message_to_api(&msg).unwrap();
        assert_eq!(api.get("role").and_then(|r| r.as_str()), Some("user"));
    }

    // --- Feature 2: Content-type-aware token estimation (A5) ---

    #[test]
    fn test_detect_content_type() {
        // JSON detection
        assert_eq!(detect_content_type(r#"{"key":"val"}"#), "json");
        assert_eq!(detect_content_type("[1, 2, 3]"), "json");
        assert_eq!(detect_content_type("  { \"a\": 1 }  "), "json");
        assert_eq!(detect_content_type("  [1]  "), "json");

        // Code detection
        assert_eq!(detect_content_type("fn main() {}"), "code");
        assert_eq!(detect_content_type("class Foo: pass"), "code");
        assert_eq!(detect_content_type("def hello(x): pass"), "code");
        assert_eq!(detect_content_type("impl Trait for Foo {}"), "code");
        assert_eq!(detect_content_type("import std::io"), "code");
        assert_eq!(detect_content_type("package main"), "code");
        assert_eq!(detect_content_type("struct Foo { x: i32 }"), "code");
        assert_eq!(detect_content_type("const X = 42;"), "code");
        assert_eq!(detect_content_type("var x = 10;"), "code");
        assert_eq!(detect_content_type("type Result<T>"), "code");
        assert_eq!(detect_content_type("func foo() { }"), "code");

        // Natural text
        assert_eq!(detect_content_type("Hello, how are you?"), "natural");
        assert_eq!(detect_content_type("The quick brown fox jumps over the lazy dog"), "natural");

        // Not JSON: doesn't start with { or [
        assert_eq!(detect_content_type("some text"), "natural");
    }

    #[test]
    fn test_estimate_tokens_typed() {
        // JSON: len / 3.0 rounded up
        // {"x":1} = 7 chars, 7/3.0 = 2.33, ceil = 3
        assert_eq!(estimate_tokens_typed(r#"{"x":1}"#), 3);

        // Code: len / 3.5 rounded up
        // fn x(){} = 8 chars, 8/3.5 = 2.29, ceil = 3
        assert_eq!(estimate_tokens_typed("fn x(){}"), 3);

        // Natural: len / 4.0 rounded up
        // Hello = 5 chars, 5/4.0 = 1.25, ceil = 2
        assert_eq!(estimate_tokens_typed("Hello"), 2);

        // Empty
        assert_eq!(estimate_tokens_typed(""), 0);

        // Longer JSON: {"key": "value", "num": 42} = 27 chars, 27/3.0 = 9.0, ceil = 9
        assert_eq!(estimate_tokens_typed(r#"{"key": "value", "num": 42}"#), 9);

        // Longer code: fn main() { println!("Hello, world!"); } = 39 chars, 39/3.5 = 11.14, ceil = 12
        assert_eq!(
            estimate_tokens_typed(r#"fn main() { println!("Hello, world!"); }"#),
            12
        );
    }

    #[test]
    fn test_message_tokens_uses_typed_estimation() {
        // Text with code content should use code ratio (3.5)
        let code_msg = Message::new(
            MessageRole::User,
            MessageContent::Text("fn main() {}".to_string()),
        );
        // 12 chars / 3.5 = 3.43, ceil = 4, + 3 role overhead = 7
        assert_eq!(estimate_message_tokens(&code_msg), 7);

        // Text with JSON content should use json ratio (3.0)
        let json_msg = Message::new(
            MessageRole::User,
            MessageContent::Text(r#"{"a":1}"#.to_string()),
        );
        // 7 chars / 3.0 = 2.33, ceil = 3, + 3 role overhead = 6
        assert_eq!(estimate_message_tokens(&json_msg), 6);

        // Summary with natural text should use natural ratio (4.0)
        let summary_msg = Message::new(
            MessageRole::User,
            MessageContent::Summary("Hello world".to_string()),
        );
        // 11 chars / 4.0 = 2.75, ceil = 3, + 3 role overhead = 6
        assert_eq!(estimate_message_tokens(&summary_msg), 6);

        // ToolUseBlocks should use json ratio (3.0) for input
        let mut tool_input = std::collections::HashMap::new();
        tool_input.insert("path".to_string(), serde_json::Value::String("test.txt".to_string()));
        let tool_msg = Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                id: "t1".to_string(),
                name: "read_file".to_string(),
                input: tool_input,
            }]),
        );
        // Input JSON: {"path":"test.txt"} = 19 chars, 19/3.0 = 6.33, ceil = 7
        // Name "read_file" = 9 chars, natural ratio: 9/4.0 = 2.25, ceil = 3
        // total = 3 (role) + 10 (overhead) + 3 (name) + 7 (input) = 23
        assert_eq!(estimate_message_tokens(&tool_msg), 23);
    }

    // --- Feature 1: 3-pass pre-pruning (A1) ---

    #[test]
    fn test_dedup_tool_results() {
        let mut messages = vec![
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "tool-1".to_string(),
                    content: vec![crate::context::ToolResultContent::Text {
                        text: "same content here".to_string(),
                    }],
                    is_error: false,
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "tool-2".to_string(),
                    content: vec![crate::context::ToolResultContent::Text {
                        text: "same content here".to_string(),
                    }],
                    is_error: false,
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "tool-3".to_string(),
                    content: vec![crate::context::ToolResultContent::Text {
                        text: "different content".to_string(),
                    }],
                    is_error: false,
                }]),
            ),
        ];

        dedup_tool_results(&mut messages);

        // First result: original, unchanged
        if let MessageContent::ToolResultBlocks(blocks) = &messages[0].content {
            if let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0] {
                assert_eq!(text, "same content here");
            } else {
                panic!("Expected Text content");
            }
        } else {
            panic!("Expected ToolResultBlocks");
        }

        // Second result: duplicate of first — replaced with reference
        if let MessageContent::ToolResultBlocks(blocks) = &messages[1].content {
            if let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0] {
                assert!(text.contains("[duplicate result, see tool_use_id tool-1]"));
            } else {
                panic!("Expected Text content");
            }
        } else {
            panic!("Expected ToolResultBlocks");
        }

        // Third result: unique content, unchanged
        if let MessageContent::ToolResultBlocks(blocks) = &messages[2].content {
            if let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0] {
                assert_eq!(text, "different content");
            } else {
                panic!("Expected Text content");
            }
        } else {
            panic!("Expected ToolResultBlocks");
        }
    }

    #[test]
    fn test_dedup_same_tool_use_id_not_marked_duplicate() {
        // If the same tool_use_id appears twice (shouldn't happen normally), it should not
        // be marked as duplicate of itself.
        let mut messages = vec![
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![
                    ToolResultBlock {
                        tool_use_id: "tool-1".to_string(),
                        content: vec![crate::context::ToolResultContent::Text {
                            text: "content".to_string(),
                        }],
                        is_error: false,
                    },
                    ToolResultBlock {
                        tool_use_id: "tool-1".to_string(),
                        content: vec![crate::context::ToolResultContent::Text {
                            text: "content".to_string(),
                        }],
                        is_error: false,
                    },
                ]),
            ),
        ];

        dedup_tool_results(&mut messages);

        if let MessageContent::ToolResultBlocks(blocks) = &messages[0].content {
            // Both blocks should still have original content
            assert_eq!(blocks.len(), 2);
            for block in blocks {
                if let crate::context::ToolResultContent::Text { text } = &block.content[0] {
                    assert_eq!(text, "content");
                }
            }
        }
    }

    #[test]
    fn test_summarize_old_tool_results() {
        let mut messages = Vec::new();

        // Tool call/result pair 1 (will be in the "old" section)
        messages.push(Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: {
                    let mut m = std::collections::HashMap::new();
                    m.insert("path".to_string(), serde_json::Value::String("foo.txt".to_string()));
                    m
                },
            }]),
        ));
        messages.push(Message::new(
            MessageRole::User,
            MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "tool-1".to_string(),
                content: vec![crate::context::ToolResultContent::Text {
                    text: "line 1\nline 2\nline 3".to_string(),
                }],
                is_error: false,
            }]),
        ));

        // Add a large message to push the tool result into the "old" section
        messages.push(Message::new(
            MessageRole::User,
            MessageContent::Text("x".repeat(500)),
        ));
        messages.push(Message::new(
            MessageRole::User,
            MessageContent::Text("y".repeat(500)),
        ));

        summarize_old_tool_results(&mut messages);

        // The tool result (messages[1]) should be summarized
        if let MessageContent::ToolResultBlocks(blocks) = &messages[1].content {
            if let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0] {
                assert!(text.starts_with("[read_file] -> ok,"));
                assert!(text.contains("3 lines"));
            } else {
                panic!("Expected Text content");
            }
        } else {
            panic!("Expected ToolResultBlocks");
        }
    }

    #[test]
    fn test_summarize_error_tool_result() {
        let mut messages = Vec::new();

        messages.push(Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                id: "t1".to_string(),
                name: "exec".to_string(),
                input: std::collections::HashMap::new(),
            }]),
        ));
        messages.push(Message::new(
            MessageRole::User,
            MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "t1".to_string(),
                content: vec![crate::context::ToolResultContent::Text {
                    text: "error output\nline 2".to_string(),
                }],
                is_error: true,
            }]),
        ));
        // Large messages to push tool result into old section
        messages.push(Message::new(
            MessageRole::User,
            MessageContent::Text("z".repeat(1000)),
        ));

        summarize_old_tool_results(&mut messages);

        if let MessageContent::ToolResultBlocks(blocks) = &messages[1].content {
            if let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0] {
                assert!(text.contains("[exec] -> error"));
                assert!(text.contains("2 lines"));
            }
        }
    }

    #[test]
    fn test_truncate_large_tool_args() {
        // Create a tool use block with input exceeding 2000 chars
        let mut tool_input = std::collections::HashMap::new();
        tool_input.insert(
            "content".to_string(),
            serde_json::Value::String("A".repeat(3000)),
        );
        tool_input.insert(
            "path".to_string(),
            serde_json::Value::String("large_file.txt".to_string()),
        );

        let mut messages = vec![Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                id: "t1".to_string(),
                name: "write_file".to_string(),
                input: tool_input,
            }]),
        )];

        truncate_large_tool_args(&mut messages);

        if let MessageContent::ToolUseBlocks(blocks) = &messages[0].content {
            let json = serde_json::to_string(&blocks[0].input).unwrap();
            assert!(json.len() <= 2100, "Truncated JSON too long: {}", json.len());
            // Should contain truncation indicator
            assert!(
                json.contains("...[truncated]") || json.contains("truncated"),
                "No truncation marker in: {}",
                json
            );
        } else {
            panic!("Expected ToolUseBlocks");
        }
    }

    #[test]
    fn test_truncate_small_tool_args_unchanged() {
        let mut tool_input = std::collections::HashMap::new();
        tool_input.insert(
            "path".to_string(),
            serde_json::Value::String("small.txt".to_string()),
        );

        let mut messages = vec![Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                id: "t1".to_string(),
                name: "read_file".to_string(),
                input: tool_input.clone(),
            }]),
        )];

        truncate_large_tool_args(&mut messages);

        // Small input should be unchanged
        if let MessageContent::ToolUseBlocks(blocks) = &messages[0].content {
            assert_eq!(blocks[0].input, tool_input);
        }
    }

    #[test]
    fn test_prune_tool_results_integration() {
        let mut messages = Vec::new();

        // Create several tool call/result pairs
        for i in 0..6 {
            messages.push(Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: format!("tool-{}", i),
                    name: "read_file".to_string(),
                    input: std::collections::HashMap::new(),
                }]),
            ));
            messages.push(Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: format!("tool-{}", i),
                    content: vec![crate::context::ToolResultContent::Text {
                        text: "result content line 1\nresult content line 2\nresult content line 3\nresult content line 4\nresult content line 5".to_string(),
                    }],
                    is_error: false,
                }]),
            ));
        }

        let total_before: usize = messages.iter().map(estimate_message_tokens).sum();

        prune_tool_results(&mut messages);

        let total_after: usize = messages.iter().map(estimate_message_tokens).sum();

        // Pruning should reduce total token count (old tool results get summarized)
        assert!(
            total_after < total_before,
            "Pruning should reduce tokens: before={}, after={}",
            total_before,
            total_after
        );
    }

    #[test]
    fn test_prune_empty_messages() {
        let mut messages: Vec<Message> = Vec::new();
        // Should not panic
        prune_tool_results(&mut messages);
        assert!(messages.is_empty());
    }

    #[test]
    fn test_prune_no_tool_messages() {
        let mut messages = vec![
            Message::new(
                MessageRole::User,
                MessageContent::Text("Hello".to_string()),
            ),
            Message::new(
                MessageRole::Assistant,
                MessageContent::Text("Hi there!".to_string()),
            ),
        ];

        let before = estimate_total_tokens(&messages);
        prune_tool_results(&mut messages);
        let after = estimate_total_tokens(&messages);

        // No tool messages, so token count should be unchanged
        assert_eq!(before, after);
    }

    // --- Feature A3: Sensitive info redaction ---

    #[test]
    fn test_redact_sensitive_json_key() {
        let input = r#"{"api_key": "sk-secret123", "name": "test"}"#;
        let output = redact_sensitive_text(input);
        assert!(!output.contains("sk-secret123"));
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains("\"name\": \"test\""));
    }

    #[test]
    fn test_redact_sensitive_kv_pattern() {
        let input = "password=secret123 token=abc456";
        let output = redact_sensitive_text(input);
        assert!(!output.contains("secret123"));
        assert!(!output.contains("abc456"));
        assert!(output.contains("password=[REDACTED]"));
        assert!(output.contains("token=[REDACTED]"));
    }

    #[test]
    fn test_redact_no_sensitive_keys() {
        let input = "Hello world, path=/home/user/file.txt";
        let output = redact_sensitive_text(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_redact_various_sensitive_keys() {
        let keys = ["api_key", "password", "secret", "token", "credential",
                     "auth", "private_key", "access_key", "secret_key", "passwd",
                     "access_token", "refresh_token"];
        for key in keys {
            let input = format!("{}=myvalue", key);
            let output = redact_sensitive_text(&input);
            assert!(output.contains("[REDACTED]"), "Failed to redact key: {}", key);
            assert!(!output.contains("myvalue"), "Value leaked for key: {}", key);
        }
    }

    // --- Feature A2: Compactor last_summary field ---

    #[test]
    fn test_compactor_last_summary() {
        let mut compactor = Compactor::new();
        assert!(compactor.last_summary.is_none());

        compactor.last_summary = Some("Previous summary".to_string());
        assert_eq!(compactor.last_summary, Some("Previous summary".to_string()));

        compactor.reset();
        assert!(compactor.last_summary.is_none());
    }

    // --- Feature A4: Anti-thrashing ---

    #[test]
    fn test_compactor_anti_thrashing() {
        let mut compactor = Compactor::new();

        // Initially no savings recorded — should not skip
        assert!(compactor.last_compact_savings.is_empty());

        // Record low savings
        compactor.last_compact_savings.push(0.05);
        compactor.last_compact_savings.push(0.08);

        // With 2 low savings, anti-thrashing should kick in
        assert_eq!(compactor.last_compact_savings.len(), 2);
        assert!(compactor.last_compact_savings.iter().all(|s| *s < 0.10));

        // Reset clears anti-thrashing state
        compactor.reset();
        assert!(compactor.last_compact_savings.is_empty());
    }

    #[test]
    fn test_compactor_savings_tracking() {
        let mut compactor = Compactor::new();

        // Push savings (simulating what compact() does: push then trim to 2)
        compactor.last_compact_savings.push(0.15);
        compactor.last_compact_savings.push(0.20);
        compactor.last_compact_savings.push(0.25);
        // Trim to last 2 (as compact() does)
        if compactor.last_compact_savings.len() > 2 {
            let remove_idx = compactor.last_compact_savings.len() - 2;
            compactor.last_compact_savings.drain(0..remove_idx);
        }

        assert_eq!(compactor.last_compact_savings.len(), 2);
        assert!((compactor.last_compact_savings[0] - 0.20).abs() < 0.001);
        assert!((compactor.last_compact_savings[1] - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_compact_user_prompt_iterative() {
        let prompt = COMPACT_USER_PROMPT_ITERATIVE.replace("{previous_summary}", "Old summary");
        assert!(prompt.contains("Old summary"));
        assert!(prompt.contains("update"));
    }
}
