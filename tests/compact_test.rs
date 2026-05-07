//! Integration tests for compact module

use miniclaudecode_rust::compact::{
    estimate_tokens, estimate_message_tokens, estimate_total_tokens,
    find_round_boundaries,
    Compactor, CompactPhase,
};
use miniclaudecode_rust::config::Config;
use miniclaudecode_rust::context::{ConversationContext, Message, MessageContent, MessageRole, CompactTrigger, ToolUseBlock, ToolResultBlock, ToolResultContent};
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

// ─── estimate_message_tokens ───

#[test]
fn estimate_text_message_tokens() {
    let msg = Message::new(MessageRole::User, MessageContent::Text("Hello world".to_string()));
    let tokens = estimate_message_tokens(&msg);
    // 3 (role overhead) + estimate_tokens("Hello world") = 3 + 3 = 6
    assert_eq!(tokens, 6);
}

#[test]
fn estimate_empty_text_message_tokens() {
    let msg = Message::new(MessageRole::User, MessageContent::Text(String::new()));
    let tokens = estimate_message_tokens(&msg);
    assert_eq!(tokens, 3); // just role overhead
}

#[test]
fn estimate_tool_use_message_tokens() {
    let mut input: HashMap<String, serde_json::Value> = HashMap::new();
    input.insert("path".to_string(), serde_json::json!("test.txt"));

    let msg = Message::new(
        MessageRole::Assistant,
        MessageContent::ToolUseBlocks(vec![ToolUseBlock {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input,
        }]),
    );
    let tokens = estimate_message_tokens(&msg);
    assert!(tokens > 3);
}

#[test]
fn estimate_tool_result_message_tokens() {
    let msg = Message::new(
        MessageRole::User,
        MessageContent::ToolResultBlocks(vec![ToolResultBlock {
            tool_use_id: "call_1".to_string(),
            content: vec![ToolResultContent::Text { text: "Result".to_string() }],
            is_error: false,
        }]),
    );
    let tokens = estimate_message_tokens(&msg);
    assert!(tokens > 0);
}

#[test]
fn estimate_summary_message_tokens() {
    let msg = Message::new(
        MessageRole::User,
        MessageContent::Summary("A brief summary of the conversation.".to_string()),
    );
    let tokens = estimate_message_tokens(&msg);
    assert!(tokens > 3);
}

#[test]
fn estimate_compact_boundary_message_tokens() {
    let msg = Message::new(
        MessageRole::System,
        MessageContent::CompactBoundary {
            trigger: CompactTrigger::Auto,
            pre_compact_tokens: 50000,
            uuid: uuid::Uuid::new_v4().to_string(),
        },
    );
    let tokens = estimate_message_tokens(&msg);
    assert_eq!(tokens, 15); // fixed overhead
}

// ─── estimate_total_tokens ───

#[test]
fn estimate_total_empty() {
    let messages: Vec<Message> = vec![];
    assert_eq!(estimate_total_tokens(&messages), 0);
}

#[test]
fn estimate_total_single_message() {
    let messages = vec![Message::new(
        MessageRole::User,
        MessageContent::Text("test".to_string()),
    )];
    let total = estimate_total_tokens(&messages);
    assert!(total > 0);
}

#[test]
fn estimate_total_multiple_messages() {
    let messages = vec![
        Message::new(MessageRole::User, MessageContent::Text("Hello".to_string())),
        Message::new(MessageRole::Assistant, MessageContent::Text("Hi there".to_string())),
    ];
    let total = estimate_total_tokens(&messages);
    // estimate_total_tokens applies 4/3 padding, so it's ceil(sum * 4/3)
    // not equal to raw sum of individual estimates
    let individual = estimate_message_tokens(&messages[0]) + estimate_message_tokens(&messages[1]);
    assert_eq!(total, ((individual as f64 * 4.0 / 3.0).ceil() as usize));
}

// ─── find_round_boundaries ───

#[test]
fn find_round_boundaries_empty() {
    let messages: Vec<Message> = vec![];
    let rounds = find_round_boundaries(&messages);
    assert!(rounds.is_empty());
}

#[test]
fn find_round_boundaries_single_user() {
    let messages = vec![Message::new(
        MessageRole::User,
        MessageContent::Text("Hello".to_string()),
    )];
    let rounds = find_round_boundaries(&messages);
    assert_eq!(rounds, vec![(0, 0)]);
}

#[test]
fn find_round_boundaries_user_assistant() {
    let messages = vec![
        Message::new(MessageRole::User, MessageContent::Text("Hello".to_string())),
        Message::new(MessageRole::Assistant, MessageContent::Text("Hi".to_string())),
    ];
    let rounds = find_round_boundaries(&messages);
    assert_eq!(rounds, vec![(0, 1)]);
}

#[test]
fn find_round_boundaries_with_tool_result() {
    let messages = vec![
        Message::new(MessageRole::User, MessageContent::Text("Run command".to_string())),
        Message::new(
            MessageRole::Assistant,
            MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                id: "call_1".to_string(),
                name: "exec".to_string(),
                input: HashMap::new(),
            }]),
        ),
        Message::new(
            MessageRole::User,
            MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "call_1".to_string(),
                content: vec![ToolResultContent::Text { text: "output".to_string() }],
                is_error: false,
            }]),
        ),
        Message::new(MessageRole::Assistant, MessageContent::Text("Done".to_string())),
    ];
    let rounds = find_round_boundaries(&messages);
    // Tool result at index 2 should mark end of first round (0, 2)
    // Remaining entries form second round (3, 3)
    assert_eq!(rounds, vec![(0, 2), (3, 3)]);
}

#[test]
fn find_round_boundaries_multiple_tool_results() {
    let messages = vec![
        Message::new(MessageRole::User, MessageContent::Text("A".to_string())),
        Message::new(MessageRole::Assistant, MessageContent::ToolUseBlocks(vec![])),
        Message::new(
            MessageRole::User,
            MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "c1".to_string(),
                content: vec![ToolResultContent::Text { text: "R1".to_string() }],
                is_error: false,
            }]),
        ),
        Message::new(MessageRole::Assistant, MessageContent::ToolUseBlocks(vec![])),
        Message::new(
            MessageRole::User,
            MessageContent::ToolResultBlocks(vec![ToolResultBlock {
                tool_use_id: "c2".to_string(),
                content: vec![ToolResultContent::Text { text: "R2".to_string() }],
                is_error: false,
            }]),
        ),
    ];
    let rounds = find_round_boundaries(&messages);
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0], (0, 2));
    assert_eq!(rounds[1], (3, 4));
}

// ─── Compactor (legacy truncation tests, LLM compaction requires async + API) ───

#[test]
fn compactor_compact_no_op() {
    let config = Config::default();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Hello".to_string());

    let mut compactor = Compactor::new();
    // Won't trigger compaction with only 1 message
    let rt = tokio::runtime::Runtime::new().unwrap();
    let stats = rt.block_on(compactor.compact(&mut ctx, &reqwest::Client::new(), "claude-sonnet-4-20250514", "sk-fake", "https://api.example.com"));
    assert_eq!(stats.phase, CompactPhase::None);
    assert_eq!(stats.entries_before, 1);
    assert_eq!(stats.entries_after, 1);
}

#[test]
fn compactor_legacy_phase() {
    let config = Config::default();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Start".to_string());

    // Create many entries to exceed threshold
    for i in 0..20 {
        ctx.add_assistant_text(format!("Response {}", i));
        ctx.add_user_message(format!("Follow-up {}", i));
    }

    let mut compactor = Compactor::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let stats = rt.block_on(compactor.compact(&mut ctx, &reqwest::Client::new(), "claude-sonnet-4-20250514", "sk-fake", "https://api.example.com"));
    // LLM compaction will fail (fake key), so fallback to legacy truncation
    assert!(stats.entries_after <= stats.entries_before);
}

#[test]
fn compactor_reset() {
    let mut c = Compactor::new();
    c.reset();
    assert_eq!(c.phase(), CompactPhase::None);
}

// ─── ContextWindowTracker ───

#[test]
fn context_window_tracker_threshold() {
    let tracker = miniclaudecode_rust::compact::ContextWindowTracker::new(
        "claude-sonnet-4-20250514", 0.75, 13_000,
    );
    assert_eq!(tracker.effective_window(), 980_000); // 1M - 20K (Sonnet-4 supports 1M context)
    // threshold = min(980K * 0.75, 980K - 13K) = min(735K, 967K) = 735K
    assert_eq!(tracker.compact_threshold(), 735_000);
}

#[test]
fn context_window_tracker_should_compact() {
    let tracker = miniclaudecode_rust::compact::ContextWindowTracker::new(
        "claude-sonnet-4-20250514", 0.75, 13_000,
    );

    // Create enough messages to trigger compaction (threshold is now 735K for Sonnet-4)
    let mut messages = Vec::new();
    for _i in 0..30000 {
        messages.push(Message::new(
            MessageRole::User,
            MessageContent::Text("A".repeat(100)), // ~25 tokens each
        ));
    }

    assert!(tracker.should_compact(&messages));
}

#[test]
fn context_window_tracker_should_not_compact() {
    let tracker = miniclaudecode_rust::compact::ContextWindowTracker::new(
        "claude-sonnet-4-20250514", 0.75, 13_000,
    );

    let messages = vec![Message::new(
        MessageRole::User,
        MessageContent::Text("Hello".to_string()),
    )];

    assert!(!tracker.should_compact(&messages));
}
