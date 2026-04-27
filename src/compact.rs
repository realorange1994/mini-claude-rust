//! Compact module - intelligent context compaction
//!
//! Implements multi-layered context management inspired by Claude Code's official implementation:
//! 1. Micro-compaction (time-based tool result clearing)
//! 2. LLM-driven compaction (summary generation via API call)
//! 3. Progressive truncation (fallback when compaction fails)

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

/// Estimate tokens for a single message, accounting for content type overhead
pub fn estimate_message_tokens(msg: &Message) -> usize {
    match &msg.content {
        MessageContent::Text(text) => {
            // Role overhead (~3 tokens) + content
            3 + estimate_tokens(text)
        }
        MessageContent::ToolUseBlocks(blocks) => {
            let mut total = 3; // role overhead
            for block in blocks {
                total += 10; // type(1) + id(4) + name(3) + structure(2)
                total += estimate_tokens(&block.name);
                if let Ok(json) = serde_json::to_string(&block.input) {
                    total += estimate_tokens(&json);
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
                            total += estimate_tokens(text);
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
            3 + estimate_tokens(text)
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

/// Compress messages using LLM summary generation.
/// This is the primary compaction method — replaces older truncation-based approaches.
pub async fn compact_conversation(
    messages: &[Message],
    client: &reqwest::Client,
    model: &str,
    api_key: &str,
    base_url: &str,
    trigger: CompactTrigger,
    is_auto: bool,
) -> anyhow::Result<CompactionResult> {
    if messages.is_empty() {
        anyhow::bail!("No messages to compact");
    }

    let pre_compact_tokens = estimate_total_tokens(messages);

    // Build the API request: send all messages with a summary instruction
    let api_messages: Vec<serde_json::Value> = messages
        .iter()
        .filter_map(|msg| message_to_api(msg))
        .collect();

    // Add the summary instruction as the final user message
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
        "content": [{"type": "text", "text": COMPACT_USER_PROMPT}]
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

    // Extract the summary text from the response
    let summary_text = extract_summary_text(&body)
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
            ).await {
                Ok(result) => {
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
}
