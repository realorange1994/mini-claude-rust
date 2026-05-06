//! Compact module - intelligent context compaction
//!
//! Implements multi-layered context management inspired by Claude Code's official implementation:
//! 1. Micro-compaction (time-based tool result clearing)
//! 2. LLM-driven compaction (summary generation via API call)
//! 3. Progressive truncation (fallback when compaction fails)

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use regex::Regex;

use crate::context::{
    CompactTrigger, ConversationContext, Message, MessageContent, MessageRole,
    ToolResultContent, ToolUseBlock, ToolResultBlock,
};
use crate::session_memory::SessionMemory;

/// Creates a compact boundary Message with a unique UUID.
/// Used by the transcript, session storage, and QueryEngine to reference
/// specific compaction events.
pub fn make_compact_boundary_message(trigger: CompactTrigger, pre_compact_tokens: usize) -> Message {
    Message::new(
        MessageRole::System,
        MessageContent::CompactBoundary {
            trigger,
            pre_compact_tokens,
            uuid: uuid::Uuid::new_v4().to_string(),
        },
    )
}

/// Direction for partial compaction.
/// - `UpTo`: Compact everything before the pivot index (keeps recent context)
/// - `From`: Compact everything after the pivot index (keeps early + recent context)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialCompactDirection {
    UpTo,
    From,
}

impl std::fmt::Display for PartialCompactDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartialCompactDirection::UpTo => write!(f, "up_to"),
            PartialCompactDirection::From => write!(f, "from"),
        }
    }
}

/// Result from partial compaction
pub struct PartialCompactionResult {
    pub boundary: Message,
    pub summary: Message,
    pub entries_before: usize,
    pub entries_after: usize,
    pub pre_compact_tokens: usize,
    pub post_compact_tokens: usize,
}

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
                    // Tool inputs are JSON -- use json ratio
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
                            // Tool results are typically JSON/structured -- use json ratio
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
        MessageContent::Attachment(text) => {
            3 + estimate_tokens_typed(text)
        }
    }
}

/// Estimate total tokens for all messages.
/// Applies 4/3 padding factor (matching upstream's Math.ceil(totalTokens * 4/3))
/// for conservative estimates that avoid over-filling the context window.
pub fn estimate_total_tokens(messages: &[Message]) -> usize {
    let total: usize = messages.iter().map(estimate_message_tokens).sum();
    (total as f64 * 4.0 / 3.0).ceil() as usize
}

/// Truncate a string to max_len, appending "..." if truncated.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

/// Generate a detailed summary of conversation entries, matching Go's entriesToSummaryText.
/// Produces per-message previews (user, assistant, tool calls, tool results), counts
/// turns and tool calls, and lists files mentioned.
pub(crate) fn entries_to_summary_text(messages: &[Message]) -> String {
    let mut details = String::new();
    let mut turn_count = 0;
    let mut tool_call_count = 0;
    let mut files_mentioned: Vec<String> = Vec::new();

    for msg in messages {
        match &msg.content {
            MessageContent::Text(text) => {
                if msg.role == MessageRole::User {
                    turn_count += 1;
                    let preview = truncate_preview(text, 200);
                    details.push_str(&format!("User: {}\n", preview));
                } else if msg.role == MessageRole::Assistant {
                    let preview = truncate_preview(text, 200);
                    details.push_str(&format!("Assistant: {}\n", preview));
                }
            }
            MessageContent::ToolUseBlocks(blocks) => {
                for block in blocks {
                    tool_call_count += 1;
                    // Extract file paths from tool call input
                    if let Some(path) = block.input.get("path")
                        .or_else(|| block.input.get("file_path"))
                        .and_then(|v| v.as_str())
                    {
                        files_mentioned.push(path.to_string());
                    }
                    details.push_str(&format!("[tool call: {}]\n", block.name));
                }
            }
            MessageContent::ToolResultBlocks(blocks) => {
                for block in blocks {
                    for content in &block.content {
                        if let ToolResultContent::Text { text } = content {
                            let lines = text.lines().count();
                            let preview = truncate_preview(text, 100);
                            details.push_str(&format!(
                                "[tool result: {} lines] {}\n",
                                lines.saturating_add(1),
                                preview
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Deduplicate files mentioned
    files_mentioned.sort();
    files_mentioned.dedup();

    let mut summary = String::new();
    summary.push_str(&format!(
        "Summary of {} conversation turns with {} tool calls.\n",
        turn_count, tool_call_count
    ));
    if !files_mentioned.is_empty() {
        summary.push_str(&format!("Files mentioned: {}\n", files_mentioned.join(", ")));
    }
    summary.push_str("---\n");
    summary.push_str(&details);
    summary
}

// --- Context window tracking ---

/// Model-specific context window sizes.
/// Supports [1m] suffix and known Sonnet 4 / Opus 4 models for 1M context.
/// Falls back to 200K for all other models.
pub fn model_context_window(model: &str) -> usize {
    let lower = model.to_lowercase();
    // Priority 1: [1m] suffix — explicit 1M context request
    if lower.contains("[1m]") {
        return 1_000_000;
    }
    // Priority 2: Sonnet 4 / Opus 4 裸模型也支持 1M
    if (lower.contains("sonnet-4") || lower.contains("opus-4-6") || lower.contains("opus-4-7"))
        && !lower.contains("[haiku]") && !lower.contains("[3.5]") && !lower.contains("[3.0]") {
        return 1_000_000;
    }
    // Default to 200K for all other Anthropic models
    200_000
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
    /// Returns true if the context should be compacted based on token usage.
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
    /// Token count of the kept messages (summary + boundary + tail)
    pub tokens_after: usize,
    /// Token count of the post-compact result (boundary + summary + tail)
    pub post_compact_tokens: usize,
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
                // Skip error results — they contain important debugging info
                if block.is_error {
                    continue;
                }

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
                        // Duplicate found -- replace content
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
            let truncated_val: String = val_str.chars().take(30).collect();
            if val_str.len() > 30 {
                format!("{}={}", k, truncated_val)
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
                        let truncated: String = json.chars().take(MAX_ARGS_LEN).collect();
                        let truncated_json =
                            format!("{}...[truncated]", truncated);
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

/// Strip base64-encoded image data and image URLs from text/tool results.
/// Replaces them with a placeholder to save tokens during compaction.
fn strip_image_content(messages: &mut Vec<Message>) {
    static IMAGE_RE: OnceLock<Regex> = OnceLock::new();
    static URL_IMAGE_RE: OnceLock<Regex> = OnceLock::new();
    let image_re = IMAGE_RE.get_or_init(|| {
        Regex::new(r"data:image/[a-zA-Z0-9+.-]+;base64,[A-Za-z0-9+/=]{10,}").unwrap()
    });
    let url_re = URL_IMAGE_RE.get_or_init(|| {
        Regex::new(r"https?://\S+\.(?:png|jpg|jpeg|gif|webp|svg|bmp|tiff)").unwrap()
    });
    const PLACEHOLDER: &str = "[image content stripped]";

    for msg in messages {
        match &mut msg.content {
            MessageContent::Text(text) => {
                let stripped = image_re.replace_all(text, PLACEHOLDER);
                let stripped = url_re.replace_all(&stripped, PLACEHOLDER);
                *text = stripped.to_string();
            }
            MessageContent::ToolResultBlocks(blocks) => {
                for block in blocks {
                    for content in &mut block.content {
                        let ToolResultContent::Text { text } = content;
                        let stripped = image_re.replace_all(text, PLACEHOLDER);
                        let stripped = url_re.replace_all(&stripped, PLACEHOLDER);
                        *text = stripped.to_string();
                    }
                }
            }
            MessageContent::Attachment(text) => {
                let stripped = image_re.replace_all(text, PLACEHOLDER);
                let stripped = url_re.replace_all(&stripped, PLACEHOLDER);
                *text = stripped.to_string();
            }
            _ => {}
        }
    }
}

/// Run all 4 passes of pre-pruning on messages.
pub fn prune_tool_results(messages: &mut Vec<Message>) {
    dedup_tool_results(messages);
    // NOTE: summarize_old_tool_results removed — it replaced tool result content
    // with one-line summaries before the LLM compaction call, meaning the LLM
    // never saw the full content and couldn't generate an accurate summary.
    // This was the root cause of "micro-compact memory loss" where tool results
    // were permanently lost. Upstream avoids this by using cache_edits to delete
    // tool results server-side without modifying local messages, and uses a
    // system prompt section ("summarize_tool_results") instructing the model to
    // write down important information before results are cleared.
    truncate_large_tool_args(messages);
    strip_image_content(messages);
}

/// SelectiveClear: clear tool results for read-only tools (read_file, grep, glob,
/// web_fetch, web_search, list_dir) while preserving exec, git, write, and other
/// tools. This saves the most tokens while keeping critical debugging info intact.
pub fn selective_clear_read_only_tools(messages: &mut Vec<Message>, placeholder: &str) {
    // Build tool_use_id -> tool_name mapping
    let mut tool_name_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for msg in messages.iter() {
        if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
            for block in blocks {
                if !block.id.is_empty() {
                    tool_name_map.insert(block.id.clone(), block.name.clone());
                }
            }
        }
    }

    for msg in messages.iter_mut() {
        if let MessageContent::ToolResultBlocks(blocks) = &mut msg.content {
            for block in blocks.iter_mut() {
                // Skip error results
                if block.is_error {
                    continue;
                }
                if let Some(name) = tool_name_map.get(&block.tool_use_id) {
                    if is_read_only_tool(name) {
                        block.content = vec![ToolResultContent::Text {
                            text: placeholder.to_string(),
                        }];
                    }
                }
            }
        }
    }
}

/// Read-only tools whose results can be safely cleared during selective compaction
/// without losing write/exec side-effect information.
fn is_read_only_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file" | "grep" | "glob" | "web_fetch" | "web_search" | "list_dir"
    )
}

// --- LLM-driven compaction ---

/// Result from LLM-driven compaction
pub struct CompactionResult {
    pub boundary: Message,
    pub summary: Message,
    pub pre_compact_tokens: usize,
    pub post_compact_tokens: usize,
}

/// Aggressive no-tools preamble. Must appear BEFORE the main prompt to
/// prevent the model from wasting a turn attempting tool calls.
const NO_TOOLS_PREAMBLE: &str = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.

- Do NOT use Read, Bash, Grep, Glob, Edit, Write, or ANY other tool.
- You already have all the context you need in the conversation above.
- Tool calls will be REJECTED and will waste your only turn — you will fail the task.
- Your entire response must be plain text: an <analysis> block followed by a <summary> block.
";

/// Detailed analysis instruction for full compaction (BASE variant).
/// The <analysis> block is a drafting scratchpad that gets stripped before
/// the summary reaches context. Reserved for future partial compact LLM use.
#[allow(dead_code)]
const DETAILED_ANALYSIS_INSTRUCTION_BASE: &str = r#"Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts and ensure you've covered all necessary points. In your analysis process:

1. Chronologically analyze each message and section of the conversation. For each section thoroughly identify:
   - The user's explicit requests and intents
   - Your approach to addressing the user's requests
   - Key decisions, technical concepts and code patterns
   - Specific details like:
     - file names
     - full code snippets
     - function signatures
     - file edits
   - Errors that you ran into and how you fixed them
   - Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.
2. Double-check for technical accuracy and completeness, addressing each required element thoroughly."#;

/// Detailed analysis instruction for partial compaction (PARTIAL variant).
#[allow(dead_code)]
const DETAILED_ANALYSIS_INSTRUCTION_PARTIAL: &str = r#"Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts and ensure you've covered all necessary points. In your analysis process:

1. Analyze the recent messages chronologically. For each section thoroughly identify:
   - The user's explicit requests and intents
   - Your approach to addressing the user's requests
   - Key decisions, technical concepts and code patterns
   - Specific details like:
     - file names
     - full code snippets
     - function signatures
     - file edits
   - Errors that you ran into and how you fixed them
   - Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.
2. Double-check for technical accuracy and completeness, addressing each required element thoroughly."#;

/// Base compact prompt template (matches upstream BASE_COMPACT_PROMPT).
const BASE_COMPACT_PROMPT: &str = r#"Your task is to create a detailed summary of the conversation so far, paying close attention to the user's explicit requests and your previous actions.
This summary should be thorough in capturing technical details, code patterns, and architectural decisions that would be essential for continuing development work without losing context.

Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts and ensure you've covered all necessary points. In your analysis process:

1. Chronologically analyze each message and section of the conversation. For each section thoroughly identify:
   - The user's explicit requests and intents
   - Your approach to addressing the user's requests
   - Key decisions, technical concepts and code patterns
   - Specific details like:
     - file names
     - full code snippets
     - function signatures
     - file edits
   - Errors that you ran into and how you fixed them
   - Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.
2. Double-check for technical accuracy and completeness, addressing each required element thoroughly.

Your summary should include the following sections:

1. Primary Request and Intent: Capture all of the user's explicit requests and intents in detail
2. **Target Paths**: Any file paths, directory paths, or URLs that are the subject of work. Include the FULL absolute path, not just filenames. Example: "Modifying E:\\Git\\miniClaudeCode-rust\\src\\compact.rs"
3. Key Technical Concepts: List all important technical concepts, technologies, and frameworks discussed.
4. Files and Code Sections: Enumerate specific files and code sections examined, modified, or created. Pay special attention to the most recent messages and include full code snippets where applicable and include a summary of why this file read or edit is important.
4. Errors and fixes: List all errors that you ran into, and how you fixed them. Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.
5. Problem Solving: Document problems solved and any ongoing troubleshooting efforts.
6. All user messages: List ALL user messages that are not tool results. These are critical for understanding the users' feedback and changing intent.
7. Completed Tasks: List all tasks, commands, and operations that were completed during the conversation. For each completed task, briefly note what was done and the result (including any errors or partial success). This is critical for preventing re-execution of work that was already done.
8. Pending Tasks: Outline any pending tasks that you have explicitly been asked to work on. Clearly distinguish pending tasks from completed ones.
9. Current Work: Describe in detail precisely what was being worked on immediately before this summary request, paying special attention to the most recent messages from both user and assistant. Include file names and code snippets where applicable.
10. Optional Next Step: List the next step that you will take that is related to the most recent work you were doing. IMPORTANT: ensure that this step is DIRECTLY in line with the user's most recent explicit requests, and the task you were working on immediately before this summary request. If your last task was concluded, then only list next steps if they are explicitly in line with the users request. Do not start on tangential requests or really old requests that were already completed without confirming with the user first.
                       If there is a next step, include direct quotes from the most recent conversation showing exactly what task you were working on and where you left off. This should be verbatim to ensure there's no drift in task interpretation.

Here's an example of how your output should be structured:

<example>
<analysis>
[Your thought process, ensuring all points are covered thoroughly and accurately]
</analysis>

<summary>
1. Primary Request and Intent:
   [Detailed description]

2. Target Paths:
   - [Full absolute path 1 - description of what's being done]
   - [Full absolute path 2 - description]

3. Key Technical Concepts:
   - [Concept 1]
   - [Concept 2]
   - [...]

4. Files and Code Sections:
   - [File Name 1]
      - [Summary of why this file is important]
      - [Summary of the changes made to this file, if any]
      - [Important Code Snippet]
   - [File Name 2]
      - [Important Code Snippet]
   - [...]

5. Errors and fixes:
    - [Detailed description of error 1]:
      - [How you fixed the error]
      - [User feedback on the error if any]
    - [...]

6. Problem Solving:
   [Description of solved problems and ongoing troubleshooting]

7. All user messages:
    - [Detailed non tool use user message]
    - [...]

8. Completed Tasks:
   - [Task 1 completed]: [Brief description of what was done and result]
   - [Task 2 completed]: [Brief description of what was done and result]
   - [...]

9. Pending Tasks:
   - [Task 1]
   - [Task 2]
   - [...]

10. Current Work:
   [Precise description of current work]

11. Optional Next Step:
   [Optional Next step to take]

</summary>
</example>

Please provide your summary based on the conversation so far, following this structure and ensuring precision and thoroughness in your response.

There may be additional summarization instructions provided in the included context. If so, remember to follow these instructions when creating the above summary. Examples of instructions include:
<example>
## Compact Instructions
When summarizing the conversation focus on typescript code changes and also remember the mistakes you made and how you fixed them.
</example>

<example>
# Summary instructions
When you are using compact - please focus on test output and code changes. Include file reads verbatim.
</example>
"#;

/// Partial compact prompt template (matches upstream PARTIAL_COMPACT_PROMPT).
/// Summarizes only the recent portion of the conversation.
#[allow(dead_code)]
const PARTIAL_COMPACT_PROMPT: &str = r#"Your task is to create a detailed summary of the RECENT portion of the conversation — the messages that follow earlier retained context. The earlier messages are being kept intact and do NOT need to be summarized. Focus your summary on what was discussed, learned, and accomplished in the recent messages only.

Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts and ensure you've covered all necessary points. In your analysis process:

1. Analyze the recent messages chronologically. For each section thoroughly identify:
   - The user's explicit requests and intents
   - Your approach to addressing the user's requests
   - Key decisions, technical concepts and code patterns
   - Specific details like:
     - file names
     - full code snippets
     - function signatures
     - file edits
   - Errors that you ran into and how you fixed them
   - Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.
2. Double-check for technical accuracy and completeness, addressing each required element thoroughly.

Your summary should include the following sections:

1. Primary Request and Intent: Capture the user's explicit requests and intents from the recent messages
2. Key Technical Concepts: List important technical concepts, technologies, and frameworks discussed recently.
3. Files and Code Sections: Enumerate specific files and code sections examined, modified, or created. Include full code snippets where applicable and include a summary of why this file read or edit is important.
4. Errors and fixes: List errors encountered and how they were fixed.
5. Problem Solving: Document problems solved and any ongoing troubleshooting efforts.
6. All user messages: List ALL user messages from the recent portion that are not tool results.
7. Completed Tasks: List tasks completed in the recent messages. For each, briefly note what was done and the result.
8. Pending Tasks: Outline any pending tasks from the recent messages.
9. Current Work: Describe precisely what was being worked on immediately before this summary request.
10. Optional Next Step: List the next step related to the most recent work. Include direct quotes from the most recent conversation.

Here's an example of how your output should be structured:

<example>
<analysis>
[Your thought process, ensuring all points are covered thoroughly and accurately]
</analysis>

<summary>
1. Primary Request and Intent:
   [Detailed description]

2. Key Technical Concepts:
   - [Concept 1]
   - [Concept 2]

3. Files and Code Sections:
   - [File Name 1]
      - [Summary of why this file is important]
      - [Important Code Snippet]

4. Errors and fixes:
    - [Error description]:
      - [How you fixed it]

5. Problem Solving:
   [Description]

6. All user messages:
    - [Detailed non tool use user message]

7. Completed Tasks:
   - [Task 1 completed]: [Brief description]
   - [...]

8. Pending Tasks:
   - [Task 1]
   - [...]

9. Current Work:
   [Precise description of current work]

10. Optional Next Step:
   [Optional Next step to take]

</summary>
</example>

Please provide your summary based on the RECENT messages only (after the retained earlier context), following this structure and ensuring precision and thoroughness in your response.
"#;

/// Partial compact prompt for "up_to" direction (matches upstream).
/// Summary will precede kept recent messages. Reserved for future LLM-driven partial compaction.
#[allow(dead_code)]
const PARTIAL_COMPACT_UP_TO_PROMPT: &str = r#"Your task is to create a detailed summary of this conversation. This summary will be placed at the start of a continuing session; newer messages that build on this context will follow after your summary (you do not see them here). Summarize thoroughly so that someone reading only your summary and then the newer messages can fully understand what happened and continue the work.

Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts and ensure you've covered all necessary points. In your analysis process:

1. Chronologically analyze each message and section of the conversation. For each section thoroughly identify:
   - The user's explicit requests and intents
   - Your approach to addressing the user's requests
   - Key decisions, technical concepts and code patterns
   - Specific details like:
     - file names
     - full code snippets
     - function signatures
     - file edits
   - Errors that you ran into and how you fixed them
   - Pay special attention to specific user feedback that you received, especially if the user told you to do something differently.
2. Double-check for technical accuracy and completeness, addressing each required element thoroughly.

Your summary should include the following sections:

1. Primary Request and Intent: Capture the user's explicit requests and intents in detail
2. Key Technical Concepts: List important technical concepts, technologies, and frameworks discussed.
3. Files and Code Sections: Enumerate specific files and code sections examined, modified, or created. Include full code snippets where applicable and include a summary of why this file read or edit is important.
4. Errors and fixes: List errors encountered and how they were fixed.
5. Problem Solving: Document problems solved and any ongoing troubleshooting efforts.
6. All user messages: List ALL user messages that are not tool results.
7. Pending Tasks: Outline any pending tasks.
8. Work Completed: Describe what was accomplished by the end of this portion.
9. Context for Continuing Work: Summarize any context, decisions, or state that would be needed to understand and continue the work in subsequent messages.

Here's an example of how your output should be structured:

<example>
<analysis>
[Your thought process, ensuring all points are covered thoroughly and accurately]
</analysis>

<summary>
1. Primary Request and Intent:
   [Detailed description]

2. Key Technical Concepts:
   - [Concept 1]
   - [Concept 2]

3. Files and Code Sections:
   - [File Name 1]
      - [Summary of why this file is important]
      - [Important Code Snippet]

4. Errors and fixes:
    - [Error description]:
      - [How you fixed it]

5. Problem Solving:
   [Description]

6. All user messages:
    - [Detailed non tool use user message]

7. Pending Tasks:
   - [Task 1]

8. Work Completed:
   [Description of what was accomplished]

9. Context for Continuing Work:
   [Key context, decisions, or state needed to continue the work]

</summary>
</example>

Please provide your summary following this structure, ensuring precision and thoroughness in your response.
"#;

/// Trailer appended to all compact prompts to reinforce the no-tools constraint.
const NO_TOOLS_TRAILER: &str = "\n\nREMINDER: Do NOT call any tools. Respond with plain text only — an <analysis> block followed by a <summary> block. Tool calls will be rejected and you will fail the task.";

/// System prompt for compact (matches upstream).
const COMPACT_SYSTEM_PROMPT: &str = "You are a helpful AI assistant tasked with summarizing conversations.";

/// Iterative variant used when a previous summary already exists.
const COMPACT_USER_PROMPT_ITERATIVE: &str = r#"Below is the previous summary followed by new conversation messages. Update the summary by:
- Merging new information into existing fields
- Updating progress on tasks mentioned in the previous summary
- Moving tasks from "Pending Tasks" to "Completed Tasks" when they are finished — this is critical to prevent re-execution of work already done
- Adding new files, errors, or decisions that appeared in the new messages
- Removing information that is no longer relevant
- Preserving all user messages (add new ones, keep existing ones)
- Preserving code snippets, function signatures, and file edits from the previous summary (do NOT summarize them away -- keep them verbatim)
- CRITICAL: Preserve all Target Paths from the previous summary with their FULL absolute paths. Never abbreviate or drop file paths.
- CRITICAL: Preserve the Primary Request and Intent section completely. Do not summarize it further or lose any detail.
- If the previous summary has a Target Paths section, keep it and update it with any new paths or completed paths.
- When the previous summary is already condensed, do NOT further compress it. Keep all existing detail and only merge new messages.

Previous Summary:
{previous_summary}

Write your analysis in <analysis> tags, then the updated summary in <summary> tags with the same structure as the previous summary."#;

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
        let mut search_from = 0;
        while let Some(rel_start) = result[search_from..].find(&json_key) {
            let start = search_from + rel_start;
            let after_key = &result[start + json_key.len()..];
            let trimmed = after_key.trim_start();
            if !trimmed.starts_with(':') {
                search_from = start + json_key.len();
                continue;
            }
            let after_colon = trimmed[1..].trim_start();
            if after_colon.starts_with('"') {
                if let Some(end_offset) = find_closing_quote(&after_colon[1..]) {
                    // value_start points at the colon (:) in the original string.
                    let value_start = start + json_key.len()
                        + (after_key.len() - after_key.trim_start().len());
                    // after_colon points at the opening quote.
                    // Compute after_colon's position in the original string:
                    //   after_key starts at (start + json_key.len())
                    //   after_colon is offset from after_key by:
                    //     +1 (skip the colon in trimmed[1..])
                    //     +whitespace (trimmed[1..].trim_start())
                    let ws_after_colon = trimmed[1..].len() - trimmed[1..].trim_start().len();
                    let after_colon_pos = start + json_key.len()
                        + (after_key.len() - after_key.trim_start().len()) + 1 + ws_after_colon;
                    // Closing quote position = after_colon_pos + 1 (skip opening quote) + end_offset
                    let value_end = after_colon_pos + 1 + end_offset;
                    let replacement = ": \"[REDACTED]\"";
                    result.replace_range(value_start..=value_end, replacement);
                    // Advance past the replacement to avoid re-matching the same key
                    search_from = value_start + replacement.len();
                } else {
                    break;
                }
            } else {
                search_from = start + json_key.len();
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

/// Finds a safe boundary in the flat messages list where dropping won't orphan
/// tool_use/tool_result pairs. Walks forward to detect if any tool_result
/// in the kept region references a tool_use that would be dropped, and walks
/// backward to detect if any tool_use in the kept region has a tool_result
/// in the drop region.
///
/// Returns the adjusted start index for the drop (all messages from this
/// index onward are safe to drop without orphaning pairs).
fn find_safe_drop_boundary(messages: &[Message], drop_start_idx: usize) -> usize {
    if drop_start_idx == 0 || drop_start_idx >= messages.len() {
        return drop_start_idx;
    }

    // Build a map of tool_use_id -> index for tool_use messages.
    // This lets us quickly look up whether a tool_result's tool_use
    // is in the kept or dropped region.
    use std::collections::HashMap;
    let mut tool_use_index: HashMap<String, usize> = HashMap::new();
    for (i, msg) in messages.iter().enumerate() {
        if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
            for block in blocks {
                tool_use_index.insert(block.id.clone(), i);
            }
        }
    }

    let mut cut_idx = drop_start_idx;

    // Walk FORWARD from cut_idx: detect if any tool_result in the kept
    // region (i >= cut_idx) references a tool_use that is being dropped
    // (index < cut_idx). If so, move cut_idx backward to include that
    // tool_use in the drop region.
    for i in cut_idx..messages.len() {
        if let MessageContent::ToolResultBlocks(blocks) = &messages[i].content {
            for block in blocks {
                if let Some(&tool_use_msg_idx) = tool_use_index.get(&block.tool_use_id) {
                    if tool_use_msg_idx < cut_idx {
                        // tool_result at i references tool_use at tool_use_msg_idx,
                        // but tool_use_msg_idx < cut_idx (would be kept) while i >= cut_idx (would be dropped).
                        // This orphans the tool_result. Move cut_idx backward to include the tool_use.
                        cut_idx = tool_use_msg_idx;
                    }
                }
            }
        }
    }

    // Walk BACKWARD from cut_idx: detect if any tool_use in the kept
    // region (i < cut_idx) has a tool_result in the drop region
    // (j >= cut_idx). If so, move cut_idx forward to include that tool_result.
    for i in (0..cut_idx).rev() {
        if let MessageContent::ToolUseBlocks(blocks) = &messages[i].content {
            for block in blocks {
                // Check if this tool_use has a result in the drop region
                for j in cut_idx..messages.len() {
                    if let MessageContent::ToolResultBlocks(result_blocks) = &messages[j].content {
                        for result_block in result_blocks {
                            if result_block.tool_use_id == block.id {
                                // tool_use at i has its result at j, and j >= cut_idx (would be dropped).
                                // Move cut_idx forward to include the result.
                                cut_idx = j + 1;
                            }
                        }
                    }
                }
            }
        }
    }

    cut_idx
}

/// Compress messages using LLM summary generation.
/// This is the primary compaction method -- replaces older truncation-based approaches.
/// If `last_summary` is provided, uses iterative update prompt.
/// Includes a PTL (prompt-too-long) retry loop: if the compact API call itself
/// exceeds the context limit, progressively drop oldest messages and retry,
/// up to MAX_PTL_RETRIES times.
const MAX_PTL_RETRIES: usize = 3;

pub async fn compact_conversation(
    messages: &[Message],
    client: &reqwest::Client,
    model: &str,
    api_key: &str,
    base_url: &str,
    trigger: CompactTrigger,
    is_auto: bool,
    last_summary: Option<&str>,
    transcript_path: Option<&str>,
) -> anyhow::Result<CompactionResult> {
    if messages.is_empty() {
        anyhow::bail!("No messages to compact");
    }

    // PTL retry loop: try compaction, and if the API itself rejects due to
    // prompt-too-long, progressively drop the oldest messages and retry.
    let mut current_messages = messages.to_vec();
    let mut last_err: Option<anyhow::Error> = None;

    for _attempt in 0..=MAX_PTL_RETRIES {
        let result = do_compact_llm_call(&current_messages, client, model, api_key, base_url, trigger, is_auto, last_summary, transcript_path).await;
        match result {
            Ok(r) => return Ok(r),
            Err(e) => {
                let err_str = e.to_string();
                if !crate::error_types::is_context_length_error(&err_str) {
                    // Non-PTL error — bail out
                    return Err(e);
                }

                last_err = Some(e);

                // Prompt-too-long: try to drop oldest messages
                let drop_count = if let Some((actual, max)) = crate::error_types::parse_prompt_too_long_token_gap(&err_str) {
                    // Drop just enough to cover the token gap
                    let needed = actual.saturating_sub(max);
                    let total = estimate_total_tokens(&current_messages);
                    if total > 0 {
                        let fraction = (needed as f64 / total as f64).max(0.20);
                        let count = (current_messages.len() as f64 * fraction) as usize;
                        count.max(1).min(current_messages.len() / 2)
                    } else {
                        current_messages.len() / 5 // fallback: drop 20%
                    }
                } else {
                    // Gap unparseable: drop 20% fallback
                    current_messages.len() / 5
                };

                if drop_count < 1 || current_messages.len() - drop_count < 2 {
                    break; // not enough messages left
                }

                // Drop the oldest messages, but use find_safe_drop_boundary to
                // avoid splitting tool_use/tool_result pairs.
                // Skip system message at index 0 if present.
                let has_system = current_messages.first().map(|m| m.role == MessageRole::System).unwrap_or(false);
                let proposed_start = if has_system && current_messages.len() > 2 {
                    1 + drop_count.min(current_messages.len() - 2)
                } else {
                    drop_count.min(current_messages.len() - 2)
                };
                let start = find_safe_drop_boundary(&current_messages, proposed_start);

                current_messages = current_messages[start..].to_vec();
                if current_messages.len() < 2 {
                    break;
                }
            }
        }
    }

    Err(anyhow::anyhow!("Compact API error: prompt too long after {} retries, {}", MAX_PTL_RETRIES, last_err.unwrap_or_else(|| anyhow::anyhow!("unknown error"))))
}

/// Single attempt at the LLM compaction API call.
async fn do_compact_llm_call(
    messages: &[Message],
    client: &reqwest::Client,
    model: &str,
    api_key: &str,
    base_url: &str,
    trigger: CompactTrigger,
    is_auto: bool,
    last_summary: Option<&str>,
    transcript_path: Option<&str>,
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
                        let crate::context::ToolResultContent::Text { text } = content;
                        *text = redact_sensitive_text(text);
                    }
                }
            }
            MessageContent::Attachment(text) => {
                *text = redact_sensitive_text(text);
            }
            _ => {}
        }
    }

    // Build the API request: send all messages with a summary instruction
    let api_messages: Vec<serde_json::Value> = pruned_messages
        .iter()
        .filter_map(|msg| message_to_api(msg))
        .collect();

    // Generate structured metadata from the pruned messages and inject it into
    // the compact prompt. This ensures the LLM sees an explicit inventory of
    // files, tool calls, and user messages even when it can't fully parse
    // the entire conversation history due to token limits.
    let structured_meta = entries_to_summary_text(&pruned_messages);
    let meta_prefix = if !structured_meta.is_empty() {
        format!("## Structured context from {} conversation messages:\n{}\n\n", pruned_messages.len(), structured_meta)
    } else {
        String::new()
    };

    // Choose prompt based on whether we have a previous summary
    // All prompts are wrapped with NO_TOOLS_PREAMBLE + NO_TOOLS_TRAILER
    // to prevent the model from wasting a turn on tool calls.
    let user_prompt = if let Some(summary) = last_summary {
        format!("{}{}\n{}\n{}", NO_TOOLS_PREAMBLE, meta_prefix, COMPACT_USER_PROMPT_ITERATIVE.replace("{previous_summary}", summary), NO_TOOLS_TRAILER)
    } else {
        format!("{}{}\n{}\n{}", NO_TOOLS_PREAMBLE, meta_prefix, BASE_COMPACT_PROMPT, NO_TOOLS_TRAILER)
    };

    // Build the payload
    let mut payload = serde_json::Map::new();
    payload.insert("model".to_string(), serde_json::json!(model));
    payload.insert("max_tokens".to_string(), serde_json::json!(20000));
    // Build system prompt with cache_control for prompt caching efficiency
    let mut system_json = serde_json::json!([{"type": "text", "text": COMPACT_SYSTEM_PROMPT}]);
    crate::prompt_caching::cache_system_prompt(&mut system_json);
    payload.insert("system".to_string(), system_json);
    // Disable extended thinking during compaction to prevent wasting output
    // tokens on thinking blocks. The summary needs all available tokens.
    payload.insert(
        "thinking".to_string(),
        serde_json::json!({"type": "disabled"}),
    );

    // Append the summary prompt as the final user message after the conversation history
    let mut all_messages = api_messages;
    all_messages.push(serde_json::json!({
        "role": "user",
        "content": [{"type": "text", "text": user_prompt}]
    }));

    // Apply prompt caching to reduce input token costs on compaction calls.
    // This reuses cached prefixes from the previous conversation when possible.
    crate::prompt_caching::apply_prompt_caching(&mut all_messages, "5m");

    payload.insert("messages".to_string(), serde_json::json!(all_messages));

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

    // Extract the summary text from the response, strip analysis blocks,
    // extract <summary> content, and redact sensitive info
    let summary_text = extract_summary_text(&body)
        .map(|t| extract_summary_from_compact_output(&t))
        .map(|t| redact_sensitive_text(&t))
        .ok_or_else(|| anyhow::anyhow!("No summary text in compact response"))?;

    // Build the compaction result
    let boundary = make_compact_boundary_message(trigger, pre_compact_tokens);

    // Match upstream's getCompactUserSummaryMessage: add transcript path
    // for detail recovery and recentMessagesPreserved notice.
    let summary_content = format!(
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n\
         [Previous conversation summary ({} tokens compressed)]\n\n{}",
        pre_compact_tokens, summary_text
    );
    let summary_content = if let Some(tp) = transcript_path {
        format!("{}\n\nIf you need specific details from before compaction (like exact code snippets, error messages, or content you generated), read the full transcript at: {}", summary_content, tp)
    } else {
        summary_content
    };
    let summary_content = format!("{}\n\nRecent messages are preserved verbatim.\n\nContinue the conversation from where it left off without asking the user any further questions. Resume directly \u{2014} do not acknowledge the summary, do not recap what was happening, do not preface with \"I'll continue\" or similar. Pick up the last task as if the break never happened.", summary_content);
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

/// Strip `<analysis>...</analysis>` blocks and extract `<summary>...</summary>` content
/// from the LLM's compaction response.
/// If no `<summary>` tags are found, returns the full text with analysis blocks removed.
fn extract_summary_from_compact_output(text: &str) -> String {
    fn analysis_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?s)<analysis>.*?</analysis>").unwrap())
    }
    fn summary_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?s)<summary>(.*?)</summary>").unwrap())
    }

    // Strip <analysis>...</analysis> entirely
    let cleaned = analysis_re().replace_all(text, "");

    // Extract content from <summary>...</summary>
    if let Some(caps) = summary_re().captures(&cleaned) {
        return caps[1].trim().to_string();
    }

    // Fallback: if no <summary> tags, return cleaned text
    cleaned.trim().to_string()
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
        MessageContent::Attachment(text) => Some(serde_json::json!({
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

// --- SM-compact (session memory compaction without API call) ---

/// Attempt SM-compact: use session memory as the compaction summary
/// instead of calling the LLM API. Returns Some(result) if session memory
/// exists and has content, None otherwise.
///
/// This follows the official Claude Code approach: when session-memory.md
/// contains extracted context, use it directly as the summary, skipping
/// the expensive LLM compaction call entirely.
pub fn try_sm_compact(
    context: &mut ConversationContext,
    session_memory: Option<&SessionMemory>,
    trigger: CompactTrigger,
    transcript_path: Option<&str>,
) -> Option<CompactStats> {
    // Check if session memory is available and has content
    let mem = session_memory?;
    let mem_content = mem.format_for_prompt();
    if mem_content.is_empty() {
        return None;
    }

    let messages = context.messages().to_vec();
    let entries_before = context.len();
    let tokens_before = estimate_total_tokens(&messages);

    eprintln!("[sm-compact] Using session memory as summary (skipping LLM API call)");

    // Build the compact boundary marker
    let boundary = make_compact_boundary_message(trigger, tokens_before);

    // Format the session memory as a summary message
    // Match upstream's getCompactUserSummaryMessage: add transcript path
    // for detail recovery and recentMessagesPreserved notice.
    // Also inject structured metadata (file paths, tool calls) from the
    // messages being compacted so the agent doesn't lose target paths.
    let structured_meta = entries_to_summary_text(&messages);
    let meta_section = if !structured_meta.is_empty() {
        format!("\n\n## Structured context from compacted messages:\n{}", structured_meta)
    } else {
        String::new()
    };
    // Cap session memory content at ~40K tokens to prevent context overflow.
    // Matches upstream's DEFAULT_SM_COMPACT_CONFIG.maxTokens = 40_000.
    const MAX_SESSION_MEMORY_TOKENS: usize = 40_000;
    let sm_tokens = estimate_tokens(&mem_content);
    let mem_content_for_summary = if sm_tokens > MAX_SESSION_MEMORY_TOKENS {
        let char_limit = MAX_SESSION_MEMORY_TOKENS * 4;
        let chars: Vec<char> = mem_content.chars().collect();
        if chars.len() > char_limit {
            let mut truncated: String = chars[..char_limit].iter().collect();
            if let Some(nl) = truncated.rfind('\n') {
                if nl > char_limit / 2 {
                    truncated.truncate(nl);
                }
            }
            eprintln!("[sm-compact] Session memory truncated: {} tokens -> {} token limit", sm_tokens, MAX_SESSION_MEMORY_TOKENS);
            format!("{}\n\n[... session memory truncated for length. Read the full session memory file for details ...]", truncated)
        } else {
            mem_content.clone()
        }
    } else {
        mem_content.clone()
    };

    let summary_content = format!(
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n\
         [Previous conversation summary ({} tokens compressed, SM-compact)]\n\n{}{}",
        tokens_before, mem_content_for_summary, meta_section
    );
    let summary_content = if let Some(tp) = transcript_path {
        format!("{}\n\nIf you need specific details from before compaction (like exact code snippets, error messages, or content you generated), read the full transcript at: {}", summary_content, tp)
    } else {
        summary_content
    };
    let summary_content = format!("{}\n\nRecent messages are preserved verbatim.\n\nContinue the conversation from where it left off without asking the user any further questions. Resume directly \u{2014} do not acknowledge the summary, do not recap what was happening, do not preface with \"I'll continue\" or similar. Pick up the last task as if the break never happened.", summary_content);
    let summary = Message::new(
        MessageRole::User,
        MessageContent::Summary(summary_content),
    );

    // Keep the last few messages as tail to maintain context continuity.
    // 4 was too small: a single tool_use + tool_result = 2 messages, so 4 only
    // preserved 2 recent tool pairs. After SM-compact the model would forget the
    // tool results from just 2 turns back, causing re-execution.
    // 8 gave 4 tool pairs (~2 turns of back-and-forth) which matched upstream's
    // keepLast default in SmartCompact.
    // 12 gives 6 tool pairs (~3 turns) — better context retention after compaction
    // without excessive token usage.
    const TAIL_SIZE: usize = 12;
    let tail_start = messages.len().saturating_sub(TAIL_SIZE);
    let mut tail: Vec<Message> = messages[tail_start..].to_vec();

    // Adjust tail backwards to include missing tool_use blocks.
    // If tail starts with a ToolResultBlocks that references a tool_use NOT in the tail,
    // walk backwards to include the corresponding tool_use message.
    // This matches upstream's adjustIndexToPreserveAPIInvariants.
    let mut missing_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in &tail {
        if let MessageContent::ToolResultBlocks(blocks) = &msg.content {
            for block in blocks {
                missing_ids.insert(block.tool_use_id.clone());
            }
        }
    }
    for msg in &tail {
        if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
            for block in blocks {
                missing_ids.remove(&block.id);
            }
        }
    }
    if !missing_ids.is_empty() {
        for i in (0..tail_start).rev() {
            if let MessageContent::ToolUseBlocks(blocks) = &messages[i].content {
                let mut has_match = false;
                for block in blocks {
                    if missing_ids.remove(&block.id) {
                        has_match = true;
                    }
                }
                if has_match {
                    tail.insert(0, messages[i].clone());
                }
            }
            if missing_ids.is_empty() {
                break;
            }
        }
        if !missing_ids.is_empty() {
            eprintln!("[sm-compact] {} tool_use blocks missing in tail, will be cleaned by validate_tool_pairing", missing_ids.len());
        }
    }

    let mut new_messages = vec![boundary, summary];
    new_messages.extend(tail);

    let post_tokens = estimate_total_tokens(&new_messages);
    let tokens_saved = tokens_before.saturating_sub(post_tokens);

    context.replace_messages(new_messages);
    context.validate_tool_pairing();
    context.fix_role_alternation();

    let entries_after = context.len();
    let tokens_after = estimate_total_tokens(context.messages());

    Some(CompactStats {
        phase: CompactPhase::None,
        entries_before,
        entries_after,
        tokens_after,
        estimated_tokens_saved: tokens_saved,
        estimated_tokens_before: tokens_before,
        estimated_tokens_after: tokens_after,
        post_compact_tokens: post_tokens,
    })
}

// --- Partial compact (directional compaction) ---

/// Perform partial compaction in the specified direction around a pivot index.
///
/// - `UpTo`: Compact messages 0..pivot, keeping messages pivot..end.
///   Useful when a specific recent message is the focus point.
/// - `From`: Compact messages pivot..end, keeping messages 0..pivot.
///   Preserves early context (initial instructions, setup) while
///   summarizing the middle/large portion. The very last N messages
///   are always preserved to maintain recent context.
///
/// This is a lightweight, non-LLM version of partial compaction that
/// summarizes old tool results in the targeted range (matching the
/// 3-pass pre-pruning approach).
pub fn partial_compact(
    context: &mut ConversationContext,
    direction: PartialCompactDirection,
    pivot_index: usize,
    transcript_path: Option<&str>,
    conclusions: &[String],
) -> PartialCompactionResult {
    let messages = context.messages().to_vec();
    let entries_before = context.len();
    let tokens_before = estimate_total_tokens(&messages);

    let pivot = pivot_index.min(messages.len());

    let (summary_range, keep_range) = match direction {
        PartialCompactDirection::UpTo => {
            // Summarize 0..pivot, keep pivot..end
            let summary: Vec<Message> = messages[..pivot].to_vec();
            let keep: Vec<Message> = messages[pivot..].to_vec();
            (summary, keep)
        }
        PartialCompactDirection::From => {
            // Keep 0..pivot + last N, summarize pivot..(end-N)
            const KEEP_LAST: usize = 3;
            let last_keep_start = messages.len().saturating_sub(KEEP_LAST);
            if last_keep_start <= pivot {
                // Nothing to summarize -- keep everything
                return PartialCompactionResult {
                    boundary: make_compact_boundary_message(CompactTrigger::Manual, tokens_before),
                    summary: Message::new(MessageRole::User, MessageContent::Summary(
                        format!("[No messages to summarize, {} tokens]", tokens_before),
                    )),
                    entries_before,
                    entries_after: entries_before,
                    pre_compact_tokens: tokens_before,
                    post_compact_tokens: tokens_before,
                };
            }
            let mut keep: Vec<Message> = messages[..pivot].to_vec();
            keep.extend(messages[last_keep_start..].to_vec());
            let summary: Vec<Message> = messages[pivot..last_keep_start].to_vec();
            (summary, keep)
        }
    };

    // Generate a lightweight summary from the range
    // Use token count and message count as the summary content
    let summary_tokens = estimate_total_tokens(&summary_range);
    let summary_msg_count = summary_range.len();

    // Generate detailed summary matching Go's entriesToSummaryText
    let summary_text = entries_to_summary_text(&summary_range);

    eprintln!(
        "[partial-compact {}] Summarized {} messages ({} tokens), keeping {} messages",
        direction, summary_msg_count, summary_tokens, keep_range.len()
    );

    // Build the result
    let boundary = make_compact_boundary_message(CompactTrigger::Manual, tokens_before);

    // Match upstream's getCompactUserSummaryMessage: add transcript path
    // for detail recovery and recentMessagesPreserved notice.
    let mut summary_content = format!(
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n\
         [Previous conversation summary ({} tokens compressed, partial-compact {})]\n\n{}",
        tokens_before, direction, summary_text
    );

    // Include tool state tracker conclusions (what the agent claimed was done).
    if !conclusions.is_empty() {
        summary_content.push_str("\n## Completed Work\n");
        for c in conclusions {
            summary_content.push_str(&format!("- {}\n", c));
        }
        summary_content.push('\n');
    }

    let summary_content = if let Some(tp) = transcript_path {
        format!("{}\n\nIf you need specific details from before compaction (like exact code snippets, error messages, or content you generated), read the full transcript at: {}", summary_content, tp)
    } else {
        summary_content
    };
    let summary_content = format!("{}\n\nRecent messages are preserved verbatim.\n\nContinue the conversation from where it left off without asking the user any further questions. Resume directly \u{2014} do not acknowledge the summary, do not recap what was happening, do not preface with \"I'll continue\" or similar. Pick up the last task as if the break never happened.", summary_content);
    let summary = Message::new(
        MessageRole::User,
        MessageContent::Summary(summary_content),
    );

    let mut new_messages = vec![boundary.clone(), summary.clone()];
    new_messages.extend(keep_range);

    context.replace_messages(new_messages);
    context.validate_tool_pairing();
    context.fix_role_alternation();

    let entries_after = context.len();
    let post_tokens = estimate_total_tokens(context.messages());

    PartialCompactionResult {
        boundary,
        summary,
        entries_before,
        entries_after,
        pre_compact_tokens: tokens_before,
        post_compact_tokens: post_tokens,
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

/// Compactor handles context compaction with LLM-based, SM-based, and fallback strategies
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
    /// Token count right after last successful compaction (cooldown tracking)
    post_compact_tokens: Option<usize>,
    /// Session memory for SM-compact (skip LLM API call when memory has content)
    session_memory: Option<std::sync::Arc<SessionMemory>>,
    /// Token count from the previous turn, for reactive compact detection
    prev_turn_tokens: Option<usize>,
    /// Threshold for reactive compact (token delta that triggers proactive compaction)
    reactive_compact_threshold: usize,
    /// Path to transcript file for detail recovery after compaction
    transcript_path: Option<String>,
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
            post_compact_tokens: None,
            session_memory: None,
            prev_turn_tokens: None,
            reactive_compact_threshold: 5000, // default: 5000 token delta
            transcript_path: None,
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

    /// Set session memory for SM-compact support.
    /// When session memory has content, compaction uses it directly as the summary
    /// instead of calling the LLM API.
    pub fn with_session_memory(mut self, memory: Option<std::sync::Arc<SessionMemory>>) -> Self {
        self.session_memory = memory;
        self
    }

    /// Set the transcript path for detail recovery after compaction
    pub fn set_transcript_path(&mut self, path: String) {
        self.transcript_path = Some(path);
    }

    /// Get the transcript path as Option<&str> for passing to compact functions
    pub fn get_transcript_path(&self) -> Option<&str> {
        self.transcript_path.as_deref()
    }

    /// Set the reactive compact threshold (token delta that triggers proactive compaction).
    pub fn with_reactive_threshold(mut self, threshold: usize) -> Self {
        self.reactive_compact_threshold = threshold;
        self
    }

    /// Get the compact threshold (for saving/restoring in reactive compact).
    pub fn get_compact_threshold(&self) -> f64 {
        self.compact_threshold
    }

    /// Set the compact threshold (for temporarily overriding in reactive compact).
    pub fn set_compact_threshold(&mut self, threshold: f64) {
        self.compact_threshold = threshold;
    }

    /// Check if reactive compact should be triggered due to a token spike.
    /// Compares current token count to the previous turn's count.
    /// If the delta exceeds the threshold, returns true (trigger compaction).
    /// Only triggers if not already planning to compact (i.e., `should_compact` is false).
    pub fn should_reactive_compact(&self, current_tokens: usize) -> bool {
        // If we're already above the normal threshold, no need for reactive detection
        if self.should_compact(current_tokens) {
            return false;
        }

        let Some(prev) = self.prev_turn_tokens else {
            return false;
        };

        let delta = current_tokens.saturating_sub(prev);
        if delta > self.reactive_compact_threshold {
            eprintln!(
                "[reactive-compact] Token spike detected: {} -> {} (+{} tokens, threshold: {})",
                prev, current_tokens, delta, self.reactive_compact_threshold
            );
            return true;
        }

        false
    }

    /// Update the previous turn token count. Call this after each turn to
    /// track token growth for reactive compact detection.
    pub fn update_prev_turn_tokens(&mut self, tokens: usize) {
        self.prev_turn_tokens = Some(tokens);
    }

    /// Check if compaction is needed based on token usage.
    /// Includes cooldown protection: after a successful compaction, skip further
    /// compaction until token count has grown by at least 25% from the post-compact level.
    pub fn should_compact(&self, total_tokens: usize) -> bool {
        let threshold = (self.max_tokens as f64 * self.compact_threshold) as usize;

        if total_tokens < threshold {
            return false;
        }

        // Cooldown: if we recently compacted, don't re-compact until context has grown
        // significantly (25% above post-compact level). This prevents immediate re-compaction
        // when the summary + a few new messages still exceeds the threshold.
        if let Some(post_tokens) = self.post_compact_tokens {
            let cooldown_threshold = post_tokens + (post_tokens / 4); // 25% growth
            if total_tokens < cooldown_threshold {
                return false;
            }
        }

        true
    }

    /// Preflight compaction for resumed sessions (synchronous, no LLM call).
    /// Uses simple truncation: keep the first entry (initial user message) plus
    /// the most recent entries. This is designed to be called before the agent
    /// loop starts, when the resumed transcript is too large.
    pub fn compact_preflight(&mut self, context: &mut ConversationContext) -> CompactStats {
        let messages = context.messages().to_vec();
        let entries_before = context.len();
        let tokens_before = estimate_total_tokens(&messages);

        if entries_before <= 12 {
            return CompactStats {
                phase: CompactPhase::None,
                entries_before,
                entries_after: entries_before,
                estimated_tokens_saved: 0,
                estimated_tokens_before: tokens_before,
                estimated_tokens_after: tokens_before,
                tokens_after: tokens_before,
                post_compact_tokens: tokens_before,
            };
        }

        // Truncation: keep first entry + last 10 entries
        let keep = 10;
        let first = messages[0..1].to_vec();
        let tail = messages[entries_before - keep..].to_vec();
        let new_messages: Vec<Message> = first.into_iter().chain(tail).collect();

        let tokens_after = estimate_total_tokens(&new_messages);
        let saved = tokens_before.saturating_sub(tokens_after);

        context.replace_messages(new_messages);
        context.validate_tool_pairing();
        context.fix_role_alternation();

        CompactStats {
            phase: CompactPhase::Truncated,
            entries_before,
            entries_after: context.len(),
            estimated_tokens_saved: saved,
            estimated_tokens_before: tokens_before,
            estimated_tokens_after: tokens_after,
            tokens_after,
            post_compact_tokens: tokens_after,
        }
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

        // Check if compaction is needed (includes cooldown protection)
        if !self.should_compact(tokens_before) {
            return CompactStats {
                phase: CompactPhase::None,
                entries_before,
                entries_after: entries_before,
                estimated_tokens_saved: 0,
                estimated_tokens_before: tokens_before,
                estimated_tokens_after: tokens_before,
                tokens_after: tokens_before,
                post_compact_tokens: tokens_before,
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
                tokens_after: tokens_before,
                post_compact_tokens: tokens_before,
            };
        }

        let effective_window = self.max_tokens.saturating_sub(20_000);
        let percent_used = (tokens_before as f64 / effective_window as f64 * 100.0).min(100.0) as u32;
        eprintln!(
            "[Compaction] Triggered: {} tokens ({}% of effective window)",
            tokens_before, percent_used
        );

        // Try SM-compact first (Feature 1): use session memory as summary, skipping LLM API call.
        // This is the preferred path when session memory is available and has content,
        // following the official Claude Code approach in sessionMemoryCompact.ts.
        if let Some(sm_stats) = try_sm_compact(context, self.session_memory.as_deref(), CompactTrigger::Auto, self.get_transcript_path()) {
            eprintln!(
                "[sm-compact] {}: {} -> {} entries, ~{} tokens saved",
                "auto",
                sm_stats.entries_before, sm_stats.entries_after, sm_stats.estimated_tokens_saved
            );
            // Store a lightweight summary from session memory for iterative updates
            if let Some(ref mem) = self.session_memory {
                let mem_content = mem.format_for_prompt();
                if !mem_content.is_empty() {
                    let preview: String = mem_content.chars().take(2000).collect();
                        self.last_summary = Some(format!("[SM-compact summary] {}", preview));
                }
            }
            // Reset LLM failure count on successful SM-compact
            self.llm_compact_failed_count = 0;
            self.phase = CompactPhase::None;

            // Record savings for anti-thrashing
            let savings = if sm_stats.estimated_tokens_before > 0 {
                sm_stats.estimated_tokens_saved as f64 / sm_stats.estimated_tokens_before as f64
            } else {
                0.0
            };
            self.last_compact_savings.push(savings);
            if self.last_compact_savings.len() > 2 {
                self.last_compact_savings.remove(0);
            }

            // Set cooldown from post-compact tokens
            self.post_compact_tokens = Some(sm_stats.tokens_after);

            return sm_stats;
        }

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
                self.get_transcript_path(),
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

                    // Replace context with boundary + summary + recent tail messages.
                    // This preserves the most recent conversation context while
                    // replacing older messages with the summary, matching the
                    // pattern used by hermes and the Go version.
                    // Keep the last few messages as "tail" to maintain context
                    // continuity after compaction.
                    // 4 was too small (only ~2 tool pairs); 8 was OK but still
                    // caused context loss. Use 12 for ~3 turns of back-and-forth.
                    const TAIL_SIZE: usize = 12;
                    let tail_start = messages.len().saturating_sub(TAIL_SIZE);
                    let mut tail: Vec<Message> = messages[tail_start..].to_vec();

                    // Adjust tail backwards to include missing tool_use blocks.
                    // If tail starts with a ToolResultBlocks that references a tool_use NOT in the tail,
                    // walk backwards to include the corresponding tool_use message.
                    // This prevents orphaned tool_results that validate_tool_pairing would delete,
                    // matching the same adjustment done in try_sm_compact.
                    let mut missing_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
                    for msg in &tail {
                        if let MessageContent::ToolResultBlocks(blocks) = &msg.content {
                            for block in blocks {
                                missing_ids.insert(block.tool_use_id.clone());
                            }
                        }
                    }
                    for msg in &tail {
                        if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
                            for block in blocks {
                                missing_ids.remove(&block.id);
                            }
                        }
                    }
                    if !missing_ids.is_empty() {
                        for i in (0..tail_start).rev() {
                            if let MessageContent::ToolUseBlocks(blocks) = &messages[i].content {
                                let mut has_match = false;
                                for block in blocks {
                                    if missing_ids.remove(&block.id) {
                                        has_match = true;
                                    }
                                }
                                if has_match {
                                    tail.insert(0, messages[i].clone());
                                }
                            }
                            if missing_ids.is_empty() {
                                break;
                            }
                        }
                    }

                    let mut new_messages = vec![result.boundary, result.summary];
                    new_messages.extend(tail);

                    // Calculate post_compact_tokens from the actual combined messages
                    // (boundary + summary + tail), not just boundary + summary.
                    // Previously this only counted boundary + summary, underestimating
                    // the token count and causing compaction thrashing when tail
                    // messages added significant tokens.
                    let post_tokens = estimate_total_tokens(&new_messages);

                    // Set cooldown: record post-compact token count from the
                    // full rebuilt message set (including tail), matching Go's
                    // agent_loop.go which calls BuildMessages() and measures
                    // the actual post-compact token count.
                    self.post_compact_tokens = Some(post_tokens);

                    context.replace_messages(new_messages);

                    // After replacing messages, validate tool pairing and fix
                    // role alternation to ensure API compatibility.
                    context.validate_tool_pairing();
                    context.fix_role_alternation();

                    let entries_after = context.len();
                    let tokens_after = estimate_total_tokens(context.messages());

                    return CompactStats {
                        phase: CompactPhase::None, // LLM compaction doesn't use legacy phases
                        entries_before,
                        entries_after,
                        estimated_tokens_saved: tokens_before.saturating_sub(tokens_after),
                        estimated_tokens_before: tokens_before,
                        estimated_tokens_after: tokens_after,
                        tokens_after,
                        post_compact_tokens: tokens_after,
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
                // Selectively clear read-only tool results (read_file, grep, glob, etc.)
                // while keeping all messages intact. This saves tokens without losing context.
                let mut msgs = context.messages().to_vec();
                selective_clear_read_only_tools(&mut msgs, "[read-only tool result cleared]");
                context.replace_messages(msgs);
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

        // Set cooldown: record post-compact token count
        self.post_compact_tokens = Some(tokens_after);

        CompactStats {
            phase,
            entries_before,
            entries_after,
            estimated_tokens_saved: tokens_saved,
            estimated_tokens_before: tokens_before,
            estimated_tokens_after: tokens_after,
            tokens_after,
            post_compact_tokens: tokens_after,
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
        self.post_compact_tokens = None;
        self.prev_turn_tokens = None;
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
                let truncated_content: String = content.chars().take(char_budget).collect();
                format!("...[truncated]...\n{}", truncated_content)
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
            let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0];
            assert_eq!(text, "same content here");
        } else {
            panic!("Expected ToolResultBlocks");
        }

        // Second result: duplicate of first -- replaced with reference
        if let MessageContent::ToolResultBlocks(blocks) = &messages[1].content {
            let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0];
            assert!(text.contains("[duplicate result, see tool_use_id tool-1]"));
        } else {
            panic!("Expected ToolResultBlocks");
        }

        // Third result: unique content, unchanged
        if let MessageContent::ToolResultBlocks(blocks) = &messages[2].content {
            let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0];
            assert_eq!(text, "different content");
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
                let crate::context::ToolResultContent::Text { text } = &block.content[0];
                assert_eq!(text, "content");
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
            let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0];
            assert!(text.starts_with("[read_file] -> ok,"));
            assert!(text.contains("3 lines"));
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
            let crate::context::ToolResultContent::Text { text } = &blocks[0].content[0];
            assert!(text.contains("[exec] -> error"));
            assert!(text.contains("2 lines"));
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

    #[test]
    fn test_redact_preserves_adjacent_content() {
        // Regression test: off-by-one in value_end calculation could corrupt
        // content after the redacted value (e.g. trailing "3" or closing quote).
        let input = r#"{"api_key": "sk-123", "name": "test"}"#;
        let output = redact_sensitive_text(input);
        assert_eq!(output, r#"{"api_key": "[REDACTED]", "name": "test"}"#);
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

        // Initially no savings recorded -- should not skip
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

    #[test]
    fn test_extract_summary_from_compact_output() {
        // Test with analysis + summary blocks
        let input = r#"Some preamble text
<analysis>
Reviewing message 1: user asked to do X
Reviewing message 2: assistant did Y
</analysis>
<summary>
1. Primary Request and Intent: User wanted X
2. Key Technical Concepts: Y
3. Files and Code Sections: Z
</summary>
Some trailing text"#;
        let result = extract_summary_from_compact_output(input);
        assert!(result.contains("1. Primary Request and Intent"));
        assert!(result.contains("3. Files and Code Sections"));
        assert!(!result.contains("<analysis>"));
        assert!(!result.contains("<summary>"));
        assert!(!result.contains("Reviewing message"));

        // Test with no tags -- should return cleaned text
        let plain = "Just a plain text summary without any tags.";
        let result2 = extract_summary_from_compact_output(plain);
        assert_eq!(result2, "Just a plain text summary without any tags.");

        // Test with only summary tags (no analysis)
        let summary_only = "<summary>\n1. Primary Request: Foo\n</summary>";
        let result3 = extract_summary_from_compact_output(summary_only);
        assert!(result3.contains("1. Primary Request: Foo"));
        assert!(!result3.contains("<summary>"));
    }

    // --- find_safe_drop_boundary tests ---

    #[test]
    fn test_find_safe_drop_boundary_no_tools() {
        // No tool pairs — boundary should be unchanged
        let msgs = vec![
            Message::new(MessageRole::System, MessageContent::Text("system".to_string())),
            Message::new(MessageRole::User, MessageContent::Text("user1".to_string())),
            Message::new(MessageRole::Assistant, MessageContent::Text("assistant1".to_string())),
            Message::new(MessageRole::User, MessageContent::Text("user2".to_string())),
        ];
        assert_eq!(find_safe_drop_boundary(&msgs, 2), 2);
        assert_eq!(find_safe_drop_boundary(&msgs, 1), 1);
        assert_eq!(find_safe_drop_boundary(&msgs, 3), 3);
    }

    #[test]
    fn test_find_safe_drop_boundary_orphan_tool_result() {
        // Proposed drop at index 2 (tool_result_1) would orphan its tool_use at index 1.
        // Boundary should shift backward to include the tool_use.
        let msgs = vec![
            Message::new(MessageRole::System, MessageContent::Text("system".to_string())),
            Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: "t1".to_string(),
                    name: "read_file".to_string(),
                    input: std::collections::HashMap::new(),
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "t1".to_string(),
                    content: vec![ToolResultContent::Text { text: "result".to_string() }],
                    is_error: false,
                }]),
            ),
            Message::new(MessageRole::User, MessageContent::Text("user".to_string())),
        ];
        // drop_start_idx=2 would drop tool_result_1 but keep tool_use_1 — bad.
        // Expected: boundary shifts to 1 (drop tool_use_1 instead, keeping tool_result_1).
        assert_eq!(find_safe_drop_boundary(&msgs, 2), 1);
    }

    #[test]
    fn test_find_safe_drop_boundary_orphan_tool_use() {
        // Proposed drop at index 1 only drops the system message.
        // The t1_use(1)/t1_result(2) pair is fully kept — no orphan.
        // So the boundary stays at 1.
        let msgs = vec![
            Message::new(MessageRole::System, MessageContent::Text("system".to_string())),
            Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: "t1".to_string(),
                    name: "read_file".to_string(),
                    input: std::collections::HashMap::new(),
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "t1".to_string(),
                    content: vec![ToolResultContent::Text { text: "result".to_string() }],
                    is_error: false,
                }]),
            ),
            Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: "t2".to_string(),
                    name: "edit_file".to_string(),
                    input: std::collections::HashMap::new(),
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "t2".to_string(),
                    content: vec![ToolResultContent::Text { text: "done".to_string() }],
                    is_error: false,
                }]),
            ),
        ];
        // drop_start_idx=1 only drops system — t1 pair fully kept, no adjustment.
        assert_eq!(find_safe_drop_boundary(&msgs, 1), 1);
    }

    #[test]
    fn test_find_safe_drop_boundary_drops_tool_use_keeps_result() {
        // Drop only t1_use (index 1) but keep t1_result (index 2) — orphan.
        // Forward walk detects t1_result references t1_use at index 1.
        // Since 1 >= 1 (tool_use in kept region) and tool_result in kept region,
        // this is NOT an orphan by the forward check (both in same region).
        // But the backward walk: t1_use at 1 < 1? No (1 < 1 is false).
        // Wait, the cut_idx stays at 1. The backward walk checks i < 1, which is just i=0.
        // t1_use is at index 1 which is >= 1, so it's not in the backward walk scope.
        // Let me create a clearer scenario: no system msg, drop t1_use but keep t1_result.
        let msgs = vec![
            Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: "t1".to_string(),
                    name: "read_file".to_string(),
                    input: std::collections::HashMap::new(),
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "t1".to_string(),
                    content: vec![ToolResultContent::Text { text: "result".to_string() }],
                    is_error: false,
                }]),
            ),
        ];
        // drop_start_idx=1: drop t1_use, keep t1_result — orphan!
        // Forward: i=1, t1_result, tool_use_idx=0. Is 0 < 1? YES. cut_idx=0.
        assert_eq!(find_safe_drop_boundary(&msgs, 1), 0);
    }

    #[test]
    fn test_find_safe_drop_boundary_safe_boundary() {
        // Messages: [t1_use(0), t1_result(1), t2_use(2), t2_result(3)]
        // Proposed drop at index 3 would drop t2_result but keep t2_use — orphan pair.
        // Forward walk detects: t2_result at 3 references t2_use at 2 (2 < 3, kept),
        // so orphan. Boundary shifts to 2 (drop both t2_use and t2_result).
        let msgs = vec![
            Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: "t1".to_string(),
                    name: "read_file".to_string(),
                    input: std::collections::HashMap::new(),
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "t1".to_string(),
                    content: vec![ToolResultContent::Text { text: "r1".to_string() }],
                    is_error: false,
                }]),
            ),
            Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: "t2".to_string(),
                    name: "edit_file".to_string(),
                    input: std::collections::HashMap::new(),
                }]),
            ),
            Message::new(
                MessageRole::User,
                MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                    tool_use_id: "t2".to_string(),
                    content: vec![ToolResultContent::Text { text: "r2".to_string() }],
                    is_error: false,
                }]),
            ),
        ];
        // drop_start_idx=3 keeps t1 pair, would drop t2_result but keep t2_use — orphan.
        // Boundary shifts to 2 to drop the full t2 pair together.
        assert_eq!(find_safe_drop_boundary(&msgs, 3), 2);

        // drop_start_idx=4 (drop nothing): safe, no adjustment.
        assert_eq!(find_safe_drop_boundary(&msgs, 4), 4);

        // drop_start_idx=2 (drop t2 pair): safe, no adjustment.
        assert_eq!(find_safe_drop_boundary(&msgs, 2), 2);
    }

    #[test]
    fn test_find_safe_drop_boundary_edge_cases() {
        // Edge case: drop_start_idx at boundaries
        let msgs = vec![
            Message::new(MessageRole::System, MessageContent::Text("system".to_string())),
            Message::new(MessageRole::User, MessageContent::Text("user".to_string())),
        ];
        assert_eq!(find_safe_drop_boundary(&msgs, 0), 0); // start of list
        assert_eq!(find_safe_drop_boundary(&msgs, 1), 1);
        assert_eq!(find_safe_drop_boundary(&msgs, 2), 2); // end of list
    }

    #[test]
    fn test_estimate_total_tokens_padding() {
        // 4/3 padding factor: estimate_total_tokens should return ceil(total * 4/3)
        // Single message: "Hello" = 5 chars / 4.0 = 1.25, ceil = 2, + 3 role = 5
        // Total = 5, padded = ceil(5 * 4/3) = ceil(6.67) = 7
        let messages = vec![
            Message::new(MessageRole::User, MessageContent::Text("Hello".to_string())),
        ];
        assert_eq!(estimate_total_tokens(&messages), 7);
    }
}
