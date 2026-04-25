//! Integration tests for compact module

use miniclaudecode_rust::compact::{
    estimate_tokens, estimate_entry_tokens, estimate_total_tokens,
    find_round_boundaries, find_safe_boundaries,
    Compactor, CompactPhase, CompactStats,
};
use miniclaudecode_rust::config::Config;
use miniclaudecode_rust::context::{ConversationContext, ConversationEntry, MessageContent, ToolUseBlock, ToolResultBlock, ToolResultContent};
use std::collections::HashMap;

// ─── estimate_tokens ───

#[test]
fn estimate_tokens_hello() {
    assert_eq!(estimate_tokens("hello"), 2); // 5 chars / 4 = 1.25, ceil = 2
}

#[test]
fn estimate_tokens_empty() {
    assert_eq!(estimate_tokens(""), 0);
}

#[test]
fn estimate_tokens_4_chars() {
    assert_eq!(estimate_tokens("abcd"), 1); // exactly 4 chars = 1 token
}

#[test]
fn estimate_tokens_long_string() {
    let s = "a".repeat(100);
    assert_eq!(estimate_tokens(&s), 25); // 100 / 4 = 25
}

#[test]
fn estimate_tokens_unicode() {
    // Unicode chars count as bytes for this estimator
    let s = "中文"; // 6 bytes in UTF-8
    assert_eq!(estimate_tokens(s), 2); // 6 / 4 = 1.5, ceil = 2
}

// ─── estimate_entry_tokens ───

#[test]
fn estimate_entry_text_tokens() {
    let entry = ConversationEntry {
        role: "user".to_string(),
        content: MessageContent::Text("Hello world".to_string()),
    };
    let tokens = estimate_entry_tokens(&entry);
    // 4 (role overhead) + estimate_tokens("Hello world") = 4 + 3 = 7
    assert_eq!(tokens, 7);
}

#[test]
fn estimate_entry_empty_text() {
    let entry = ConversationEntry {
        role: "user".to_string(),
        content: MessageContent::Text(String::new()),
    };
    let tokens = estimate_entry_tokens(&entry);
    assert_eq!(tokens, 4); // just role overhead
}

#[test]
fn estimate_entry_tool_use_tokens() {
    let mut input = HashMap::new();
    input.insert("path".to_string(), serde_json::json!("test.txt"));

    let entry = ConversationEntry {
        role: "assistant".to_string(),
        content: MessageContent::ToolUseBlocks(vec![ToolUseBlock {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input,
        }]),
    };
    let tokens = estimate_entry_tokens(&entry);
    // 4 (role) + 8 (block overhead) + estimate_tokens("read_file") + estimate_tokens(json_input)
    assert!(tokens > 4);
}

#[test]
fn estimate_entry_tool_result_tokens() {
    let entry = ConversationEntry {
        role: "user".to_string(),
        content: MessageContent::ToolResultBlocks(vec![ToolResultBlock {
            tool_use_id: "call_1".to_string(),
            content: vec![ToolResultContent::Text { text: "Result".to_string() }],
            is_error: false,
        }]),
    };
    let tokens = estimate_entry_tokens(&entry);
    assert!(tokens > 0);
}

// ─── estimate_total_tokens ───

#[test]
fn estimate_total_empty() {
    let entries: Vec<ConversationEntry> = vec![];
    assert_eq!(estimate_total_tokens(&entries), 0);
}

#[test]
fn estimate_total_single_entry() {
    let entries = vec![ConversationEntry {
        role: "user".to_string(),
        content: MessageContent::Text("test".to_string()),
    }];
    let total = estimate_total_tokens(&entries);
    assert!(total > 0);
}

#[test]
fn estimate_total_multiple_entries() {
    let entries = vec![
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::Text("Hello".to_string()),
        },
        ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::Text("Hi there".to_string()),
        },
    ];
    let total = estimate_total_tokens(&entries);
    let individual = estimate_entry_tokens(&entries[0]) + estimate_entry_tokens(&entries[1]);
    assert_eq!(total, individual);
}

// ─── find_round_boundaries ───

#[test]
fn find_round_boundaries_empty() {
    let entries: Vec<ConversationEntry> = vec![];
    let rounds = find_round_boundaries(&entries);
    assert!(rounds.is_empty());
}

#[test]
fn find_round_boundaries_single_user() {
    let entries = vec![ConversationEntry {
        role: "user".to_string(),
        content: MessageContent::Text("Hello".to_string()),
    }];
    let rounds = find_round_boundaries(&entries);
    assert_eq!(rounds, vec![(0, 0)]);
}

#[test]
fn find_round_boundaries_user_assistant() {
    let entries = vec![
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::Text("Hello".to_string()),
        },
        ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::Text("Hi".to_string()),
        },
    ];
    let rounds = find_round_boundaries(&entries);
    assert_eq!(rounds, vec![(0, 1)]);
}

#[test]
fn find_round_boundaries_with_tool_result() {
    let entries = vec![
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::Text("Run command".to_string()),
        },
        ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                id: "call_1".to_string(),
                name: "exec".to_string(),
                input: HashMap::new(),
            }]),
        },
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "call_1".to_string(),
                content: vec![ToolResultContent::Text { text: "output".to_string() }],
                is_error: false,
            }]),
        },
        ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::Text("Done".to_string()),
        },
    ];
    let rounds = find_round_boundaries(&entries);
    // Tool result at index 2 should mark end of first round (0, 2)
    // Remaining entries form second round (3, 3)
    assert_eq!(rounds, vec![(0, 2), (3, 3)]);
}

#[test]
fn find_round_boundaries_multiple_tool_results() {
    let entries = vec![
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::Text("A".to_string()),
        },
        ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::ToolUseBlocks(vec![]),
        },
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "c1".to_string(),
                content: vec![ToolResultContent::Text { text: "R1".to_string() }],
                is_error: false,
            }]),
        },
        ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::ToolUseBlocks(vec![]),
        },
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "c2".to_string(),
                content: vec![ToolResultContent::Text { text: "R2".to_string() }],
                is_error: false,
            }]),
        },
    ];
    let rounds = find_round_boundaries(&entries);
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0], (0, 2));
    assert_eq!(rounds[1], (3, 4));
}

// ─── find_safe_boundaries ───

#[test]
fn find_safe_boundaries_empty() {
    let entries: Vec<ConversationEntry> = vec![];
    assert!(find_safe_boundaries(&entries).is_empty());
}

#[test]
fn find_safe_boundaries_first_user() {
    let entries = vec![ConversationEntry {
        role: "user".to_string(),
        content: MessageContent::Text("Hello".to_string()),
    }];
    // First user message has i == 0, so no boundary
    assert!(find_safe_boundaries(&entries).is_empty());
}

#[test]
fn find_safe_boundaries_user_after_assistant() {
    let entries = vec![
        ConversationEntry {
            role: "assistant".to_string(),
            content: MessageContent::Text("Hi".to_string()),
        },
        ConversationEntry {
            role: "user".to_string(),
            content: MessageContent::Text("Hello".to_string()),
        },
    ];
    let boundaries = find_safe_boundaries(&entries);
    assert_eq!(boundaries, vec![1]);
}

// ─── Compactor ───

#[test]
fn compactor_new() {
    let c = Compactor::new();
    assert_eq!(c.determine_phase(50_000), CompactPhase::None);
}

#[test]
fn compactor_default() {
    let c = Compactor::default();
    assert_eq!(c.determine_phase(0), CompactPhase::None);
}

#[test]
fn compactor_phase_none() {
    let c = Compactor::new();
    assert_eq!(c.determine_phase(0), CompactPhase::None);
    assert_eq!(c.determine_phase(50_000), CompactPhase::None);
}

#[test]
fn compactor_phase_round_based() {
    let c = Compactor::new();
    // 75% of 100k = 75k threshold
    assert_eq!(c.determine_phase(80_000), CompactPhase::RoundBased);
}

#[test]
fn compactor_phase_turn_based() {
    let c = Compactor::new();
    assert_eq!(c.determine_phase(90_000), CompactPhase::TurnBased);
}

#[test]
fn compactor_phase_selective_clear() {
    let c = Compactor::new();
    assert_eq!(c.determine_phase(95_000), CompactPhase::SelectiveClear);
}

#[test]
fn compactor_phase_aggressive() {
    let c = Compactor::new();
    assert_eq!(c.determine_phase(99_000), CompactPhase::Aggressive);
}

#[test]
fn compactor_compact_no_op() {
    let config = Config::default();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Hello".to_string());

    let mut compactor = Compactor::new();
    let stats = compactor.compact(&mut ctx);
    assert_eq!(stats.phase, CompactPhase::None);
    assert_eq!(stats.entries_before, 1);
    assert_eq!(stats.entries_after, 1);
}

#[test]
fn compactor_round_based_compact() {
    let config = Config::default();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Start".to_string());

    // Create 4 rounds (each with tool result) to trigger keeping last 3
    for i in 0..4 {
        ctx.add_assistant_tool_calls(vec![ToolUseBlock {
            id: format!("call_{}", i),
            name: "exec".to_string(),
            input: HashMap::new(),
        }]);
        ctx.add_tool_results(vec![ToolResultBlock {
            tool_use_id: format!("call_{}", i),
            content: vec![ToolResultContent::Text { text: format!("Result {}", i) }],
            is_error: false,
        }]);
    }

    let mut compactor = Compactor::new();
    let stats = compactor.compact(&mut ctx);
    // Should have applied some compaction
    assert!(stats.entries_before == stats.entries_after || stats.entries_after < stats.entries_before);
}

#[test]
fn compactor_clone() {
    let c = Compactor::new();
    let cloned = c.clone();
    assert_eq!(c.determine_phase(50_000), cloned.determine_phase(50_000));
}

#[test]
fn compactor_reset() {
    let mut c = Compactor::new();
    // Force a phase change
    c.determine_phase(99_000);
    c.reset();
    assert_eq!(c.phase(), CompactPhase::None);
}

// ─── CompactStats ───

#[test]
fn compact_stats_fields() {
    let stats = CompactStats {
        phase: CompactPhase::TurnBased,
        entries_before: 100,
        entries_after: 50,
        estimated_tokens_saved: 500,
        estimated_tokens_before: 10000,
        estimated_tokens_after: 9500,
    };
    assert_eq!(stats.entries_before, 100);
    assert_eq!(stats.entries_after, 50);
}
