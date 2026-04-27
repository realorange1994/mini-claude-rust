//! Integration tests for context module

use miniclaudecode_rust::config::Config;
use miniclaudecode_rust::context::{
    ConversationContext, ConversationEntry, Message, MessageContent, MessageRole,
    ToolUseBlock, ToolResultBlock, ToolResultContent,
};
use std::collections::HashMap;

fn test_config() -> Config {
    Config {
        max_context_msgs: 10,
        ..Config::default()
    }
}

// ─── ConversationContext basic operations ───

#[test]
fn context_new() {
    let config = test_config();
    let ctx = ConversationContext::new(config);
    assert_eq!(ctx.len(), 0);
    assert!(ctx.is_empty());
}

#[test]
fn context_add_user_message() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Hello".to_string());
    assert_eq!(ctx.len(), 1);
}

#[test]
fn context_add_user_multiple() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("A".to_string());
    ctx.add_user_message("B".to_string());
    ctx.add_user_message("C".to_string());
    assert_eq!(ctx.len(), 3);
}

#[test]
fn context_add_assistant_text() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Hello".to_string());
    ctx.add_assistant_text("Hi there!".to_string());
    assert_eq!(ctx.len(), 2);
    let entries = ctx.entries();
    assert_eq!(entries[1].role, MessageRole::Assistant);
}

#[test]
fn context_add_assistant_text_empty_ignored() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Hello".to_string());
    ctx.add_assistant_text(String::new());
    assert_eq!(ctx.len(), 1); // Empty text should be ignored
}

#[test]
fn context_add_assistant_tool_calls() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Run ls".to_string());
    ctx.add_assistant_tool_calls(vec![ToolUseBlock {
        id: "call_1".to_string(),
        name: "exec".to_string(),
        input: [("command".to_string(), serde_json::json!("ls"))].into_iter().collect(),
    }]);
    assert_eq!(ctx.len(), 2);
    let entries = ctx.entries();
    match &entries[1].content {
        MessageContent::ToolUseBlocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].name, "exec");
        }
        _ => panic!("expected tool use blocks"),
    }
}

#[test]
fn context_add_tool_results() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Run ls".to_string());
    ctx.add_assistant_tool_calls(vec![ToolUseBlock {
        id: "call_1".to_string(),
        name: "exec".to_string(),
        input: HashMap::new(),
    }]);
    ctx.add_tool_results(vec![ToolResultBlock {
        tool_use_id: "call_1".to_string(),
        content: vec![ToolResultContent::Text { text: "file1.txt".to_string() }],
        is_error: false,
    }]);
    assert_eq!(ctx.len(), 3);
    let entries = ctx.entries();
    match &entries[2].content {
        MessageContent::ToolResultBlocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            assert!(!blocks[0].is_error);
        }
        _ => panic!("expected tool result blocks"),
    }
}

#[test]
fn context_add_tool_results_error() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Run ls".to_string());
    ctx.add_tool_results(vec![ToolResultBlock {
        tool_use_id: "call_1".to_string(),
        content: vec![ToolResultContent::Text { text: "Error!".to_string() }],
        is_error: true,
    }]);
    let entries = ctx.entries();
    match &entries[1].content {
        MessageContent::ToolResultBlocks(blocks) => {
            assert!(blocks[0].is_error);
        }
        _ => panic!("expected tool result blocks"),
    }
}

// ─── System prompt ───

#[test]
fn context_set_system_prompt() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.set_system_prompt("You are helpful".to_string());
    assert_eq!(ctx.system_prompt(), "You are helpful");
}

#[test]
fn context_default_system_prompt() {
    let config = test_config();
    let ctx = ConversationContext::new(config);
    assert_eq!(ctx.system_prompt(), "");
}

// ─── Truncation ───

#[test]
fn context_truncate_if_needed() {
    let config = Config {
        max_context_msgs: 5,
        ..Config::default()
    };
    let mut ctx = ConversationContext::new(config);

    for i in 0..10 {
        ctx.add_user_message(format!("Message {}", i));
    }

    // Should be truncated to max_msgs-1 + 1 = max_msgs
    assert!(ctx.len() <= 6);
}

#[test]
fn context_truncate_history() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);

    ctx.add_user_message("Start".to_string());
    for i in 0..20 {
        ctx.add_assistant_text(format!("Turn {}", i));
        ctx.add_user_message(format!("Message {}", i));
    }

    ctx.truncate_history();
    // Implementation keeps first 1 + last 10 entries when len > 12
    // After truncation: first entry + last 10 = 11 entries (if len > 12)
    assert!(ctx.len() <= 12);
}

#[test]
fn context_truncate_history_no_op() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Start".to_string());
    ctx.add_assistant_text("Hi".to_string());

    ctx.truncate_history();
    // Only 2 entries, below threshold of 12
    assert_eq!(ctx.len(), 2);
}

#[test]
fn context_aggressive_truncate_history() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);

    ctx.add_user_message("Start".to_string());
    for i in 0..10 {
        ctx.add_assistant_text(format!("Turn {}", i));
    }

    ctx.aggressive_truncate_history();
    // Implementation keeps first 1 + last 5 entries when len > 6
    assert!(ctx.len() <= 7);
}

#[test]
fn context_aggressive_truncate_history_no_op() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Start".to_string());
    ctx.add_assistant_text("Hi".to_string());

    ctx.aggressive_truncate_history();
    // Only 2 entries, below threshold of 6
    assert_eq!(ctx.len(), 2);
}

#[test]
fn context_minimum_history() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);

    ctx.add_user_message("Start".to_string());
    for i in 0..10 {
        ctx.add_assistant_text(format!("Turn {}", i));
    }

    ctx.minimum_history();
    // Implementation keeps first 1 + last 2 entries when len > 3
    assert!(ctx.len() <= 4);
}

#[test]
fn context_minimum_history_no_op() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Start".to_string());
    ctx.add_assistant_text("Hi".to_string());

    ctx.minimum_history();
    // Only 2 entries, below threshold of 3
    assert_eq!(ctx.len(), 2);
}

// ─── Clear and replace ───

#[test]
fn context_clear() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Hello".to_string());
    ctx.add_assistant_text("Hi".to_string());
    ctx.clear();
    assert!(ctx.is_empty());
}

#[test]
fn context_replace_entries() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Original".to_string());

    let new_entries = vec![
        Message::new(MessageRole::User, MessageContent::Text("New start".to_string())),
        Message::new(MessageRole::Assistant, MessageContent::Text("New response".to_string())),
    ];
    ctx.replace_messages(new_entries);

    assert_eq!(ctx.len(), 2);
    // Verify content via match (MessageContent doesn't impl PartialEq across crates)
    match &ctx.entries()[0].content {
        MessageContent::Text(t) => assert_eq!(t, "New start"),
        _ => panic!("expected text content"),
    }
}

// ─── ToolUseBlock ───

#[test]
fn tool_use_block_serialization() {
    let block = ToolUseBlock {
        id: "call_1".to_string(),
        name: "read_file".to_string(),
        input: [("path".to_string(), serde_json::json!("test.txt"))].into_iter().collect(),
    };
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains("call_1"));
    assert!(json.contains("read_file"));
    assert!(json.contains("test.txt"));
}

#[test]
fn tool_use_block_deserialization() {
    let json = r#"{"id":"c1","name":"exec","input":{"command":"ls"}}"#;
    let block: ToolUseBlock = serde_json::from_str(json).unwrap();
    assert_eq!(block.id, "c1");
    assert_eq!(block.name, "exec");
    assert_eq!(block.input["command"], "ls");
}

// ─── ToolResultBlock ───

#[test]
fn tool_result_block_serialization() {
    let block = ToolResultBlock {
        tool_use_id: "call_1".to_string(),
        content: vec![ToolResultContent::Text { text: "output".to_string() }],
        is_error: false,
    };
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains("call_1"));
    assert!(json.contains("output"));
}

#[test]
fn tool_result_block_deserialization() {
    let json = r#"{"tool_use_id":"c1","content":[{"type":"text","text":"ok"}],"is_error":true}"#;
    let block: ToolResultBlock = serde_json::from_str(json).unwrap();
    assert_eq!(block.tool_use_id, "c1");
    assert!(block.is_error);
    assert_eq!(block.content.len(), 1);
}

// ─── ToolResultContent ───

#[test]
fn tool_result_content_serialization() {
    let content = ToolResultContent::Text { text: "Hello".to_string() };
    let json = serde_json::to_string(&content).unwrap();
    assert!(json.contains("text"));
    assert!(json.contains("Hello"));
}

#[test]
fn tool_result_content_deserialization() {
    let json = r#"{"type":"text","text":"Hello"}"#;
    let content: ToolResultContent = serde_json::from_str(json).unwrap();
    match content {
        ToolResultContent::Text { text } => assert_eq!(text, "Hello"),
    }
}

// ─── Tool pairing validation ───

#[test]
fn validate_tool_pairing_keeps_valid() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Run ls".to_string());
    ctx.add_assistant_tool_calls(vec![ToolUseBlock {
        id: "call_1".to_string(),
        name: "exec".to_string(),
        input: HashMap::new(),
    }]);
    ctx.add_tool_results(vec![ToolResultBlock {
        tool_use_id: "call_1".to_string(),
        content: vec![ToolResultContent::Text { text: "output".to_string() }],
        is_error: false,
    }]);

    ctx.validate_tool_pairing();
    assert_eq!(ctx.len(), 3); // Nothing should be removed
}

#[test]
fn validate_tool_pairing_removes_orphan() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Start".to_string());
    // Simulate truncation: tool_use is gone but tool_result remains
    ctx.add_tool_results(vec![ToolResultBlock {
        tool_use_id: "call_deleted".to_string(),
        content: vec![ToolResultContent::Text { text: "orphan".to_string() }],
        is_error: false,
    }]);
    ctx.add_assistant_text("Response".to_string());
    ctx.add_user_message("Next".to_string());

    assert_eq!(ctx.len(), 4);
    ctx.validate_tool_pairing();
    // Orphaned tool_result message should be removed
    assert_eq!(ctx.len(), 3);
}

#[test]
fn validate_tool_pairing_partial_removal() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Start".to_string());
    ctx.add_assistant_tool_calls(vec![
        ToolUseBlock { id: "call_1".to_string(), name: "a".to_string(), input: HashMap::new() },
        ToolUseBlock { id: "call_2".to_string(), name: "b".to_string(), input: HashMap::new() },
    ]);
    ctx.add_tool_results(vec![
        ToolResultBlock { tool_use_id: "call_1".to_string(), content: vec![ToolResultContent::Text { text: "r1".to_string() }], is_error: false },
        ToolResultBlock { tool_use_id: "call_2".to_string(), content: vec![ToolResultContent::Text { text: "r2".to_string() }], is_error: false },
        ToolResultBlock { tool_use_id: "call_deleted".to_string(), content: vec![ToolResultContent::Text { text: "orphan".to_string() }], is_error: false },
    ]);

    ctx.validate_tool_pairing();
    assert_eq!(ctx.len(), 3); // One tool_result removed from the message
}

// ─── Role alternation fix ───

#[test]
fn fix_role_alternation_merges_consecutive_user() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("First".to_string());
    ctx.add_user_message("Second".to_string());
    ctx.add_assistant_text("Response".to_string());

    assert_eq!(ctx.len(), 3);
    ctx.fix_role_alternation();
    // Two consecutive user messages should be merged
    assert_eq!(ctx.len(), 2);
}

#[test]
fn fix_role_alternation_merges_consecutive_assistant() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("Hello".to_string());
    ctx.add_assistant_text("Part 1".to_string());
    ctx.add_assistant_text("Part 2".to_string());

    assert_eq!(ctx.len(), 3);
    ctx.fix_role_alternation();
    assert_eq!(ctx.len(), 2);
}

#[test]
fn fix_role_alternation_preserves_valid_sequence() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("A".to_string());
    ctx.add_assistant_text("B".to_string());
    ctx.add_user_message("C".to_string());
    ctx.add_assistant_text("D".to_string());

    let before = ctx.len();
    ctx.fix_role_alternation();
    assert_eq!(ctx.len(), before); // No change needed
}

#[test]
fn fix_role_alternation_preserves_system_messages() {
    let config = test_config();
    let mut ctx = ConversationContext::new(config);
    ctx.add_user_message("A".to_string());
    ctx.add_compact_boundary(miniclaudecode_rust::context::CompactTrigger::Auto, 1000);
    ctx.add_user_message("B".to_string());

    // System messages are kept as-is (they're filtered out by entries_to_messages_from_ctx)
    ctx.fix_role_alternation();
    // System messages should be preserved
    assert!(ctx.len() >= 2);
}
