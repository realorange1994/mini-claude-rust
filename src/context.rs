use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents a single entry in the conversation history
#[derive(Debug, Clone)]
pub enum MessageContent {
    Text(String),
    ToolUseBlocks(Vec<ToolUseBlock>),
    ToolResultBlocks(Vec<ToolResultBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub content: Vec<ToolResultContent>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

#[derive(Debug, Clone)]
pub struct ConversationEntry {
    pub role: String,
    pub content: MessageContent,
}

/// Manages conversation message history and system prompt
#[derive(Debug)]
pub struct ConversationContext {
    config: Config,
    entries: Vec<ConversationEntry>,
    #[allow(dead_code)]
    system_prompt: String,
}

impl ConversationContext {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            entries: Vec::new(),
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

    pub fn add_user_message(&mut self, content: String) {
        self.entries.push(ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::Text(content),
        });
        self.truncate_if_needed();
    }

    pub fn add_assistant_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.entries.push(ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::Text(text),
        });
        self.truncate_if_needed();
    }

    pub fn add_assistant_tool_calls(&mut self, tool_calls: Vec<ToolUseBlock>) {
        self.entries.push(ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::ToolUseBlocks(tool_calls),
        });
        self.truncate_if_needed();
    }

    pub fn add_tool_results(&mut self, results: Vec<ToolResultBlock>) {
        self.entries.push(ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::ToolResultBlocks(results),
        });
        self.truncate_if_needed();
    }

    /// Get all entries
    pub fn entries(&self) -> &[ConversationEntry] {
        &self.entries
    }

    /// Get entry count
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all entries except system prompt
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Replace all entries (used by compactor)
    pub fn replace_entries(&mut self, entries: Vec<ConversationEntry>) {
        self.entries = entries;
    }

    fn truncate_if_needed(&mut self) {
        let max_msgs = self.config.max_context_msgs;
        if self.entries.len() > max_msgs {
            let keep = max_msgs.saturating_sub(1);
            if keep > 0 {
                let first = self.entries[..1].to_vec();
                let recent = self.entries[self.entries.len() - keep..].to_vec();

                // Merge and ensure role alternation — remove consecutive same-role entries
                let mut merged: Vec<ConversationEntry> = Vec::with_capacity(first.len() + recent.len());
                for entry in first.into_iter().chain(recent) {
                    if merged.last().is_none_or(|last| last.role != entry.role) {
                        merged.push(entry);
                    }
                    // Skip entries with same role as last kept entry
                }

                self.entries = merged;
            } else {
                self.entries = vec![self.entries[0].clone()];
            }
        }
    }

    /// TruncateHistory drops older messages to recover from context overflow.
    /// Keeps the first entry (initial user message) and the last 10 entries.
    pub fn truncate_history(&mut self) {
        if self.entries.len() <= 12 {
            return;
        }
        let keep = 10;
        let first = self.entries[0..1].to_vec();
        let recent = self.entries[self.entries.len() - keep..].to_vec();
        self.entries = [first, recent].concat();
    }

    /// AggressiveTruncateHistory drops more aggressively - keeps only first and last 5.
    pub fn aggressive_truncate_history(&mut self) {
        if self.entries.len() <= 6 {
            return;
        }
        let keep = 5;
        let first = self.entries[0..1].to_vec();
        let recent = self.entries[self.entries.len() - keep..].to_vec();
        self.entries = [first, recent].concat();
    }

    /// MinimumHistory drops to bare minimum - only first user message and last 2 entries.
    pub fn minimum_history(&mut self) {
        if self.entries.len() <= 3 {
            return;
        }
        let first = self.entries[0..1].to_vec();
        let recent = self.entries[self.entries.len() - 2..].to_vec();
        self.entries = [first, recent].concat();
    }

    /// CompactContext performs intelligent compaction (placeholder - full implementation in compact.rs)
    #[allow(dead_code)]
    pub fn compact_context(&mut self) -> bool {
        // TODO: Implement full compaction logic
        false
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
}
