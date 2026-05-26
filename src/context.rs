use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

// ─── Todo Types ───────────────────────────────────────────────────────────────

/// Status of a todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
}

/// A single todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

/// Thread-safe todo list shared between the agent loop and TodoWriteTool.
#[derive(Clone)]
pub struct TodoList {
    inner: Arc<RwLock<Vec<TodoItem>>>,
    /// Turns since TodoWrite was last called. Reset to 0 on Update.
    turns_since_last_write: Arc<std::sync::atomic::AtomicUsize>,
    /// Turns since the last idle reminder was shown. Reset to 0 when reminder shown.
    turns_since_last_remind: Arc<std::sync::atomic::AtomicUsize>,
}

impl TodoList {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Vec::new())),
            turns_since_last_write: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            turns_since_last_remind: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn update(&self, items: Vec<TodoItem>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = items;
        }
        self.turns_since_last_write
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns true if an idle TodoWrite reminder should be injected.
    /// Increments both turn counters; resets the "since last remind" counter
    /// when a reminder is due (to prevent spamming).
    pub fn increment_turn(&self) -> bool {
        const TURNS_SINCE_WRITE: usize = 10;
        const TURNS_BETWEEN_REMINDERS: usize = 10;

        let _prev_write = self.turns_since_last_write.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.turns_since_last_remind
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let since_write = self.turns_since_last_write.load(std::sync::atomic::Ordering::Relaxed);
        let since_remind = self.turns_since_last_remind.load(std::sync::atomic::Ordering::Relaxed);

        if since_write >= TURNS_SINCE_WRITE && since_remind >= TURNS_BETWEEN_REMINDERS {
            self.turns_since_last_remind
                .store(0, std::sync::atomic::Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Returns a nudge message when the model hasn't used TodoWrite for 10+ turns.
    pub fn build_idle_reminder(&self) -> String {
        String::from("The TodoWrite tool hasn't been used recently. If you're on tasks that would benefit from tracking progress, consider using the TodoWrite tool to update your task list. If your current task list is stale, update it. If you don't have a task list, create one for multi-step work.")
    }

    pub fn build_reminder(&self) -> String {
        let guard = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return String::new(),
        };
        if guard.is_empty() {
            return String::new();
        }
        let mut sb = String::from("\n## Current Tasks\n");
        for item in guard.iter() {
            let icon = match item.status {
                TodoStatus::Pending => "\u{25cb}",   // ○
                TodoStatus::InProgress => "\u{25d0}", // ◐
                TodoStatus::Completed => "\u{25cf}",  // ●
            };
            let active = item.active_form.as_deref().unwrap_or("");
            let active_suffix = if !active.is_empty() {
                format!(" ({})", active)
            } else {
                String::new()
            };
            sb.push_str(&format!(
                "  {} {}{} [{:?}]\n",
                icon, item.content, active_suffix, item.status
            ));
        }
        sb
    }
}

impl Default for TodoList {
    fn default() -> Self { Self::new() }
}

/// Callback type for todo list updates.
pub type TodoUpdateFunc = Arc<dyn Fn(Vec<TodoItem>) + Send + Sync>;


/// Unique ID for messages (used for transcript tracking and compact boundary relinking)
fn generate_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("msg-{}-{:x}", duration.as_secs(), duration.subsec_nanos())
}

fn generate_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
}

/// Role of a message in the conversation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl MessageRole {
    pub fn as_str(&self) -> &str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
        }
    }
}

/// What triggered a compaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactTrigger {
    Auto,
    Manual,
}

impl std::fmt::Display for CompactTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactTrigger::Auto => write!(f, "auto"),
            CompactTrigger::Manual => write!(f, "manual"),
        }
    }
}

/// Tool use block (matches Anthropic API format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: HashMap<String, serde_json::Value>,
}

/// Content within a tool result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

/// Tool result block (matches Anthropic API format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub content: Vec<ToolResultContent>,
    #[serde(default)]
    pub is_error: bool,
}

/// Content variants for a message
#[derive(Debug, Clone)]
pub enum MessageContent {
    /// Plain text content
    Text(String),
    /// Assistant tool use blocks (role: assistant)
    ToolUseBlocks(Vec<ToolUseBlock>),
    /// Tool result blocks (role: user)
    ToolResultBlocks(Vec<ToolResultBlock>),
    /// Compact boundary marker (role: system) -- signals that messages before this point
    /// have been summarized and should not be sent to the API
    CompactBoundary {
        trigger: CompactTrigger,
        pre_compact_tokens: usize,
        /// UUID uniquely identifies this compact boundary. Used by the transcript,
        /// session storage, and QueryEngine to reference specific compaction events.
        uuid: String,
    },
    /// Summary of compressed conversation history (role: user)
    /// This is injected after compaction to preserve semantic continuity
    Summary(String),
    /// Attachment content for post-compact recovery (role: user)
    /// Re-injects file/skill content after compaction
    Attachment(String),
    /// Post-compaction rules to prevent re-execution of completed tasks.
    /// Separated from the summary so it survives further compaction.
    AntiReplay(String),
    /// Structured goal block (pending/completed tasks, current work).
    /// Separated from the summary so it survives further compaction.
    Goal(String),
    /// Inline compression instruction injected for cache-reusing compaction.
    /// The instruction is appended as a user message so the next API call
    /// reuses the prompt cache prefix.
    CompressionInstruction { level: usize },
    /// Parsed LLM response from inline compression. Wraps summary with
    /// chunk anchors, topics metadata, and previous-chunks index.
    CompressedSummary {
        summary: String,
        topics: String,
        chunk_path: String,
    },
}

/// A single message in the conversation history.
/// Replaces the old ConversationEntry with richer type information.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: MessageRole,
    pub content: MessageContent,
    pub uuid: String,
    pub timestamp: String,
}

impl Message {
    pub fn new(role: MessageRole, content: MessageContent) -> Self {
        Self {
            role,
            content,
            uuid: generate_uuid(),
            timestamp: generate_timestamp(),
        }
    }

    /// Serialize any MessageContent to plain text for safe merging.
    /// Used by fix_role_alternation to avoid silent content loss when
    /// consecutive same-role messages have incompatible content types
    /// (e.g., Summary followed by Text after truncation).
    pub fn content_to_text(&self) -> String {
        match &self.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Summary(s) => s.clone(),
            MessageContent::ToolUseBlocks(blocks) => {
                let parts: Vec<String> = blocks.iter()
                    .map(|b| format!("[tool_use: {}({})]", b.name, b.id))
                    .collect();
                parts.join(" ")
            }
            MessageContent::ToolResultBlocks(blocks) => {
                let parts: Vec<String> = blocks.iter()
                    .map(|b| {
                        let texts: Vec<String> = b.content.iter()
                            .map(|c| match c {
                                ToolResultContent::Text { text } => text.clone(),
                            })
                            .collect();
                        format!("[tool_result: {}] {}", b.tool_use_id, texts.join(" "))
                    })
                    .collect();
                parts.join(" ")
            }
            MessageContent::CompactBoundary { trigger, pre_compact_tokens, .. } => {
                format!("[compact boundary: {}, {} tokens]", trigger, pre_compact_tokens)
            }
            MessageContent::Attachment(a) => a.clone(),
            MessageContent::AntiReplay(a) => a.clone(),
            MessageContent::Goal(g) => g.clone(),
            MessageContent::CompressionInstruction { level } => {
                format!("[compression instruction: level {}]", level)
            }
            MessageContent::CompressedSummary { summary, .. } => {
                format!("[compressed summary: {}]", summary)
            }
        }
    }

    /// Check if this message is a compact boundary
    pub fn is_compact_boundary(&self) -> bool {
        matches!(self.content, MessageContent::CompactBoundary { .. })
    }

    /// Check if this message is a summary
    pub fn is_summary(&self) -> bool {
        matches!(self.content, MessageContent::Summary(_))
    }

    /// Get text content if this is a text or summary message
    pub fn text_content(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Text(t) => Some(t),
            MessageContent::Summary(s) => Some(s),
            _ => None,
        }
    }
}

// Backwards compatibility alias
pub type ConversationEntry = Message;

/// Records what the agent has done across turns.
/// Injected into the system prompt before each API call so the agent
/// Tracks what the agent has done across turns. Injected into the system prompt
/// before each API call so the agent knows what it has already read/searched.
///
/// Compaction is the primary source of "short-term memory loss":
/// when context is compacted, tool results (file content, grep output) are removed
/// from the conversation history. We solve this with an epoch counter: every
/// compaction increments the epoch. Items recorded with epoch == current_epoch
/// are "fresh" (content still in context); items with lower epoch are "stale"
/// (compaction cleared them, re-read is OK).
#[derive(Debug)]
pub struct FileState {
    pub epoch: usize,
    pub mtime_ms: i64, // mtimeMs when the file was read
}

#[derive(Debug)]
pub struct ToolStateTracker {
    compaction_epoch: usize,
    read_files: HashMap<String, FileState>, // path -> (epoch, mtime) when read
    search_queries: HashMap<String, usize>, // pattern -> epoch when searched
    conclusions: Vec<String>,
}

impl ToolStateTracker {
    pub fn new() -> Self {
        Self {
            compaction_epoch: 0,
            read_files: HashMap::new(),
            search_queries: HashMap::new(),
            conclusions: Vec::new(),
        }
    }

    /// Mark a file as read at the current epoch, recording the mtime.
    pub fn record_file_read(&mut self, path: &str) {
        let abs = std::fs::canonicalize(path)
            .unwrap_or_else(|_| std::path::PathBuf::from(path));
        let mtime_ms = std::fs::metadata(abs.as_path())
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.read_files.insert(abs.display().to_string(), FileState { epoch: self.compaction_epoch, mtime_ms });
    }

    /// Record a successful grep/glob search pattern at the current epoch.
    pub fn record_search(&mut self, pattern: &str, had_results: bool) {
        if had_results {
            self.search_queries.insert(pattern.to_string(), self.compaction_epoch);
        }
    }

    /// Append a key finding claimed by the agent.
    pub fn record_conclusion(&mut self, conclusion: &str) {
        if conclusion.is_empty() {
            return;
        }
        self.conclusions.push(conclusion.to_string());
    }

    /// Called after context compaction runs. Advances the epoch, marking all
    /// previously tracked items as stale (their tool results are gone from context).
    pub fn on_compaction(&mut self) {
        self.compaction_epoch += 1;
    }

    /// Mark a file as fresh — its content is back in context (e.g., after
    /// post-compact recovery re-injects it). Updates the file's epoch to current.
    pub fn mark_file_fresh(&mut self, path: &str) {
        let abs = std::fs::canonicalize(path)
            .unwrap_or_else(|_| std::path::PathBuf::from(path));
        let mtime_ms = std::fs::metadata(abs.as_path())
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.read_files.insert(abs.display().to_string(), FileState { epoch: self.compaction_epoch, mtime_ms });
    }

    /// Clear all recorded conclusions. Called after compaction when no files were
    /// recovered — the summary captures all pre-compact knowledge, so stale
    /// conclusions should not be re-stated.
    pub fn clear_conclusions(&mut self) {
        self.conclusions.clear();
    }

    /// Return a copy of the recorded conclusions.
    pub fn get_conclusions(&self) -> Vec<String> {
        self.conclusions.clone()
    }

    /// Return the text to inject into the system prompt.
    pub fn build_session_state_note(&self) -> String {
        let mut sb = String::new();
        sb.push_str("## Session State\n");

        let mut fresh_files: Vec<&String> = Vec::new();
        let mut stale_files: Vec<&String> = Vec::new();
        let mut modified_files: Vec<String> = Vec::new();
        for (f, state) in &self.read_files {
            if state.epoch == self.compaction_epoch {
                fresh_files.push(f);
                // Check if file was modified externally since we read it
                if let Ok(meta) = std::fs::metadata(f) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                            let current_mtime_ms = duration.as_millis() as i64;
                            if state.mtime_ms != 0 && current_mtime_ms != state.mtime_ms {
                                modified_files.push(f.clone());
                            }
                        }
                    }
                }
            } else {
                stale_files.push(f);
            }
        }
        fresh_files.sort();
        stale_files.sort();

        if !fresh_files.is_empty() {
            sb.push_str("Files already read — content is in context (do NOT re-read):\n");
            for f in &fresh_files {
                sb.push_str("  - ");
                sb.push_str(f);
                if modified_files.contains(&(**f).clone()) {
                    sb.push_str(" (MODIFIED since last read — re-read if needed)");
                }
                sb.push('\n');
            }
        }
        if !stale_files.is_empty() {
            sb.push_str("Files read before compaction — content was cleared from context:\n");
            for f in &stale_files {
                sb.push_str("  - ");
                sb.push_str(f);
                sb.push_str(" (RE-READ if needed)\n");
            }
        }

        let mut fresh_queries: Vec<&String> = Vec::new();
        let mut stale_queries: Vec<&String> = Vec::new();
        for (q, e) in &self.search_queries {
            if *e == self.compaction_epoch {
                fresh_queries.push(q);
            } else {
                stale_queries.push(q);
            }
        }
        fresh_queries.sort();
        stale_queries.sort();

        if !fresh_queries.is_empty() {
            sb.push_str("Search patterns already run — results in context (do NOT repeat):\n");
            for q in &fresh_queries {
                sb.push_str("  - ");
                sb.push_str(q);
                sb.push('\n');
            }
        }
        if !stale_queries.is_empty() {
            sb.push_str("Search patterns from before compaction — results were cleared:\n");
            for q in &stale_queries {
                sb.push_str("  - ");
                sb.push_str(q);
                sb.push_str(" (RE-RUN if needed)\n");
            }
        }

        if !self.conclusions.is_empty() {
            sb.push_str("Key findings from this session:\n");
            for c in &self.conclusions {
                sb.push_str("  - ");
                sb.push_str(c);
                sb.push('\n');
            }
        }

        if fresh_files.is_empty() && stale_files.is_empty()
            && fresh_queries.is_empty() && stale_queries.is_empty()
            && self.conclusions.is_empty()
        {
            sb.push_str("(no prior state)\n");
        }

        sb
    }
}

impl Default for ToolStateTracker {
    fn default() -> Self { Self::new() }
}

/// Manages conversation message history and system prompt
#[derive(Debug)]
pub struct ConversationContext {
    config: Config,
    messages: Vec<Message>,
    #[allow(dead_code)]
    system_prompt: String,
    last_assistant_time: Option<std::time::Instant>, // used for time-based microcompact
    // Hybrid token estimation: use API anchor as precise baseline
    api_token_anchor: i64,   // exact input_tokens from last API response
    api_anchor_entries: usize, // entry count when anchor was recorded
    // Tool result replacement map for cache-stable serialization
    tool_result_replacements: HashMap<String, String>,
    // Tracks tool_use_ids whose content was cleared by micro-compact
    cleared_tool_results: std::collections::HashSet<String>,
    // Tracks compression level for progressive summarization
    compression_level: usize,
    // Whether context has been compacted at least once
    is_compacted: bool,
    // Disk persistence for oversized tool results (None = feature disabled)
    tool_result_store: Option<ToolResultStore>,
    // Replacement state tracker for prompt cache stability
    content_replacement_state: Option<Arc<ContentReplacementState>>,
    /// Redacted thinking blocks from assistant responses that arrived before
    /// tool_use blocks. Must be re-submitted for context continuity.
    pending_redacted_thinking: Vec<serde_json::Value>,
}

/// Represents a detected turn interruption in the conversation.
#[derive(Debug, Clone)]
pub struct TurnInterruption {
    /// Index of the interrupted assistant message
    pub interrupted_at: usize,
    /// Index of the new user message that caused the interruption
    pub user_message_idx: usize,
}

/// Set of tool names whose results should be cleared during micro-compaction.
/// These are read/search/web/write tools where the raw output is large and not
/// needed for context after the turn passes.
/// Tools like git, memory, skill, list_dir, exec, etc. are NOT compacted because
/// their results contain structural information the model may need later.
const COMPACTABLE_TOOLS: &[&str] = &[
    "read_file",
    "exec",
    "edit_file",
    "write_file",
    "multi_edit",
    "grep",
    "glob",
    "list_dir",     // matching Go compactableToolNames
    "web_fetch",
    "web_search",
    "exa_search",   // web search variant
];

fn is_compactable_tool(name: &str) -> bool {
    COMPACTABLE_TOOLS.contains(&name)
}

impl ConversationContext {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            messages: Vec::new(),
            system_prompt: String::new(),
            last_assistant_time: None,
            api_token_anchor: 0,
            api_anchor_entries: 0,
            tool_result_replacements: HashMap::new(),
            cleared_tool_results: std::collections::HashSet::new(),
            compression_level: 0,
            is_compacted: false,
            tool_result_store: None,
            content_replacement_state: None,
            pending_redacted_thinking: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn set_system_prompt(&mut self, prompt: String) {
        self.system_prompt = prompt;
    }

    #[allow(dead_code)]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Returns true when the time gap since the last assistant message exceeds
    /// gap_minutes. A gap_minutes of 0 means always fire (legacy count-based
    /// behavior for backward compatibility).
    pub fn should_time_based_micro_compact(&self, gap_minutes: u64) -> bool {
        if gap_minutes == 0 {
            return true; // disabled — fire every turn
        }
        match self.last_assistant_time {
            None => true, // no assistant yet — fire
            Some(t) => t.elapsed() >= std::time::Duration::from_secs(gap_minutes * 60),
        }
    }

    /// Add a user text message
    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message::new(
            MessageRole::User,
            MessageContent::Text(content),
        ));
        self.truncate_if_needed();
    }

    /// Add an assistant text message
    pub fn add_assistant_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.messages.push(Message::new(
            MessageRole::Assistant,
            MessageContent::Text(text),
        ));
        self.last_assistant_time = Some(std::time::Instant::now());
        self.truncate_if_needed();
    }

    /// Add assistant tool use blocks
    pub fn add_assistant_tool_calls(&mut self, tool_calls: Vec<ToolUseBlock>) {
        self.messages.push(Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(tool_calls),
        ));
        self.last_assistant_time = Some(std::time::Instant::now());
        self.truncate_if_needed();
    }

    /// Add tool result blocks
    pub fn add_tool_results(&mut self, results: Vec<ToolResultBlock>) {
        self.messages.push(Message::new(
            MessageRole::User,
            MessageContent::ToolResultBlocks(results),
        ));
        self.truncate_if_needed();
    }

    /// Add a compact boundary marker
    pub fn add_compact_boundary(&mut self, trigger: CompactTrigger, pre_compact_tokens: usize) {
        let uuid = uuid::Uuid::new_v4().to_string();
        self.messages.push(Message::new(
            MessageRole::System,
            MessageContent::CompactBoundary {
                trigger,
                pre_compact_tokens,
                uuid,
            },
        ));
    }

    /// Add a summary message (from compaction)
    pub fn add_summary(&mut self, content: String) {
        self.messages.push(Message::new(
            MessageRole::User,
            MessageContent::Summary(content),
        ));
    }

    /// Add an attachment message (post-compact recovery of file/skill content)
    pub fn add_attachment(&mut self, content: String) {
        self.messages.push(Message::new(
            MessageRole::User,
            MessageContent::Attachment(content),
        ));
    }

    /// Add a generic system message
    #[allow(dead_code)]
    pub fn add_system_message(&mut self, content: String) {
        self.messages.push(Message::new(
            MessageRole::System,
            MessageContent::Text(content),
        ));
    }

    /// Add a raw message (for transcript replay)
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
        self.truncate_if_needed();
    }

    /// Set pending redacted thinking data from a previous API response.
    /// These opaque blobs must be re-submitted for context continuity.
    pub fn set_pending_redacted_thinking(&mut self, data: Vec<serde_json::Value>) {
        self.pending_redacted_thinking = data;
    }

    /// Build API-compatible message array from conversation history.
    /// This is the Rust equivalent of Go's BuildMessages().
    /// Finds the last compact boundary, converts all content types,
    /// applies tool result replacements, merges consecutive same-role messages,
    /// and fixes orphaned tool results.
    pub fn build_messages(&mut self) -> Vec<serde_json::Value> {
        // Consume pending redacted thinking (only used once)
        let mut redacted_data: Vec<serde_json::Value> = std::mem::take(&mut self.pending_redacted_thinking);

        // Copy replacement map for cache-stable serialization
        let replacements: HashMap<String, String> = self.tool_result_replacements.clone();

        // Find the last compact boundary. Entries at or after this point are preserved;
        // everything before is dropped. This is the key mechanism that makes compaction
        // actually reduce token usage — without this reset, old messages would still be
        // included and compaction would be a no-op.
        let boundary_idx = self.messages.iter().rposition(|m| m.is_compact_boundary());
        let start_idx = boundary_idx.unwrap_or(0);

        let mut messages: Vec<serde_json::Value> = Vec::with_capacity(self.messages.len() - start_idx);

        for msg in &self.messages[start_idx..] {
            let mut content_blocks: Vec<serde_json::Value> = Vec::new();
            let role = msg.role.as_str();

            match &msg.content {
                MessageContent::Text(text) => {
                    content_blocks.push(serde_json::json!({
                        "type": "text", "text": text
                    }));
                }
                MessageContent::ToolUseBlocks(blocks) => {
                    // Prepend redacted_thinking blocks to the first assistant tool_use message.
                    // The API requires these opaque data blobs to be re-submitted for context
                    // continuity when interleaved thinking is enabled.
                    let consumed = !redacted_data.is_empty();
                    if consumed {
                        for block in &redacted_data {
                            content_blocks.push(block.clone());
                        }
                    }
                    for b in blocks {
                        content_blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": b.id,
                            "name": b.name,
                            "input": b.input
                        }));
                    }
                    // Consume after first use — only prepend to the first tool_use message
                    if consumed {
                        redacted_data.clear();
                    }
                }
                MessageContent::ToolResultBlocks(blocks) => {
                    for r in blocks {
                        if let Some(repl) = replacements.get(&r.tool_use_id) {
                            // Apply replacement for cache-stable serialization
                            content_blocks.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_use_id,
                                "content": [{"type": "text", "text": repl}],
                                "is_error": r.is_error
                            }));
                        } else {
                            let content_values: Vec<serde_json::Value> = r.content.iter()
                                .filter_map(|c| serde_json::to_value(c).ok())
                                .collect();
                            content_blocks.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_use_id,
                                "content": content_values,
                                "is_error": r.is_error
                            }));
                        }
                    }
                }
                MessageContent::CompactBoundary { .. } => {
                    // The boundary itself is not sent to the API — it serves as the
                    // cutoff marker. The API doesn't understand compact boundaries.
                    continue;
                }
                MessageContent::Summary(text)
                | MessageContent::Attachment(text)
                | MessageContent::AntiReplay(text)
                | MessageContent::Goal(text) => {
                    content_blocks.push(serde_json::json!({
                        "type": "text", "text": format!("{}{}", SYSTEM_INJECTED_PREFIX, text)
                    }));
                }
                MessageContent::CompressionInstruction { level } => {
                    let prompt = build_compression_prompt(*level);
                    content_blocks.push(serde_json::json!({
                        "type": "text", "text": format!("{}{}", SYSTEM_INJECTED_PREFIX, prompt)
                    }));
                }
                MessageContent::CompressedSummary { summary, chunk_path, .. } => {
                    let mut text = format!("{}{}", SYSTEM_INJECTED_PREFIX, summary);
                    if !chunk_path.is_empty() {
                        text.push_str(&format!(
                            "\n\n📁 **Current chunk archived at:** `{}`\n_Use `file_reader` tool to recall details from this chunk._",
                            chunk_path
                        ));
                    }
                    content_blocks.push(serde_json::json!({
                        "type": "text", "text": text
                    }));
                }
            }

            if !content_blocks.is_empty() {
                messages.push(serde_json::json!({
                    "role": role,
                    "content": content_blocks
                }));
            }
        }

        // Merge consecutive same-role messages (API requires strict alternation).
        // This handles cases where fix_role_alternation couldn't merge due to
        // type mismatches (e.g., ToolResultContent + TextContent both user role).
        // The API allows a single user message to contain mixed text and tool_result blocks.
        let mut merged: Vec<serde_json::Value> = Vec::with_capacity(messages.len());
        for msg in messages {
            if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                if let Some(last) = merged.last_mut() {
                    if let Some(last_role) = last.get("role").and_then(|r| r.as_str()) {
                        if last_role == role {
                            // Merge content blocks
                            if let Some(arr) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                                if let Some(new_blocks) = msg.get("content").and_then(|c| c.as_array()) {
                                    arr.extend(new_blocks.clone());
                                }
                            }
                            continue;
                        }
                    }
                }
            }
            merged.push(msg);
        }

        // Fix orphaned tool_results: when the compact boundary drops tool_use
        // entries that precede it, their matching tool_results in the kept tail
        // become orphaned. Instead of silently stripping them and inserting
        // "Tool execution was interrupted", inject synthetic tool_use blocks
        // for any tool_result whose tool_use_id is not present in any assistant
        // message's tool_use blocks.
        merged = fix_orphaned_tool_results(merged);

        merged
    }

    /// Get all messages
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Get all messages (alias for backwards compat)
    pub fn entries(&self) -> &[Message] {
        &self.messages
    }

    /// Get message count
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Check if empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Clear all messages
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Replace all messages (used by compactor)
    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    /// Replace all messages (alias for backwards compat)
    pub fn replace_entries(&mut self, messages: Vec<Message>) {
        self.replace_messages(messages);
    }

    /// Get messages after the last compact boundary.
    /// Similar to Claude Code's getMessagesAfterCompactBoundary().
    /// If no compact boundary exists, returns all messages.
    pub fn messages_after_compact_boundary(&self) -> &[Message] {
        let last_boundary_idx = self.messages.iter().rposition(|m| m.is_compact_boundary());
        match last_boundary_idx {
            Some(idx) => {
                // Return from the boundary onwards (boundary + summary + subsequent messages)
                &self.messages[idx..]
            }
            None => &self.messages,
        }
    }

    /// Find the index of the last compact boundary message
    pub fn last_compact_boundary_index(&self) -> Option<usize> {
        self.messages.iter().rposition(|m| m.is_compact_boundary())
    }

    fn truncate_if_needed(&mut self) {
        let max_msgs = self.config.max_context_msgs;
        if self.messages.len() > max_msgs {
            let keep = max_msgs.saturating_sub(1);
            if keep > 0 {
                let first = self.messages[..1].to_vec();
                let recent = self.messages[self.messages.len() - keep..].to_vec();

                // Keep ALL entries and let fix_role_alternation handle same-role merging.
                // Previously, same-role entries were silently skipped here, causing content loss
                // and the "re-executes historical instructions" bug (important user instructions
                // in Summary/Attachment/Text entries were permanently dropped).
                self.messages = first.into_iter().chain(recent).collect();

                // After truncation, validate tool pairing and fix alternation
                self.validate_tool_pairing();
                self.fix_role_alternation();
            } else if !self.messages.is_empty() {
                self.messages = vec![self.messages[0].clone()];
            }
            // else: messages already empty, nothing to truncate
        }
    }

    /// TruncateHistory drops older messages to recover from context overflow.
    /// Keeps the first entry (initial user message) and the last 10 entries.
    /// Compact-boundary-aware: if compaction has occurred, preserves from the
    /// boundary through recent entries instead of discarding the summary.
    pub fn truncate_history(&mut self) {
        if self.messages.len() <= 12 {
            return;
        }
        let keep = 10;
        self.messages = self.truncate_with_boundary(1, keep);
        self.validate_tool_pairing();
        self.fix_role_alternation();
    }

    /// AggressiveTruncateHistory drops more aggressively - keeps only first and last 5.
    /// Compact-boundary-aware.
    pub fn aggressive_truncate_history(&mut self) {
        if self.messages.len() <= 6 {
            return;
        }
        let keep = 5;
        self.messages = self.truncate_with_boundary(1, keep);
        self.validate_tool_pairing();
        self.fix_role_alternation();
    }

    /// MinimumHistory drops to bare minimum - only first user message and last 2 entries.
    /// Compact-boundary-aware.
    pub fn minimum_history(&mut self) {
        if self.messages.len() <= 3 {
            return;
        }
        self.messages = self.truncate_with_boundary(1, 2);
        self.validate_tool_pairing();
        self.fix_role_alternation();
    }

    /// Performs a naive truncation but preserves the compaction boundary marker
    /// and summary if one exists. After compaction, entries look like:
    ///   [0] initial-user, [1] CompactBoundary, [2] Summary, [3..n] attachments+recent
    /// Naive truncation (entries[:1] + recent) would discard entries[1] and [2],
    /// causing the agent to lose all compressed memory. This function finds the
    /// boundary and preserves everything from the boundary onwards.
    fn truncate_with_boundary(&self, head_keep: usize, tail_keep: usize) -> Vec<Message> {
        // Find the most recent CompactBoundary
        let boundary_idx = self.messages.iter().rposition(|msg| {
            matches!(msg.content, MessageContent::CompactBoundary { .. })
        });

        if let Some(idx) = boundary_idx {
            // After compaction: keep from the boundary through recent entries.
            // This preserves the boundary marker, summary, attachments, and
            // recent messages. Don't discard the summary — it's the only memory
            // of what happened before.
            self.messages[idx..].to_vec()
        } else {
            // No boundary — use naive truncation
            let first = self.messages[0..head_keep].to_vec();
            let recent = self.messages[self.messages.len() - tail_keep..].to_vec();
            [first, recent].concat()
        }
    }

    /// Validates bidirectional tool_use/tool_result pairing.
    /// Handles two failure modes after truncation:
    /// 1. Orphaned tool_results: result references a tool_use that was removed → delete result
    /// 2. Orphaned tool_uses: tool_use has no matching result (result was truncated) →
    ///    delete the tool_use block; if message becomes empty, replace with placeholder text
    pub fn validate_tool_pairing(&mut self) {
        // Pass 1: Collect all tool_use IDs from assistant messages
        let mut call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for msg in &self.messages {
            if msg.role == MessageRole::Assistant {
                if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
                    for block in blocks {
                        call_ids.insert(block.id.clone());
                    }
                }
            }
        }

        // Pass 2: Remove orphaned tool_results (those without matching tool_use).
        // This is the critical fix for 2013 error: "tool call result does not follow tool call".
        // We ONLY remove orphaned tool_results. We do NOT remove tool_use blocks without
        // results (removed per Go's agent_loop.go: "Pass 3 REMOVED") — removing tool_use
        // blocks while leaving tool_results in the API's view causes a worse structural mismatch.
        let mut result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for msg in &mut self.messages {
            if let MessageContent::ToolResultBlocks(blocks) = &mut msg.content {
                blocks.retain(|b| {
                    if call_ids.contains(&b.tool_use_id) {
                        result_ids.insert(b.tool_use_id.clone());
                        true
                    } else {
                        false
                    }
                });
            }
        }

        // Pass 3: Remove orphaned tool_result messages that are now empty
        self.messages.retain(|msg| {
            if let MessageContent::ToolResultBlocks(blocks) = &msg.content {
                !blocks.is_empty()
            } else {
                true
            }
        });

        // Pass 4: Insert synthetic tool_results for tool_use blocks without matching results.
        // After compaction, a tool_use block may survive in the kept tail while its
        // tool_result was in the summarized portion. The API requires every tool_use
        // to have a corresponding tool_result — without one, it returns error 2013.
        // Insert a synthetic error result right after the assistant message containing
        // the unpaired tool_use. This matches upstream's ensureToolResultPairing.
        let missing_ids: Vec<String> = call_ids.iter()
            .filter(|id| !result_ids.contains(*id))
            .cloned()
            .collect();
        if !missing_ids.is_empty() {
            let missing_set: std::collections::HashSet<String> = missing_ids.into_iter().collect();
            let placeholder = "[Tool result missing due to internal error]";
            let mut new_messages: Vec<Message> = Vec::with_capacity(self.messages.len() + missing_set.len());
            let mut remaining_missing = missing_set;
            for msg in self.messages.drain(..) {
                let is_assistant = msg.role == MessageRole::Assistant;
                new_messages.push(msg);
                if is_assistant {
                    if let Some(last) = new_messages.last() {
                        if let MessageContent::ToolUseBlocks(blocks) = &last.content {
                            let mut synth_results: Vec<ToolResultBlock> = Vec::new();
                            for block in blocks {
                                if remaining_missing.contains(&block.id) {
                                    synth_results.push(ToolResultBlock {
                                        tool_use_id: block.id.clone(),
                                        content: vec![ToolResultContent::Text { text: placeholder.to_string() }],
                                        is_error: true,
                                    });
                                    remaining_missing.remove(&block.id);
                                }
                            }
                            if !synth_results.is_empty() {
                                new_messages.push(Message::new(
                                    MessageRole::User,
                                    MessageContent::ToolResultBlocks(synth_results),
                                ));
                            }
                        }
                    }
                }
            }
            self.messages = new_messages;
        }
    }

    /// Ensures strict user/assistant alternation by merging consecutive
    /// messages with the same role. Critical for Anthropic API compliance.
    ///
    /// CRITICAL: Never convert ToolResultBlocks to TextContent. Doing so
    /// destroys the tool_use/tool_result pairing, causing API error 2013.
    /// When same-role messages have incompatible content types (e.g. Text
    /// + ToolResultBlocks), keep them as SEPARATE entries instead of merging.
    pub fn fix_role_alternation(&mut self) {
        if self.messages.is_empty() {
            return;
        }

        let mut merged: Vec<Message> = Vec::with_capacity(self.messages.len());
        for msg in self.messages.drain(..) {
            // Skip system messages (compact boundaries, etc.) -- they are
            // filtered out by entries_to_messages_from_ctx anyway.
            if msg.role == MessageRole::System {
                merged.push(msg);
                continue;
            }

            if let Some(last) = merged.last_mut() {
                if last.role == msg.role {
                    // Merge same-role consecutive messages ONLY when content
                    // types are compatible. When types mismatch, keep separate
                    // to preserve ToolResultBlocks tool pairing.
                    let should_merge = match (&last.content, &msg.content) {
                        // Same types: always merge
                        (MessageContent::Text(_), MessageContent::Text(_))
                        | (MessageContent::ToolUseBlocks(_), MessageContent::ToolUseBlocks(_))
                        | (MessageContent::ToolResultBlocks(_), MessageContent::ToolResultBlocks(_))
                        | (MessageContent::Summary(_), MessageContent::Summary(_))
                        | (MessageContent::Attachment(_), MessageContent::Attachment(_)) => true,
                        // Text-compatible types (all user-role non-tool types): safe to merge.
                        // This prevents multiple consecutive user-role messages after compaction,
                        // which the Anthropic API rejects as error 2013.
                        (MessageContent::Text(_), MessageContent::Summary(_))
                        | (MessageContent::Summary(_), MessageContent::Text(_))
                        | (MessageContent::Text(_), MessageContent::Attachment(_))
                        | (MessageContent::Attachment(_), MessageContent::Text(_))
                        | (MessageContent::Summary(_), MessageContent::Attachment(_))
                        | (MessageContent::Attachment(_), MessageContent::Summary(_)) => true,
                        // ToolResultBlocks: never merge with different types -- doing so
                        // destroys the tool_use/tool_result pairing (API 2013 error)
                        // ToolUseBlocks: keep separate when mixed with non-tool types
                        _ => false,
                    };

                    if should_merge {
                        match &msg.content {
                            MessageContent::Text(b) => {
                                if let MessageContent::Text(a) = &mut last.content {
                                    a.push_str("\n\n");
                                    a.push_str(b);
                                } else if let MessageContent::Summary(a) = &mut last.content {
                                    // Summary + Text -> merge into Text
                                    let prev = a.clone();
                                    let mut new_text = prev;
                                    new_text.push_str("\n\n");
                                    new_text.push_str(b);
                                    last.content = MessageContent::Text(new_text);
                                } else {
                                    let prev = last.content_to_text();
                                    last.content = MessageContent::Text(
                                        format!("{}\n\n{}", prev, b),
                                    );
                                }
                            }
                            MessageContent::ToolUseBlocks(b) => {
                                if let MessageContent::ToolUseBlocks(a) = &mut last.content {
                                    a.extend(b.clone());
                                } else {
                                    let prev = last.content_to_text();
                                    let tools: Vec<String> = b.iter()
                                        .map(|t| format!("[tool_use: {}({})]", t.name, t.id))
                                        .collect();
                                    last.content = MessageContent::Text(
                                        format!("{}\n\n{}", prev, tools.join(" ")),
                                    );
                                }
                            }
                            MessageContent::ToolResultBlocks(b) => {
                                if let MessageContent::ToolResultBlocks(a) = &mut last.content {
                                    a.extend(b.clone());
                                }
                                // else: shouldn't reach here due to should_merge guard
                            }
                            MessageContent::Summary(b) => {
                                if let MessageContent::Summary(a) = &mut last.content {
                                    a.push_str("\n\n");
                                    a.push_str(b);
                                } else if let MessageContent::Text(a) = &mut last.content {
                                    a.push_str("\n\n");
                                    a.push_str(b);
                                } else {
                                    let prev = last.content_to_text();
                                    last.content = MessageContent::Text(
                                        format!("{}\n\n{}", prev, b),
                                    );
                                }
                            }
                            _ => {
                                // Fallback: serialize both to text and concatenate
                                let prev = last.content_to_text();
                                let curr = msg.content_to_text();
                                last.content = MessageContent::Text(
                                    format!("{}\n\n{}", prev, curr),
                                );
                            }
                        }
                        continue;
                    }
                    // should_merge == false: types are incompatible.
                    // Keep as separate entries (no merging, no conversion).
                }
            }
            merged.push(msg);
        }
        self.messages = merged;
    }

    /// Micro-compact: clears content of old tool results beyond the keep_recent window.
    /// Returns the number of tool result entries that were cleared.
    /// Tool use IDs are preserved to maintain pairing validity.
    ///
    /// Two improvements over the original:
    ///  1. Dedup: skips tool results already cleared to the placeholder string.
    ///  2. Whitelist: only clears results from compactable tools (read/exec/edit/grep/glob/web/write).
    pub fn micro_compact_entries(&mut self, keep_recent: usize, placeholder: &str, min_char_count: usize) -> usize {
        let keep_recent = if keep_recent == 0 { 5 } else { keep_recent };
        let placeholder = if placeholder.is_empty() {
            "[Old tool result content cleared]"
        } else {
            placeholder
        };
        let min_char_count = if min_char_count == 0 { 2000 } else { min_char_count };

        // Pass 1: Build tool_use_id -> tool_name mapping from ToolUseBlocks messages.
        let mut tool_name_map: HashMap<String, String> = HashMap::new();
        for msg in &self.messages {
            if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
                for block in blocks {
                    if !block.id.is_empty() {
                        tool_name_map.insert(block.id.clone(), block.name.clone());
                    }
                }
            }
        }

        // Pass 2: Iterate backwards, clearing eligible tool results.
        let mut recent_count = 0;
        let mut cleared = 0;

        for i in (0..self.messages.len()).rev() {
            if let MessageContent::ToolResultBlocks(blocks) = &mut self.messages[i].content {
                if recent_count < keep_recent {
                    recent_count += 1;
                    continue;
                }

                // Check each block: is it already cleared? is it a compactable tool AND large enough?
                let all_cleared = blocks.iter().all(|b| {
                    b.content.iter().any(|c| {
                        matches!(c, ToolResultContent::Text { text } if text == placeholder)
                    })
                });
                let has_compactable = blocks.iter().any(|b| {
                    // Preserve error results — they contain important debugging info
                    if b.is_error {
                        return false;
                    }
                    if let Some(name) = tool_name_map.get(&b.tool_use_id) {
                        if is_compactable_tool(name) {
                            // Only consider compactable if the result is large enough to justify clearing.
                            // Small results (< min_char_count) are preserved to prevent amnesia.
                            let total_chars: usize = b.content.iter().map(|c| {
                                let ToolResultContent::Text { text } = c;
                                text.len()
                            }).sum();
                            return total_chars >= min_char_count;
                        }
                    }
                    false
                });

                // Skip if all blocks are already cleared, or none are compactable/large enough
                if all_cleared || !has_compactable {
                    continue;
                }

                // Clear only compactable tool results that are large enough; leave others untouched
                for block in blocks.iter_mut() {
                    // Preserve error results — they contain important debugging info
                    if block.is_error {
                        continue;
                    }
                    if let Some(name) = tool_name_map.get(&block.tool_use_id) {
                        if is_compactable_tool(name) {
                            // Check size threshold: preserve small results to prevent amnesia
                            let total_chars: usize = block.content.iter().map(|c| {
                                let ToolResultContent::Text { text } = c;
                                text.len()
                            }).sum();
                            if total_chars >= min_char_count {
                                block.content = vec![ToolResultContent::Text {
                                    text: placeholder.to_string(),
                                }];
                            }
                        }
                    }
                }
                cleared += 1;
            }
        }
        cleared
    }

    /// KeepRecentMessages preserves the most recent conversation entries verbatim
    /// after compaction, keeping their original structure (including ToolUseBlocks
    /// and ToolResultBlocks). This matches upstream's messagesToKeep mechanism
    /// (sessionMemoryCompact.ts calculateMessagesToKeepIndex + adjustIndexToPreserveAPIInvariants).
    ///
    /// Unlike add_history_snip which converts entries to plain text (losing tool structure),
    /// this method keeps entries as-is so the model can see actual tool_use/tool_result pairs,
    /// preventing re-execution of commands it already ran.
    ///
    /// The method also adjusts the kept range backwards to include any assistant messages
    /// whose tool_use blocks are referenced by tool_results in the kept range, ensuring
    /// tool_use/tool_result pairing is never broken (matching upstream's
    /// adjustIndexToPreserveAPIInvariants).
    pub fn keep_recent_messages(&mut self, count: usize) {
        let count = if count == 0 { 8 } else { count };

        // Find the most recent CompactBoundary
        let boundary_idx = match self.messages.iter().rposition(|m| m.is_compact_boundary()) {
            Some(idx) => idx,
            None => return,
        };

        // Collect up to 'count' entries before the boundary (pre-compact messages)
        let mut kept_indices: Vec<usize> = Vec::new();
        for i in (0..boundary_idx).rev() {
            if kept_indices.len() >= count {
                break;
            }
            match &self.messages[i].content {
                MessageContent::CompactBoundary { .. }
                | MessageContent::Summary(_)
                | MessageContent::Attachment(_) => continue,
                _ => kept_indices.push(i),
            }
        }
        if kept_indices.is_empty() {
            return;
        }
        // Reverse so they're in chronological order
        kept_indices.reverse();

        // Adjust backwards to preserve tool_use/tool_result pairing.
        // If any kept entry contains ToolResultBlocks, collect its tool_use_ids,
        // then walk further backwards to find the assistant messages with matching
        // ToolUseBlocks. This prevents orphaned tool_results that would cause API error 2013.
        let needed_ids: Vec<String> = kept_indices.iter()
            .filter_map(|&i| {
                if let MessageContent::ToolResultBlocks(blocks) = &self.messages[i].content {
                    Some(blocks.iter().map(|b| b.tool_use_id.clone()))
                } else {
                    None
                }
            })
            .flatten()
            .collect();

        if !needed_ids.is_empty() {
            // Check which tool_use_ids are already present in the kept range
            let already_present: std::collections::HashSet<String> = kept_indices.iter()
                .filter_map(|&i| {
                    if let MessageContent::ToolUseBlocks(blocks) = &self.messages[i].content {
                        Some(blocks.iter().map(|b| b.id.clone()))
                    } else {
                        None
                    }
                })
                .flatten()
                .collect();

            let missing_ids: std::collections::HashSet<String> = needed_ids.into_iter()
                .filter(|id| !already_present.contains(id))
                .collect();

            if !missing_ids.is_empty() {
                // Walk backwards through pre-boundary entries to find assistant messages
                // containing the missing tool_use blocks
                let mut additional_indices: Vec<usize> = Vec::new();
                let min_idx = kept_indices.first().copied().unwrap_or(0);
                for i in (0..min_idx).rev() {
                    if self.messages[i].role != MessageRole::Assistant {
                        continue;
                    }
                    if let MessageContent::ToolUseBlocks(blocks) = &self.messages[i].content {
                        let has_match = blocks.iter().any(|b| missing_ids.contains(&b.id));
                        if has_match {
                            additional_indices.push(i);
                        }
                    }
                }
                // Insert additional indices in chronological order before the kept range
                additional_indices.reverse();
                let mut combined = additional_indices;
                combined.extend(kept_indices);
                kept_indices = combined;
            }
        }

        // Clone the kept messages and append them after the boundary+summary
        let kept_messages: Vec<Message> = kept_indices.iter()
            .map(|&i| self.messages[i].clone())
            .collect();
        self.messages.extend(kept_messages);
    }

    /// AddHistorySnip preserves the most recent conversation entries verbatim
    /// after compaction. Entries are added as user-role text messages with a
    /// [history-snip] prefix. skip_paths contains file paths recovered by
    /// PostCompactRecovery; ToolResultBlocks entries referencing those paths
    /// are skipped to avoid duplication.
    pub fn add_history_snip(&mut self, count: usize, skip_paths: &[String]) {
        let count = if count == 0 { 3 } else { count };

        // Find the most recent CompactBoundary
        let boundary_idx = self.messages.iter().rposition(|m| m.is_compact_boundary());
        let Some(boundary_idx) = boundary_idx else { return };

        // Collect up to 'count' entries before the boundary as owned (role_str, text) tuples
        // so we can release the immutable borrow before pushing to self.messages
        let mut snip_entries: Vec<(String, String)> = Vec::new();
        for i in (0..boundary_idx).rev() {
            if snip_entries.len() >= count {
                break;
            }
            let msg = &self.messages[i];
            match &msg.content {
                MessageContent::CompactBoundary { .. }
                | MessageContent::Summary(_)
                | MessageContent::Attachment(_) => {
                    continue;
                }
                MessageContent::ToolResultBlocks(blocks) => {
                    // Skip entries that reference recovered file paths
                    if !skip_paths.is_empty() {
                        let skip = blocks.iter().any(|b| {
                            b.content.iter().any(|c| {
                                let ToolResultContent::Text { text } = c;
                                skip_paths.iter().any(|p| text.contains(p))
                            })
                        });
                        if skip {
                            continue;
                        }
                    }
                    let role_str = msg.role.as_str().to_string();
                    let text = msg.content_to_text();
                    snip_entries.insert(0, (role_str, text));
                }
                _ => {
                    let role_str = msg.role.as_str().to_string();
                    let text = msg.content_to_text();
                    snip_entries.insert(0, (role_str, text));
                }
            }
        }

        // Append snip entries after the boundary as preserved text messages
        for (role_str, text) in snip_entries {
            if text.is_empty() {
                continue;
            }
            self.messages.push(Message::new(
                MessageRole::User,
                MessageContent::Text(format!("[history-snip {}] {}", role_str, text)),
            ));
        }
    }

    /// Records the exact input_tokens from an API response along with the
    /// current entry count. This enables hybrid token estimation: use the
    /// exact API count as anchor, then only estimate the delta for entries
    /// added since.
    pub fn set_api_token_anchor(&mut self, input_tokens: i64) {
        self.api_token_anchor = input_tokens;
        self.api_anchor_entries = self.messages.len();
    }

    /// Returns a hybrid token estimate using API anchor + incremental estimation.
    ///
    /// 1. If we have an API anchor, use it as baseline and estimate the delta.
    /// 2. If no anchor or stale anchor, fall back to full heuristic estimation.
    ///
    /// Only counts entries after the most recent compact boundary.
    pub fn estimated_tokens(&self) -> usize {
        // Find the most recent compact boundary
        let boundary_idx = self.messages.iter().rposition(|m| m.is_compact_boundary());
        let start_idx = boundary_idx.unwrap_or(0);

        // Hybrid estimation: use API anchor if available and valid
        if self.api_token_anchor > 0 && self.api_anchor_entries > 0 {
            // Anchor is valid only if it was recorded at or after the current boundary
            // and the entry count hasn't been invalidated by compaction
            if self.api_anchor_entries >= start_idx && self.api_anchor_entries <= self.messages.len() {
                let delta_start = self.api_anchor_entries.max(start_idx);
                let delta_estimate: usize = self.messages[delta_start..]
                    .iter()
                    .map(estimate_message_tokens)
                    .sum();
                if delta_estimate == 0 {
                    return self.api_token_anchor as usize;
                }
                // Apply 4/3 safety margin to the delta only
                let delta_with_margin = (delta_estimate as f64 * 4.0 / 3.0).ceil() as usize;
                return self.api_token_anchor as usize + delta_with_margin;
            }
        }

        // Full heuristic estimation (no anchor or stale anchor)
        let raw_total: usize = self.messages[start_idx..]
            .iter()
            .map(estimate_message_tokens)
            .sum();
        if raw_total == 0 {
            return 0;
        }
        // Apply 4/3 safety margin
        (raw_total as f64 * 4.0 / 3.0).ceil() as usize
    }

    /// Returns the text of the most recent user message (excluding tool_result
    /// messages). Used to derive the "active task" for task drift prevention.
    pub fn latest_user_message(&self) -> String {
        for msg in self.messages.iter().rev() {
            if msg.role != MessageRole::User {
                continue;
            }
            // Skip tool_result user messages
            if matches!(&msg.content, MessageContent::ToolResultBlocks(_)) {
                continue;
            }
            // Extract text
            match &msg.content {
                MessageContent::Text(t) => return t.clone(),
                MessageContent::Summary(s) => return s.clone(),
                MessageContent::Attachment(a) => return a.clone(),
                _ => {}
            }
        }
        String::new()
    }

    // ─── Injection Methods (system-injected prefix for cache stability) ─────

    /// Inject the current date/time as a user message with the system-injected prefix.
    /// This replaces the time injection that was previously inside the system prompt.
    /// By keeping it as a separate injected message, the system prompt remains fully
    /// static and cacheable, and the time message can be skipped for cache breakpoint
    /// placement.
    pub fn inject_time_context(&mut self) {
        let now = chrono::Local::now();
        let current_time = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let offset = now.offset();
        let timezone = offset.to_string();
        let time_msg = format!("{}[Current time: {} ({})]", SYSTEM_INJECTED_PREFIX, current_time, timezone);
        self.messages.push(Message::new(MessageRole::User, MessageContent::Text(time_msg)));
    }

    /// Inject the current todo list as a user message with the system-injected prefix.
    /// This replaces the previous approach of appending the todo reminder to the
    /// system prompt, which changed the system prompt every turn and broke prompt caching.
    pub fn inject_todo_reminder(&mut self, reminder: &str) {
        if reminder.is_empty() {
            return;
        }
        let msg = format!(
            "{}{}\n\n## Important\nUse TodoWrite tool to keep the above task list up to date as you work.",
            SYSTEM_INJECTED_PREFIX, reminder
        );
        self.messages.push(Message::new(MessageRole::User, MessageContent::Text(msg)));
    }

    /// Inject a TodoWrite idle nudge as a user message with the system-injected prefix.
    /// Used when the model hasn't used TodoWrite for a while and has no task list.
    pub fn inject_idle_reminder(&mut self, idle_msg: &str) {
        if idle_msg.is_empty() {
            return;
        }
        let msg = format!("{}{}", SYSTEM_INJECTED_PREFIX, idle_msg);
        self.messages.push(Message::new(MessageRole::User, MessageContent::Text(msg)));
    }

    /// Inject session state (tracked files, search patterns, etc.) as a user message
    /// with the system-injected prefix. This replaces the previous approach of
    /// appending to the system prompt, which changed the system prompt every turn
    /// and broke prompt caching.
    pub fn inject_session_state(&mut self, state: &str) {
        if state.is_empty() {
            return;
        }
        let msg = format!("{}{}", SYSTEM_INJECTED_PREFIX, state);
        self.messages.push(Message::new(MessageRole::User, MessageContent::Text(msg)));
    }

    /// Add anti-replay rules as a user message with the system-injected prefix.
    /// These rules prevent the model from repeating actions it has already taken.
    pub fn add_anti_replay_rules(&mut self, rules: &str) {
        if rules.is_empty() {
            return;
        }
        self.messages.push(Message::new(MessageRole::User, MessageContent::AntiReplay(rules.to_string())));
    }

    /// Add a goal block as a user message.
    pub fn add_goal_block(&mut self, content: &str) {
        self.messages.push(Message::new(MessageRole::User, MessageContent::Goal(content.to_string())));
    }

    /// Add compression instruction as a user message.
    pub fn add_compression_instruction(&mut self, level: usize) {
        self.messages.push(Message::new(MessageRole::User, MessageContent::CompressionInstruction { level }));
    }

    /// Add a compressed summary as a user message.
    pub fn add_compressed_summary(&mut self, summary: &str, topics: &str, chunk_path: &str) {
        self.messages.push(Message::new(MessageRole::User, MessageContent::CompressedSummary {
            summary: summary.to_string(),
            topics: topics.to_string(),
            chunk_path: chunk_path.to_string(),
        }));
    }

    /// Check if a compression instruction has already been added.
    pub fn has_compression_instruction(&self) -> bool {
        self.messages.iter().any(|m| {
            matches!(&m.content, MessageContent::CompressionInstruction { .. })
        })
    }

    /// Get the next compression level (incrementing).
    pub fn next_compression_level(&self) -> usize {
        self.compression_level + 1
    }

    /// Pull back the last k entries from history and return them.
    /// Used for two-layer context overflow recovery.
    pub fn pull_back_from_tail(&mut self, k: usize) -> Vec<Message> {
        if k == 0 || self.messages.len() <= 1 {
            return Vec::new();
        }
        let k = k.min(self.messages.len() - 1);
        let split_point = self.messages.len() - k;
        self.messages.split_off(split_point)
    }

    /// Re-append previously pulled-back entries to the end of history.
    pub fn re_append_entries(&mut self, entries: Vec<Message>) {
        self.messages.extend(entries);
    }

    /// Build a compact transcript for the compaction API call.
    pub fn build_compact_transcript(&self, max_messages: usize) -> String {
        let mut lines = Vec::new();
        let start = if self.messages.len() > max_messages {
            self.messages.len() - max_messages
        } else {
            0
        };
        for msg in &self.messages[start..] {
            let role = msg.role.as_str();
            let text = msg.content_to_text();
            if !text.is_empty() {
                lines.push(format!("[{}] {}", role, text));
            }
        }
        lines.join("\n")
    }

    /// Detect turn interruption in the conversation.
    /// A turn is considered interrupted if the last message is from the user
    /// and was sent while the model was still responding.
    pub fn detect_turn_interruption(&self) -> Option<TurnInterruption> {
        let msgs = &self.messages;
        if msgs.len() < 3 {
            return None;
        }
        // Check if last two messages are: assistant (incomplete) + user (new)
        let last = msgs.last()?;
        let second_last = msgs.get(msgs.len() - 2)?;
        if last.role == MessageRole::User && second_last.role == MessageRole::Assistant {
            // Check if the assistant message was incomplete (no stop reason or stop_reason != "end_turn")
            let text = second_last.content_to_text();
            if text.ends_with("...") || text.ends_with("[interrupted]") {
                return Some(TurnInterruption {
                    interrupted_at: msgs.len() - 2,
                    user_message_idx: msgs.len() - 1,
                });
            }
        }
        None
    }

    /// Apply turn interruption resume: mark the interrupted assistant turn
    /// and add a system-injected message noting the interruption.
    pub fn apply_turn_interruption_resume(&mut self, interruption: &TurnInterruption) {
        // Mark the interrupted assistant turn
        if let Some(msg) = self.messages.get_mut(interruption.interrupted_at) {
            if let MessageContent::Text(t) = &mut msg.content {
                if !t.ends_with("[interrupted]") {
                    t.push_str("\n[interrupted]");
                }
            }
        }
        // Add a resume marker as a system-injected message
        let resume_msg = format!(
            "{}The previous turn was interrupted. The user has sent a new message. \
             Please continue from where you left off, or respond to the new message.",
            SYSTEM_INJECTED_PREFIX
        );
        // Insert after the interrupted message, before the user message
        self.messages.insert(interruption.user_message_idx, Message::new(
            MessageRole::User,
            MessageContent::Text(resume_msg),
        ));
    }

    /// Build messages for the compaction API call.
    /// Returns only the messages that should be compacted (excluding system
    /// message and recent turns that should be preserved).
    pub fn entries_to_compaction_messages(&self, preserve_recent: usize) -> Vec<Message> {
        if self.messages.len() <= preserve_recent + 1 {
            return Vec::new();
        }
        // Skip system message (index 0) and last `preserve_recent` messages
        let end = self.messages.len() - preserve_recent;
        self.messages[1..end].to_vec()
    }

    /// Get the compaction boundary marker.
    /// Returns the index where the next compaction should start from.
    pub fn last_compact_boundary(&self) -> usize {
        self.api_token_anchor as usize
    }

    /// Set the compaction boundary marker.
    pub fn set_compact_boundary(&mut self, idx: usize) {
        self.api_token_anchor = idx as i64;
        self.api_anchor_entries = idx;
    }

    /// Compact the context by replacing old messages with a summary.
    /// This is the main entry point for context compaction.
    pub fn compact_context(&mut self, summary: &str, keep_recent: usize) -> usize {
        let removed = self.entries_to_compaction_messages(keep_recent);
        let removed_count = removed.len();
        if removed_count == 0 {
            return 0;
        }

        // Replace old messages with the summary
        let boundary = self.last_compact_boundary();
        let new_boundary = boundary + removed_count;

        // Add compressed summary at the boundary
        self.add_compressed_summary(summary, "", "");

        // Remove old messages between boundary and new_boundary
        if new_boundary < self.messages.len() {
            self.messages.drain(boundary..new_boundary);
        }

        // Update the compaction boundary
        self.set_compact_boundary(new_boundary);
        self.compression_level += 1;
        self.is_compacted = true;

        removed_count
    }

    /// Check if the context has been compacted at least once.
    pub fn is_compacted(&self) -> bool {
        self.is_compacted
    }

    /// Set the compacted flag.
    pub fn set_compacted(&mut self, compacted: bool) {
        self.is_compacted = compacted;
    }

    /// Removes trailing assistant entries that contain ToolUseBlocks with no
    /// matching ToolResultBlocks. This happens when the agent is interrupted
    /// mid-turn (e.g., user sends new message before tool results arrive).
    /// Without cleanup, the API rejects the conversation with a 400 error.
    pub fn drop_dangling_tool_calls(&mut self) {
        while let Some(last) = self.messages.last() {
            if last.role != MessageRole::Assistant {
                break;
            }
            let blocks = match &last.content {
                MessageContent::ToolUseBlocks(b) => b,
                _ => break, // not a tool_use entry, stop
            };
            // Since this is the last entry, there can't be tool results after it.
            // If it has any tool_use blocks, they are dangling — drop it.
            let has_tool_calls = blocks.iter().any(|b| !b.id.is_empty());
            if has_tool_calls {
                self.messages.pop();
            } else {
                break; // assistant message without tool_calls, stop
            }
        }
    }

    /// Configures the disk persistence store for tool results.
    /// When set, micro-compact will persist cleared results to disk.
    pub fn set_tool_result_store(&mut self, store: ToolResultStore) {
        self.tool_result_store = Some(store);
    }

    /// Configures the state tracker for prompt cache stability.
    /// When set, enforce_tool_result_budget will make consistent replacement
    /// decisions across turns, preserving the prompt cache prefix.
    pub fn set_content_replacement_state(&mut self, state: Arc<ContentReplacementState>) {
        self.content_replacement_state = Some(state);
    }

    /// Increments the compression level after each compaction.
    /// Used for progressive summarization.
    pub fn increment_compression_level(&mut self) {
        self.compression_level += 1;
    }

    /// Returns the current compression level.
    pub fn compression_level(&self) -> usize {
        self.compression_level
    }

    /// Enforces the per-message tool result budget. For each user message
    /// whose tool_result blocks together exceed the per-message limit, the
    /// largest FRESH (never-before-seen) results are persisted to disk and
    /// replaced with <persisted-output> previews.
    ///
    /// State is tracked by tool_use_id. Once a result is seen, its fate is frozen:
    /// previously-replaced results get the same replacement re-applied every turn
    /// (zero I/O, byte-identical), and previously-unreplaced results are never
    /// replaced later (would break prompt cache).
    ///
    /// Returns the number of newly replaced results.
    pub fn enforce_tool_result_budget(
        &mut self,
        limit: usize,
        skip_tool_names: &std::collections::HashSet<String>,
    ) -> usize {
        let store = match &self.tool_result_store {
            Some(s) => s,
            None => return 0,
        };
        let state = match &self.content_replacement_state {
            Some(s) => s,
            None => return 0,
        };
        if limit == 0 {
            return 0;
        }

        // Build tool_use_id -> tool_name mapping
        let mut tool_name_map: HashMap<String, String> = HashMap::new();
        for msg in &self.messages {
            if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
                for b in blocks {
                    if !b.id.is_empty() {
                        tool_name_map.insert(b.id.clone(), b.name.clone());
                    }
                }
            }
        }

        let mut newly_replaced = 0;

        // Process each ToolResultBlocks entry
        for msg in &mut self.messages {
            let blocks = match &mut msg.content {
                MessageContent::ToolResultBlocks(b) => b,
                _ => continue,
            };

            // Collect candidates from this message
            struct ToolResultCandidate {
                tool_use_id: String,
                content: String,
                size: usize,
                replacement: Option<String>, // cached replacement for mustReapply
            }

            let mut candidates: Vec<ToolResultCandidate> = Vec::new();
            for r in blocks.iter() {
                // Extract text content
                let content_text: String = r
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ToolResultContent::Text { text } => Some(text.clone()),
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if content_text.is_empty() || content_text.starts_with(PERSISTED_OUTPUT_TAG) {
                    continue; // skip empty or already-compacted
                }
                // Skip if already recorded as a replacement
                if self.tool_result_replacements.contains_key(&r.tool_use_id) {
                    continue;
                }
                // Skip if cleared
                if self.cleared_tool_results.contains(&r.tool_use_id) {
                    continue;
                }
                candidates.push(ToolResultCandidate {
                    tool_use_id: r.tool_use_id.clone(),
                    content: content_text,
                    size: 0,
                    replacement: None,
                });
                candidates.last_mut().unwrap().size = candidates.last().unwrap().content.len();
            }

            if candidates.is_empty() {
                continue;
            }

            // Partition by prior decision state
            let mut must_reapply: Vec<ToolResultCandidate> = Vec::new();
            let mut frozen: Vec<ToolResultCandidate> = Vec::new();
            let mut fresh: Vec<ToolResultCandidate> = Vec::new();

            for cand in candidates {
                if let Some(repl) = state.get_replacement(&cand.tool_use_id) {
                    let mut c = cand;
                    c.replacement = Some(repl);
                    must_reapply.push(c);
                } else if state.is_seen(&cand.tool_use_id) {
                    frozen.push(cand);
                } else {
                    fresh.push(cand);
                }
            }

            // Re-apply cached replacements
            for cand in &must_reapply {
                if let Some(ref repl) = cand.replacement {
                    self.tool_result_replacements
                        .insert(cand.tool_use_id.clone(), repl.clone());
                }
                state.mark_seen(&cand.tool_use_id);
            }
            for cand in &frozen {
                state.mark_seen(&cand.tool_use_id);
            }

            if fresh.is_empty() {
                continue;
            }

            // Skip tools in skip_tool_names
            let mut eligible: Vec<ToolResultCandidate> = Vec::new();
            for cand in fresh {
                let tool_name = tool_name_map.get(&cand.tool_use_id).cloned().unwrap_or_default();
                if skip_tool_names.contains(&tool_name) {
                    state.mark_seen(&cand.tool_use_id); // freeze without replacement
                } else {
                    eligible.push(cand);
                }
            }

            if eligible.is_empty() {
                continue;
            }

            // Calculate total size
            let frozen_size: usize = frozen.iter().map(|c| c.size).sum();
            let fresh_size: usize = eligible.iter().map(|c| c.size).sum();

            if frozen_size + fresh_size <= limit {
                // Under budget — mark all as seen (frozen) without replacement
                for cand in &eligible {
                    state.mark_seen(&cand.tool_use_id);
                }
                continue;
            }

            // Sort eligible by size descending (replace largest first)
            eligible.sort_by(|a, b| b.size.cmp(&a.size));

            // Select candidates to replace until under budget
            let mut remaining = frozen_size + fresh_size;
            let mut selected_indices: Vec<usize> = Vec::new();
            for (i, cand) in eligible.iter().enumerate() {
                if remaining <= limit {
                    break;
                }
                selected_indices.push(i);
                remaining -= cand.size;
            }

            // Mark non-selected as seen (frozen)
            let selected_set: std::collections::HashSet<usize> =
                selected_indices.iter().copied().collect();
            for (i, cand) in eligible.iter().enumerate() {
                if !selected_set.contains(&i) {
                    state.mark_seen(&cand.tool_use_id);
                }
            }

            if selected_indices.is_empty() {
                continue;
            }

            // Persist selected results and record replacements
            for i in selected_indices {
                let cand = &eligible[i];
                match store.persist(&cand.tool_use_id, &cand.content) {
                    Some(persisted) => {
                        let replacement = build_large_tool_result_message(&persisted);
                        state.mark_seen(&cand.tool_use_id);
                        state.record_replacement(&cand.tool_use_id, &replacement);
                        newly_replaced += 1;
                        self.tool_result_replacements
                            .insert(cand.tool_use_id.clone(), replacement);
                    }
                    None => {
                        // Persistence failed — mark as seen but unreplaced (frozen)
                        state.mark_seen(&cand.tool_use_id);
                    }
                }
            }
        }

        newly_replaced
    }

    /// Apply the per-message tool result budget with default settings.
    /// Returns true if any replacements were made.
    pub fn apply_tool_result_budget(&mut self) -> bool {
        if self.tool_result_store.is_none() || self.content_replacement_state.is_none() {
            return false;
        }
        self.enforce_tool_result_budget(MAX_TOOL_RESULTS_PER_MESSAGE_CHARS, &std::collections::HashSet::new()) > 0
    }

    /// Returns the current tool result replacements map.
    pub fn tool_result_replacements(&self) -> &HashMap<String, String> {
        &self.tool_result_replacements
    }

    /// Clears the tool result replacements map.
    pub fn clear_tool_result_replacements(&mut self) {
        self.tool_result_replacements.clear();
    }
}

// ─── Tool Result Persistence ──────────────────────────────────────────────────

/// XML tag used to wrap persisted output messages.
const PERSISTED_OUTPUT_TAG: &str = "<persisted-output>";
const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";
/// Preview size in bytes for the reference message.
const PREVIEW_SIZE_BYTES: usize = 2000;
/// Default threshold before persistence kicks in (chars).
const DEFAULT_MAX_RESULT_SIZE_CHARS: usize = 8000;
/// Per-message aggregate budget limit (chars).
const MAX_TOOL_RESULTS_PER_MESSAGE_CHARS: usize = 20000;
/// SystemInjectedPrefix is prepended to auto-injected content so cache
/// breakpoint placement can skip these messages.
pub const SYSTEM_INJECTED_PREFIX: &str = "<!-- system-injected -->";

/// Metadata about a persisted tool result.
#[derive(Debug, Clone)]
pub struct PersistedToolResult {
    pub filepath: String,
    pub original_size: usize,
    pub is_json: bool,
    pub preview: String,
    pub has_more: bool,
}

/// Persists oversized tool results to disk so they can be re-read on demand
/// after micro-compact clears them from context.
///
/// Storage path: {project_dir}/{session_id}/tool-results/{tool_use_id}.{txt|json}
#[derive(Debug)]
pub struct ToolResultStore {
    dir: PathBuf,
    project_dir: String,
    session_id: String,
}

impl ToolResultStore {
    /// Creates a store rooted at {project_dir}/{session_id}/tool-results/.
    /// If session_id is empty, uses {project_dir}/tool-results/ as a fallback.
    pub fn new(project_dir: &str, session_id: &str) -> Self {
        let dir = if session_id.is_empty() {
            PathBuf::from(project_dir).join("tool-results")
        } else {
            PathBuf::from(project_dir).join(session_id).join("tool-results")
        };
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            project_dir: project_dir.to_string(),
            session_id: session_id.to_string(),
        }
    }

    /// Saves a tool result to disk and returns metadata about the persisted file.
    /// Uses exclusive create — if the file already exists (from a prior turn),
    /// it is skipped (EEXIST is not an error). This matches upstream's idempotency guard.
    pub fn persist(&self, tool_use_id: &str, content: &str) -> Option<PersistedToolResult> {
        use std::io::Write;
        if content.is_empty() {
            return None;
        }
        let safe_id = sanitize_tool_id(tool_use_id);
        let is_json = content.starts_with('{') || content.starts_with('[');
        let ext = if is_json { "json" } else { "txt" };
        let filename = format!("{}.{}", safe_id, ext);
        let path = self.dir.join(&filename);

        // Use exclusive create — skip if file already exists (idempotent across turns)
        let mut fd = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => Some(f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Already persisted on a prior turn, fall through to generate preview
                None
            }
            Err(_) => return None,
        };
        if let Some(mut f) = fd.take() {
            if f.write_all(content.as_bytes()).is_err() {
                return None;
            }
            let _ = f.flush();
        }

        let (preview, has_more) = generate_preview(content, PREVIEW_SIZE_BYTES);
        Some(PersistedToolResult {
            filepath: path.display().to_string(),
            original_size: content.len(),
            is_json,
            preview,
            has_more,
        })
    }

    /// Checks if a tool result should be persisted based on size threshold.
    /// Returns the modified content string if persisted, or the original if not.
    pub fn maybe_persist_tool_result(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        content: &str,
        threshold: usize,
    ) -> String {
        if content.trim().is_empty() {
            return format!("({} completed with no output)", tool_name);
        }
        if content.len() <= threshold {
            return content.to_string();
        }
        match self.persist(tool_use_id, content) {
            Some(result) => build_large_tool_result_message(&result),
            None => content.to_string(),
        }
    }

    /// Loads a persisted tool result from disk by its toolUseID.
    pub fn read(&self, tool_use_id: &str) -> Result<String, String> {
        let safe_id = sanitize_tool_id(tool_use_id);
        for ext in &["txt", "json"] {
            let path = self.dir.join(format!("{}.{}", safe_id, ext));
            if let Ok(data) = std::fs::read_to_string(&path) {
                return Ok(data);
            }
        }
        Err(format!("tool result not found on disk: {}", tool_use_id))
    }
}

/// Makes a toolUseID safe for use as a filename.
fn sanitize_tool_id(tool_use_id: &str) -> String {
    tool_use_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Truncates content at a newline boundary when possible.
fn generate_preview(content: &str, max_bytes: usize) -> (String, bool) {
    if content.len() <= max_bytes {
        return (content.to_string(), false);
    }
    let truncated = &content[..max_bytes];
    let last_newline = truncated.rfind('\n');
    let cut_point = match last_newline {
        Some(pos) if pos > max_bytes / 2 => pos,
        _ => max_bytes,
    };
    (content[..cut_point].to_string(), true)
}

/// Formats a persisted tool result into the <persisted-output> XML message.
fn build_large_tool_result_message(result: &PersistedToolResult) -> String {
    format!(
        "{}\n\
         Output too large ({}). Full output saved to: {}\n\n\
         Preview (first {}):\n\
         {}{}\n\
         {}",
        PERSISTED_OUTPUT_TAG,
        format_file_size(result.original_size),
        result.filepath,
        format_file_size(PREVIEW_SIZE_BYTES),
        result.preview,
        if result.has_more { "\n...\n" } else { "\n" },
        PERSISTED_OUTPUT_CLOSING_TAG,
    )
}

/// Returns a human-readable file size string.
fn format_file_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} bytes", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ─── Content Replacement State ────────────────────────────────────────────────

/// Tracks per-conversation-thread state for the aggregate tool result budget.
/// Once a result is seen, its fate is frozen for the conversation.
///   - seen_ids: results that have passed through the budget check (replaced or not).
///   - replacements: subset of seen_ids that were persisted to disk and replaced
///     with <persisted-output> previews, mapped to the exact preview string.
#[derive(Debug, Default)]
pub struct ContentReplacementState {
    seen_ids: std::sync::Mutex<HashMap<String, bool>>,
    replacements: std::sync::Mutex<HashMap<String, String>>,
}

impl ContentReplacementState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks a tool_use_id as seen.
    pub fn mark_seen(&self, tool_use_id: &str) {
        let _ = self.seen_ids.lock().unwrap().insert(tool_use_id.to_string(), true);
    }

    /// Returns true if this tool_use_id has been seen before.
    pub fn is_seen(&self, tool_use_id: &str) -> bool {
        self.seen_ids.lock().unwrap().contains_key(tool_use_id)
    }

    /// Records a replacement for a tool_use_id.
    pub fn record_replacement(&self, tool_use_id: &str, replacement: &str) {
        let _ = self
            .replacements
            .lock()
            .unwrap()
            .insert(tool_use_id.to_string(), replacement.to_string());
    }

    /// Returns the replacement string for a tool_use_id, if any.
    pub fn get_replacement(&self, tool_use_id: &str) -> Option<String> {
        self.replacements.lock().unwrap().get(tool_use_id).cloned()
    }

    /// Returns all replacements as a HashMap for serialization.
    pub fn get_all_replacements(&self) -> HashMap<String, String> {
        self.replacements.lock().unwrap().clone()
    }

    /// Reconstructs state from records (loaded from transcript).
    pub fn reconstruct(entries: &[Message], records: &[(String, String)]) -> Self {
        let state = Self::new();
        // Collect all candidate tool_use_ids from entries
        let mut candidate_ids = std::collections::HashSet::new();
        for msg in entries {
            if let MessageContent::ToolResultBlocks(blocks) = &msg.content {
                for b in blocks {
                    if !b.tool_use_id.is_empty() {
                        candidate_ids.insert(b.tool_use_id.clone());
                    }
                }
            }
        }
        // Mark all candidates as seen
        for id in &candidate_ids {
            state.mark_seen(id);
        }
        // Apply records for replacements
        for (tool_use_id, replacement) in records {
            if candidate_ids.contains(tool_use_id.as_str()) {
                state.record_replacement(tool_use_id, replacement);
            }
        }
        state
    }
}

/// Serializable record of a content-replacement decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentReplacementRecord {
    pub kind: String,       // always "tool-result"
    pub tool_use_id: String,
    pub replacement: String,
}

/// Compression prompt builder — generates instruction text for inline
/// cache-reusing compaction at different compression levels.
pub fn build_compression_prompt(level: usize) -> String {
    match level {
        0 => {
            r#"═══════════════════════════════════════════════════════════════
CRITICAL: TASK CHANGE - MEMORY COMPRESSION MODE
═══════════════════════════════════════════════════════════════
The conversation above has ENDED. You are now in MEMORY COMPRESSION MODE.

CRITICAL INSTRUCTIONS - READ CAREFULLY:
1. This is NOT a continuation of the conversation
2. DO NOT respond to any requests in the conversation above
3. DO NOT call ANY tools or functions
4. DO NOT use tool_calls in your response
5. Your response MUST be PURE TEXT ONLY

YOUR ONLY TASK: Create a comprehensive summary of the conversation above.

REQUIRED RESPONSE FORMAT:
First output a <topics> line listing 3-6 key topic phrases (comma-separated, concise).
Then output the full summary wrapped in <summary> tags.

Example format:
<topics>Rails setup, database config, deploy pipeline, Tailwind CSS</topics>
<summary>
...full summary text...
</summary>"#
                .to_string()
        }
        1 => {
            r#"═══════════════════════════════════════════════════════════════
CRITICAL: TASK CHANGE - MEMORY COMPRESSION MODE [LEVEL 2]
═══════════════════════════════════════════════════════════════
The conversation above has ENDED. You are now in MEMORY COMPRESSION MODE.

DO NOT respond to requests. DO NOT call tools. PURE TEXT ONLY.

Create a CONCISE summary: key files, decisions, accomplishments only.

Format:
<topics>topic1, topic2, topic3</topics>
<summary>
...concise summary...
</summary>"#
                .to_string()
        }
        2 => {
            r#"═══════════════════════════════════════════════════════════════
CRITICAL: MEMORY COMPRESSION MODE [LEVEL 3]
═══════════════════════════════════════════════════════════════
DO NOT respond to requests. DO NOT call tools. PURE TEXT ONLY.

Create a MINIMAL summary: just project type, file counts, current status.

Format:
<topics>topic1, topic2</topics>
<summary>
...minimal summary...
</summary>"#
                .to_string()
        }
        _ => {
            r#"═══════════════════════════════════════════════════════════════
MEMORY COMPRESSION MODE [LEVEL 4+]
═══════════════════════════════════════════════════════════════
DO NOT respond. DO NOT call tools. PURE TEXT ONLY.

One-line summary of current state and progress.

Format:
<topics>topic1</topics>
<summary>
...one line...
</summary>"#
                .to_string()
        }
    }
}

// ─── Token Estimation ─────────────────────────────────────────────────────────

/// Estimates token count for a message using content-type-aware heuristics.
/// No safety margin is applied — the caller applies it if needed.
fn estimate_message_tokens(msg: &Message) -> usize {
    match &msg.content {
        MessageContent::Text(t) => estimate_text_tokens(t),
        MessageContent::Summary(s) => estimate_text_tokens(s),
        MessageContent::ToolUseBlocks(blocks) => {
            let mut total = 0;
            for b in blocks {
                total += 10; // tool_use overhead
                total += estimate_text_tokens(&b.name);
                if let Ok(json) = serde_json::to_string(&b.input) {
                    total += estimate_text_tokens(&json);
                }
            }
            total
        }
        MessageContent::ToolResultBlocks(blocks) => {
            let mut total = 0;
            for r in blocks {
                total += 8; // tool_result overhead
                for c in &r.content {
                    if let ToolResultContent::Text { text } = c {
                        total += estimate_text_tokens(text);
                    }
                }
            }
            total
        }
        MessageContent::Attachment(a) => estimate_text_tokens(a),
        MessageContent::CompactBoundary { .. } => 0, // boundary markers are small
        MessageContent::AntiReplay(a) => estimate_text_tokens(a),
        MessageContent::Goal(g) => estimate_text_tokens(g),
        MessageContent::CompressionInstruction { level } => estimate_text_tokens(&build_compression_prompt(*level)),
        MessageContent::CompressedSummary { summary, .. } => estimate_text_tokens(summary),
    }
}

/// Heuristic token estimation for text content.
/// Rough approximation: ~4 chars per token for natural language,
/// ~2 chars per token for code, ~3 for JSON.
fn estimate_text_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let chars = text.len();
    // Simple heuristic: average ~3 chars per token
    chars.div_ceil(3)
}

// ─── ConversationContext Missing Fields and Methods ───────────────────────────

// Additional fields needed on ConversationContext:
// These are added as optional members to avoid breaking existing code.
// The ConversationContext struct needs the following new fields:
//   - api_token_anchor: i64 (exact input_tokens from last API response)
//   - api_anchor_entries: usize (entry count when anchor was recorded)
//   - tool_result_replacements: HashMap<String, String>
//   - cleared_tool_results: HashMap<String, bool>
//   - compression_level: usize
//   - tool_result_store: Option<ToolResultStore>
//   - content_replacement_state: Option<Arc<ContentReplacementState>>

// ─── Orphaned Tool Result Fixing ──────────────────────────────────────────────

/// Fix orphaned tool_results: when the compact boundary drops tool_use
/// entries that precede it, their matching tool_results in the kept tail
/// become orphaned. Instead of silently stripping them and inserting
/// "Tool execution was interrupted", inject synthetic tool_use blocks
/// for any tool_result whose tool_use_id is not present in any assistant
/// message's tool_use blocks.
/// This preserves the real tool result while satisfying API pairing requirements.
fn fix_orphaned_tool_results(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    // Step 1: Collect all tool_use IDs from assistant messages
    let mut all_tool_use_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in &messages {
        if let Some("assistant") = msg.get("role").and_then(|r| r.as_str()) {
            if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                for block in blocks {
                    if let Some("tool_use") = block.get("type").and_then(|t| t.as_str()) {
                        if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                            all_tool_use_ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    // Step 2: Find orphaned tool_results and pair them with their preceding
    // assistant message by injecting synthetic tool_use blocks
    let mut result = messages.clone();

    for i in 0..result.len() {
        if let Some("user") = result[i].get("role").and_then(|r| r.as_str()) {
            // Find orphaned tool_results in this user message
            let mut orphaned: Vec<serde_json::Value> = Vec::new();
            if let Some(blocks) = result[i].get("content").and_then(|c| c.as_array()) {
                for block in blocks {
                    if let Some("tool_result") = block.get("type").and_then(|t| t.as_str()) {
                        if let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) {
                            if !id.is_empty() && !all_tool_use_ids.contains(id) {
                                orphaned.push(block.clone());
                                all_tool_use_ids.insert(id.to_string()); // mark as handled
                            }
                        }
                    }
                }
            }
            if orphaned.is_empty() {
                continue;
            }

            // Inject synthetic tool_use into preceding assistant message
            if i > 0 {
                if let Some("assistant") = result[i - 1].get("role").and_then(|r| r.as_str()) {
                    let synth_blocks: Vec<serde_json::Value> = orphaned.iter().map(|o| {
                        let tool_use_id = o.get("tool_use_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let tool_name = infer_tool_name_from_result(o);
                        serde_json::json!({
                            "type": "tool_use",
                            "id": tool_use_id,
                            "name": tool_name,
                            "input": {}
                        })
                    }).collect();

                    // Prepend synthetic tool_use blocks to the existing content
                    if let Some(existing) = result[i - 1].get_mut("content").and_then(|c| c.as_array_mut()) {
                        let mut new_content: Vec<serde_json::Value> = synth_blocks;
                        new_content.extend(existing.drain(..));
                        *existing = new_content;
                    }
                }
            }
        }
    }

    result
}

/// Infer a tool name from an orphaned tool_result.
/// Since the original tool_use is missing, we use content heuristics to provide
/// a meaningful placeholder that preserves conversation context.
fn infer_tool_name_from_result(result: &serde_json::Value) -> String {
    if let Some(blocks) = result.get("content") {
        if let Some(blocks_arr) = blocks.as_array() {
            for block in blocks_arr {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    // Heuristics based on common tool output patterns
                    if text.contains("lines") && text.contains("───") {
                        return "read_file".to_string();
                    }
                    if text.contains("$ ") || text.contains("> ") {
                        return "bash".to_string();
                    }
                    if text.contains("commit") || text.contains("branch") {
                        return "git".to_string();
                    }
                    if text.contains("Found") && text.contains("match") {
                        return "grep".to_string();
                    }
                    if text.contains("wrote") || text.contains("modified") {
                        return "edit_file".to_string();
                    }
                    if text.contains("directory") || text.contains("files") {
                        return "list_directory".to_string();
                    }
                }
            }
        }
    }
    "unknown_tool".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            max_context_msgs: 10,
            ..Config::default()
        }
    }

    #[test]
    fn test_add_user_message() {
        let config = test_config();
        let mut ctx = ConversationContext::new(config);
        ctx.add_user_message("Hello".to_string());
        assert_eq!(ctx.len(), 1);
    }

    #[test]
    fn test_truncate_if_needed() {
        let config = Config {
            max_context_msgs: 5,
            ..Config::default()
        };
        let mut ctx = ConversationContext::new(config);

        for i in 0..10 {
            ctx.add_user_message(format!("Message {}", i));
        }

        // Should be truncated
        assert!(ctx.len() <= 6);
    }

    #[test]
    fn test_clear() {
        let config = test_config();
        let mut ctx = ConversationContext::new(config);
        ctx.add_user_message("Hello".to_string());
        ctx.clear();
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_compact_boundary() {
        let config = test_config();
        let mut ctx = ConversationContext::new(config);
        ctx.add_user_message("Hello".to_string());
        ctx.add_assistant_text("Hi!".to_string());
        ctx.add_compact_boundary(CompactTrigger::Auto, 50000);

        assert_eq!(ctx.len(), 3);
        assert!(ctx.messages()[2].is_compact_boundary());

        // messages_after_compact_boundary should return from boundary onwards
        let after = ctx.messages_after_compact_boundary();
        assert_eq!(after.len(), 1);
        assert!(after[0].is_compact_boundary());
    }

    #[test]
    fn test_summary() {
        let config = test_config();
        let mut ctx = ConversationContext::new(config);
        ctx.add_user_message("Hello".to_string());
        ctx.add_summary("User said hello, assistant responded with greeting.".to_string());

        assert_eq!(ctx.len(), 2);
        assert!(ctx.messages()[1].is_summary());
        assert_eq!(ctx.messages()[1].text_content(),
            Some("User said hello, assistant responded with greeting."));
    }

    #[test]
    fn test_messages_after_boundary_without_boundary() {
        let config = test_config();
        let mut ctx = ConversationContext::new(config);
        ctx.add_user_message("Hello".to_string());
        ctx.add_assistant_text("Hi!".to_string());

        // No boundary, should return all messages
        let all = ctx.messages_after_compact_boundary();
        assert_eq!(all.len(), 2);
    }
}
