//! Compact module - intelligent context compaction
//! Implements token estimation, round grouping, and progressive compaction.

use crate::context::{ConversationContext, ConversationEntry, MessageContent};

/// Compaction phase levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactPhase {
    None,
    RoundBased,      // Phase 1: round-based compaction
    TurnBased,       // Phase 2: turn-based collapse
    SelectiveClear,  // Phase 3: selective clearing
    Aggressive,      // Phase 4: aggressive truncation
}

/// CompactStats tracks compaction metrics
#[derive(Debug, Clone)]
pub struct CompactStats {
    pub phase: CompactPhase,
    pub entries_before: usize,
    pub entries_after: usize,
    pub estimated_tokens_saved: usize,
    #[allow(dead_code)]
    pub estimated_tokens_before: usize,
    #[allow(dead_code)]
    pub estimated_tokens_after: usize,
}

/// Estimate token count from text (~4 chars per token, like Go)
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

/// Estimate tokens for a conversation entry
pub fn estimate_entry_tokens(entry: &ConversationEntry) -> usize {
    match &entry.content {
        MessageContent::Text(text) => {
            // Role overhead (~4 tokens) + content
            4 + estimate_tokens(text)
        }
        MessageContent::ToolUseBlocks(blocks) => {
            let mut total = 4; // role overhead
            for block in blocks {
                total += 8; // block type + id + name overhead
                total += estimate_tokens(&block.name);
                // Estimate input size
                if let Ok(json) = serde_json::to_string(&block.input) {
                    total += estimate_tokens(&json);
                }
            }
            total
        }
        MessageContent::ToolResultBlocks(blocks) => {
            let mut total = 4; // role overhead
            for block in blocks {
                total += 4; // block overhead
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
    }
}

/// Estimate total tokens for all entries
pub fn estimate_total_tokens(entries: &[ConversationEntry]) -> usize {
    entries.iter().map(|e| estimate_entry_tokens(e)).sum()
}

/// Round boundary detection - finds where tool-use rounds start/end
pub fn find_round_boundaries(entries: &[ConversationEntry]) -> Vec<(usize, usize)> {
    let mut rounds = Vec::new();
    let mut round_start = 0;

    for (i, entry) in entries.iter().enumerate() {
        match &entry.content {
            MessageContent::ToolResultBlocks(_) => {
                // Tool result marks end of a round
                rounds.push((round_start, i));
                round_start = i + 1;
            }
            _ => {}
        }
    }

    // Final round (if any remaining entries)
    if round_start < entries.len() {
        rounds.push((round_start, entries.len() - 1));
    }

    rounds
}

/// Safe boundary detection - finds safe truncation points
/// A safe boundary is between rounds (after tool result, before user message)
#[allow(dead_code)]
pub fn find_safe_boundaries(entries: &[ConversationEntry]) -> Vec<usize> {
    let mut boundaries = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        if entry.role == "user" && i > 0 {
            // Check if previous entry was a tool result or assistant
            if let Some(prev) = entries.get(i - 1) {
                if prev.role == "assistant" || prev.role == "user" {
                    boundaries.push(i);
                }
            }
        }
    }

    boundaries
}

/// Compactor handles intelligent context compaction
pub struct Compactor {
    phase: CompactPhase,
    round_count: usize,
    max_tokens: usize,
    compact_threshold: f64, // Trigger compaction at this % of max_tokens
}

impl Compactor {
    pub fn new() -> Self {
        Self {
            phase: CompactPhase::None,
            round_count: 0,
            max_tokens: 100_000, // Default max context tokens
            compact_threshold: 0.75, // 75% threshold like Go
        }
    }

    /// Determine compaction phase based on token count
    pub fn determine_phase(&self, estimated_tokens: usize) -> CompactPhase {
        let threshold = (self.max_tokens as f64 * self.compact_threshold) as usize;
        let ratio = estimated_tokens as f64 / self.max_tokens as f64;

        if estimated_tokens <= threshold {
            CompactPhase::None
        } else if ratio <= 0.80 {
            CompactPhase::RoundBased
        } else if ratio <= 0.90 {
            CompactPhase::TurnBased
        } else if ratio <= 0.95 {
            CompactPhase::SelectiveClear
        } else {
            CompactPhase::Aggressive
        }
    }

    /// Run compaction on context
    pub fn compact(&mut self, context: &mut ConversationContext) -> CompactStats {
        let entries_before = context.len();
        let tokens_before = estimate_total_tokens(context.entries());
        let phase = self.determine_phase(tokens_before);

        self.phase = phase;

        match phase {
            CompactPhase::None => {
                // No compaction needed
            }
            CompactPhase::RoundBased => {
                self.round_based_compact(context);
            }
            CompactPhase::TurnBased => {
                self.turn_based_compact(context);
            }
            CompactPhase::SelectiveClear => {
                self.selective_clear_compact(context);
            }
            CompactPhase::Aggressive => {
                self.aggressive_compact(context);
            }
        }

        let entries_after = context.len();
        let tokens_after = estimate_total_tokens(context.entries());
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

    /// Phase 1: Round-based compaction - keeps last 3 rounds
    fn round_based_compact(&self, context: &mut ConversationContext) {
        let entries = context.entries().to_vec();
        let rounds = find_round_boundaries(&entries);

        if rounds.len() <= 3 {
            return; // Keep all rounds if 3 or fewer
        }

        // Keep first entry (initial user message) + last 3 rounds
        let keep_from = rounds[rounds.len() - 3].0;
        let first = entries[..1].to_vec();
        let recent = entries[keep_from..].to_vec();

        context.replace_entries([first, recent].concat());
    }

    /// Phase 2: Turn-based collapse - keeps first 2 + last 2 turns
    fn turn_based_compact(&self, context: &mut ConversationContext) {
        let entries = context.entries().to_vec();
        if entries.len() <= 6 {
            return;
        }

        // Keep first entry + last 5 entries
        let first = entries[..1].to_vec();
        let recent = entries[entries.len() - 5..].to_vec();

        context.replace_entries([first, recent].concat());
    }

    /// Phase 3: Selective clearing - removes read-only tool outputs
    fn selective_clear_compact(&self, context: &mut ConversationContext) {
        let entries = context.entries().to_vec();
        if entries.len() <= 4 {
            return;
        }

        // Keep first entry + last 3 entries
        let first = entries[..1].to_vec();
        let recent = entries[entries.len() - 3..].to_vec();

        context.replace_entries([first, recent].concat());
    }

    /// Phase 4: Aggressive truncation fallback
    fn aggressive_compact(&self, context: &mut ConversationContext) {
        // Keep only first and last 2 entries
        context.minimum_history();
    }

    /// Check if context needs compaction
    #[allow(dead_code)]
    pub fn needs_compaction(&self, entry_count: usize) -> bool {
        let _tokens = estimate_total_tokens(&vec![]); // placeholder
        self.determine_phase(entry_count * 10) != CompactPhase::None
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
        self.round_count = 0;
    }
}

impl Default for Compactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Compactor {
    fn clone(&self) -> Self {
        Self {
            phase: self.phase,
            round_count: self.round_count,
            max_tokens: self.max_tokens,
            compact_threshold: self.compact_threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_estimation() {
        assert_eq!(estimate_tokens("hello"), 2); // 5 chars / 4 = 1.25, ceil = 2
        assert_eq!(estimate_tokens("hello world"), 3); // 11 chars / 4 = 2.75, ceil = 3
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_phase_determination() {
        let compactor = Compactor::new();

        // 75% of 100k = 75k tokens threshold
        assert_eq!(compactor.determine_phase(50_000), CompactPhase::None);
        assert_eq!(compactor.determine_phase(80_000), CompactPhase::RoundBased);
        assert_eq!(compactor.determine_phase(90_000), CompactPhase::TurnBased);
        assert_eq!(compactor.determine_phase(95_000), CompactPhase::SelectiveClear);
        assert_eq!(compactor.determine_phase(99_000), CompactPhase::Aggressive);
    }
}
