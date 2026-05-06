use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

        let prev_write = self.turns_since_last_write.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    },
    /// Summary of compressed conversation history (role: user)
    /// This is injected after compaction to preserve semantic continuity
    Summary(String),
    /// Attachment content for post-compact recovery (role: user)
    /// Re-injects file/skill content after compaction
    Attachment(String),
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
            MessageContent::CompactBoundary { trigger, pre_compact_tokens } => {
                format!("[compact boundary: {}, {} tokens]", trigger, pre_compact_tokens)
            }
            MessageContent::Attachment(a) => a.clone(),
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
        self.truncate_if_needed();
    }

    /// Add assistant tool use blocks
    pub fn add_assistant_tool_calls(&mut self, tool_calls: Vec<ToolUseBlock>) {
        self.messages.push(Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(tool_calls),
        ));
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
        self.messages.push(Message::new(
            MessageRole::System,
            MessageContent::CompactBoundary {
                trigger,
                pre_compact_tokens,
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
                    if let Some(name) = tool_name_map.get(&b.tool_use_id) {
                        if is_compactable_tool(name) {
                            // Only consider compactable if the result is large enough to justify clearing.
                            // Small results (< min_char_count) are preserved to prevent amnesia.
                            let total_chars: usize = b.content.iter().map(|c| {
                                match c {
                                    ToolResultContent::Text { text } => text.len(),
                                    _ => 0,
                                }
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
                    if let Some(name) = tool_name_map.get(&block.tool_use_id) {
                        if is_compactable_tool(name) {
                            // Check size threshold: preserve small results to prevent amnesia
                            let total_chars: usize = block.content.iter().map(|c| {
                                match c {
                                    ToolResultContent::Text { text } => text.len(),
                                    _ => 0,
                                }
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
