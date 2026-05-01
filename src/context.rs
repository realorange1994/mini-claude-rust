use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Manages conversation message history and system prompt
#[derive(Debug)]
pub struct ConversationContext {
    config: Config,
    messages: Vec<Message>,
    #[allow(dead_code)]
    system_prompt: String,
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

                // Merge and ensure role alternation -- remove consecutive same-role entries
                let mut merged: Vec<Message> = Vec::with_capacity(first.len() + recent.len());
                for entry in first.into_iter().chain(recent) {
                    if merged.last().is_none_or(|last| last.role != entry.role) {
                        merged.push(entry);
                    }
                    // Skip entries with same role as last kept entry
                }

                self.messages = merged;

                // After truncation, validate tool pairing and fix alternation
                self.validate_tool_pairing();
                self.fix_role_alternation();
            } else {
                self.messages = vec![self.messages[0].clone()];
            }
        }
    }

    /// TruncateHistory drops older messages to recover from context overflow.
    /// Keeps the first entry (initial user message) and the last 10 entries.
    pub fn truncate_history(&mut self) {
        if self.messages.len() <= 12 {
            return;
        }
        let keep = 10;
        let first = self.messages[0..1].to_vec();
        let recent = self.messages[self.messages.len() - keep..].to_vec();
        self.messages = [first, recent].concat();
        self.validate_tool_pairing();
        self.fix_role_alternation();
    }

    /// AggressiveTruncateHistory drops more aggressively - keeps only first and last 5.
    pub fn aggressive_truncate_history(&mut self) {
        if self.messages.len() <= 6 {
            return;
        }
        let keep = 5;
        let first = self.messages[0..1].to_vec();
        let recent = self.messages[self.messages.len() - keep..].to_vec();
        self.messages = [first, recent].concat();
        self.validate_tool_pairing();
        self.fix_role_alternation();
    }

    /// MinimumHistory drops to bare minimum - only first user message and last 2 entries.
    pub fn minimum_history(&mut self) {
        if self.messages.len() <= 3 {
            return;
        }
        let first = self.messages[0..1].to_vec();
        let recent = self.messages[self.messages.len() - 2..].to_vec();
        self.messages = [first, recent].concat();
        self.validate_tool_pairing();
        self.fix_role_alternation();
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

        // Pass 2: Remove orphaned tool_results, collect surviving result IDs
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

        // Pass 4: Remove orphaned tool_use blocks (call without matching result)
        let mut i = 0;
        while i < self.messages.len() {
            let msg = &self.messages[i];
            if msg.role == MessageRole::Assistant {
                if let MessageContent::ToolUseBlocks(blocks) = &msg.content {
                    let kept: Vec<ToolUseBlock> = blocks
                        .iter()
                        .filter(|b| b.id.is_empty() || result_ids.contains(&b.id))
                        .cloned()
                        .collect();
                    let removed = blocks.len() - kept.len();
                    if removed > 0 {
                        if kept.is_empty() {
                            // Entire message was orphaned -- replace with placeholder
                            self.messages[i].content = MessageContent::Text(
                                "(tool call removed -- result was truncated)".to_string(),
                            );
                        } else {
                            self.messages[i].content = MessageContent::ToolUseBlocks(kept);
                        }
                    }
                }
            }
            i += 1;
        }
    }

    /// Ensures strict user/assistant alternation by merging consecutive
    /// messages with the same role. Critical for Anthropic API compliance.
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
                    // Merge same-role consecutive messages
                    match &msg.content {
                        MessageContent::Text(b) => {
                            if let MessageContent::Text(a) = &mut last.content {
                                a.push_str("\n\n");
                                a.push_str(b);
                            } else {
                                // Type mismatch: serialize last content to text,
                                // append new text (avoids silent content loss)
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
                            } else {
                                let prev = last.content_to_text();
                                let results: Vec<String> = b.iter()
                                    .map(|r| {
                                        let texts: Vec<String> = r.content.iter()
                                            .map(|c| match c {
                                                ToolResultContent::Text { text } => text.clone(),
                                            })
                                            .collect();
                                        format!("[tool_result: {}] {}", r.tool_use_id, texts.join(" "))
                                    })
                                    .collect();
                                last.content = MessageContent::Text(
                                    format!("{}\n\n{}", prev, results.join(" ")),
                                );
                            }
                        }
                        MessageContent::Summary(b) => {
                            if let MessageContent::Summary(a) = &mut last.content {
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
                            // (handles CompactBoundary and any future types)
                            let prev = last.content_to_text();
                            let curr = msg.content_to_text();
                            last.content = MessageContent::Text(
                                format!("{}\n\n{}", prev, curr),
                            );
                        }
                    }
                    continue;
                }
            }
            merged.push(msg);
        }
        self.messages = merged;
    }

    /// Micro-compact: clears content of old tool results beyond the keep_recent window.
    /// Returns the number of tool result entries that were cleared.
    /// Tool use IDs are preserved to maintain pairing validity.
    pub fn micro_compact_entries(&mut self, keep_recent: usize, placeholder: &str) -> usize {
        let keep_recent = if keep_recent == 0 { 5 } else { keep_recent };
        let placeholder = if placeholder.is_empty() {
            "[Old tool result content cleared]"
        } else {
            placeholder
        };

        // Count tool_result entries from the end (recent first)
        let mut recent_count = 0;
        let mut cleared = 0;

        // Iterate backwards to find tool result entries
        for i in (0..self.messages.len()).rev() {
            if let MessageContent::ToolResultBlocks(blocks) = &mut self.messages[i].content {
                if recent_count < keep_recent {
                    recent_count += 1;
                    continue;
                }
                // Clear this tool result: replace content with placeholder, keep ToolUseIDs
                for block in blocks.iter_mut() {
                    block.content = vec![ToolResultContent::Text {
                        text: placeholder.to_string(),
                    }];
                }
                cleared += 1;
            }
        }
        cleared
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
                                if let ToolResultContent::Text { text } = c {
                                    skip_paths.iter().any(|p| text.contains(p))
                                } else {
                                    false
                                }
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
