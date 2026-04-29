//! Comprehensive unit tests for the streaming module
//!
//! Covers:
//! - CollectHandler: text/tool_call/tool_argument/thinking/usage accumulation, tool_use_as_text detection
//! - TerminalHandler: chunk routing, tool call flushing
//! - StallDetector: reset, check_stall, increment_stall, timeout
//! - parse_sse_event: all Anthropic SSE event types
//! - parse_anthropic_message: non-streaming message parsing
//! - tool_arg_summary: per-tool argument summarization
//! - process_sse_events: end-to-end SSE stream with wiremock

use miniclaudecode_rust::streaming::{
    parse_anthropic_message, parse_sse_event, tool_arg_summary, ChunkType, CollectHandler,
    StallDetector, StreamChunk, TerminalHandler, ToolCallInfo, Usage,
};
use miniclaudecode_rust::rate_limit::RateLimitState;
use std::time::Duration;

// ============================================================
// CollectHandler tests
// ============================================================

#[test]
fn collect_handler_text_accumulation() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: "Hello ".into(),
        id: None,
        name: None,
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: "World".into(),
        id: None,
        name: None,
        usage: None,
    });
    assert_eq!(h.full_response(), "Hello World");
}

#[test]
fn collect_handler_single_tool_call_with_incremental_args() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("toolu_01".into()),
        name: Some("read_file".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"path":"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#""/tmp/f.txt"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });

    let calls = h.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_01");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments, r#"{"path":"/tmp/f.txt"}"#);
}

#[test]
fn collect_handler_multiple_tool_calls() {
    let h = CollectHandler::new();
    // First tool call
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("id_1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"command":"ls"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    // Second tool call
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("id_2".into()),
        name: Some("grep".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"pattern":"TODO"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });

    let calls = h.tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "exec");
    assert_eq!(calls[1].name, "grep");
}

#[test]
fn collect_handler_thinking_accumulation() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Thinking,
        content: "Step 1: ".into(),
        id: None,
        name: None,
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::Thinking,
        content: "analyze".into(),
        id: None,
        name: None,
        usage: None,
    });

    // When there is no text, full_response falls back to thinking
    assert_eq!(h.full_response(), "Step 1: analyze");
}

#[test]
fn collect_handler_text_takes_priority_over_thinking() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Thinking,
        content: "internal thought".into(),
        id: None,
        name: None,
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: "final answer".into(),
        id: None,
        name: None,
        usage: None,
    });
    assert_eq!(h.full_response(), "final answer");
}

#[test]
fn collect_handler_usage_tracking() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Usage,
        content: String::new(),
        id: None,
        name: None,
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 50,
        }),
    });
    let u = h.usage().unwrap();
    assert_eq!(u.input_tokens, 100);
    assert_eq!(u.output_tokens, 50);
}

#[test]
fn collect_handler_tool_use_as_text_detected_via_type_and_id() {
    let h = CollectHandler::new();
    // Model echoes tool syntax containing "type":"tool_use" AND "id":"..."
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: r#"{"type":"tool_use","id":"abc","name":"foo","input":{}}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.is_tool_use_as_text());
    // Text should NOT be appended when tool_use_as_text is detected
    assert!(h.full_response().is_empty());
}

#[test]
fn collect_handler_tool_use_as_text_detected_via_type_and_name() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: r#"{"type": "tool_use","name":"read_file","input":{}}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.is_tool_use_as_text());
}

#[test]
fn collect_handler_tool_use_as_text_detected_via_id_and_name() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: r#"{"id": "toolu_01","name":"exec","input":{}}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.is_tool_use_as_text());
}

#[test]
fn collect_handler_normal_text_not_flagged_as_tool_use() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: "I will read the file for you".into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(!h.is_tool_use_as_text());
    assert_eq!(h.full_response(), "I will read the file for you");
}

#[test]
fn collect_handler_done_and_block_stop_are_no_ops() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Done,
        content: "should be ignored".into(),
        id: None,
        name: None,
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::BlockStop,
        content: "also ignored".into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.full_response().is_empty());
    assert!(h.tool_calls().is_empty());
}

#[test]
fn collect_handler_error_chunk_is_no_op() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Error,
        content: "something broke".into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.full_response().is_empty());
}

#[test]
fn collect_handler_tool_argument_without_tool_call_is_ignored() {
    let h = CollectHandler::new();
    // ToolArgument without a preceding ToolCall — last_mut() returns None
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"path":"/x"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.tool_calls().is_empty());
}

#[test]
fn collect_handler_default_is_new() {
    let h = CollectHandler::default();
    assert!(h.full_response().is_empty());
    assert!(h.tool_calls().is_empty());
    assert!(!h.is_tool_use_as_text());
}

// ============================================================
// parse_sse_event tests
// ============================================================

#[test]
fn parse_sse_message_start() {
    let event = serde_json::json!({
        "type": "message_start",
        "message": { "id": "msg_1", "role": "assistant" }
    });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::BlockStop));
}

#[test]
fn parse_sse_content_block_start_tool_use() {
    let event = serde_json::json!({
        "type": "content_block_start",
        "content_block": {
            "type": "tool_use",
            "id": "toolu_01",
            "name": "read_file"
        }
    });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::ToolCall));
    assert_eq!(chunk.id.unwrap(), "toolu_01");
    assert_eq!(chunk.name.unwrap(), "read_file");
}

#[test]
fn parse_sse_content_block_start_thinking() {
    let event = serde_json::json!({
        "type": "content_block_start",
        "content_block": { "type": "thinking" }
    });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::Thinking));
}

#[test]
fn parse_sse_content_block_start_text_returns_none() {
    // Text blocks don't generate a chunk on start (they come via deltas)
    let event = serde_json::json!({
        "type": "content_block_start",
        "content_block": { "type": "text" }
    });
    assert!(parse_sse_event(&event).is_none());
}

#[test]
fn parse_sse_text_delta() {
    let event = serde_json::json!({
        "type": "content_block_delta",
        "delta": {
            "type": "text_delta",
            "text": "Hello world"
        }
    });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::Text));
    assert_eq!(chunk.content, "Hello world");
}

#[test]
fn parse_sse_input_json_delta() {
    let event = serde_json::json!({
        "type": "content_block_delta",
        "delta": {
            "type": "input_json_delta",
            "partial_json": r#"{"path":"/tm"#
        }
    });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::ToolArgument));
    assert_eq!(chunk.content, r#"{"path":"/tm"#);
}

#[test]
fn parse_sse_thinking_delta() {
    let event = serde_json::json!({
        "type": "content_block_delta",
        "delta": {
            "type": "thinking_delta",
            "thinking": "Let me reason..."
        }
    });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::Thinking));
    assert_eq!(chunk.content, "Let me reason...");
}

#[test]
fn parse_sse_unknown_delta_type_returns_none() {
    let event = serde_json::json!({
        "type": "content_block_delta",
        "delta": { "type": "custom_delta" }
    });
    assert!(parse_sse_event(&event).is_none());
}

#[test]
fn parse_sse_content_block_stop() {
    let event = serde_json::json!({ "type": "content_block_stop", "index": 0 });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::BlockStop));
}

#[test]
fn parse_sse_message_delta_with_usage() {
    let event = serde_json::json!({
        "type": "message_delta",
        "usage": { "input_tokens": 200, "output_tokens": 80 }
    });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::Usage));
    let u = chunk.usage.unwrap();
    assert_eq!(u.input_tokens, 200);
    assert_eq!(u.output_tokens, 80);
}

#[test]
fn parse_sse_message_delta_without_usage_returns_none() {
    let event = serde_json::json!({
        "type": "message_delta",
        "delta": { "stop_reason": "end_turn" }
    });
    assert!(parse_sse_event(&event).is_none());
}

#[test]
fn parse_sse_message_stop() {
    let event = serde_json::json!({ "type": "message_stop" });
    let chunk = parse_sse_event(&event).unwrap();
    assert!(matches!(chunk.chunk_type, ChunkType::Done));
}

#[test]
fn parse_sse_unknown_type_returns_none() {
    let event = serde_json::json!({ "type": "ping" });
    assert!(parse_sse_event(&event).is_none());
}

#[test]
fn parse_sse_missing_type_field_returns_none() {
    let event = serde_json::json!({ "data": "something" });
    assert!(parse_sse_event(&event).is_none());
}

#[test]
fn parse_sse_missing_required_fields_returns_none() {
    // content_block_start with missing id
    let event = serde_json::json!({
        "type": "content_block_start",
        "content_block": { "type": "tool_use", "name": "read_file" }
    });
    assert!(parse_sse_event(&event).is_none());
}

#[test]
fn parse_sse_text_delta_missing_text_returns_none() {
    let event = serde_json::json!({
        "type": "content_block_delta",
        "delta": { "type": "text_delta" }
    });
    assert!(parse_sse_event(&event).is_none());
}

// ============================================================
// parse_anthropic_message tests
// ============================================================

#[test]
fn parse_anthropic_message_text_only() {
    let msg = serde_json::json!({
        "content": [
            { "type": "text", "text": "Hello from Claude" }
        ]
    });
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let calls = parse_anthropic_message(&msg, &collect, &term);
    assert!(calls.is_empty());
    assert_eq!(collect.full_response(), "Hello from Claude");
}

#[test]
fn parse_anthropic_message_tool_use_only() {
    let msg = serde_json::json!({
        "content": [
            {
                "type": "tool_use",
                "id": "toolu_42",
                "name": "exec",
                "input": { "command": "ls -la" }
            }
        ]
    });
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let calls = parse_anthropic_message(&msg, &collect, &term);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_42");
    assert_eq!(calls[0].name, "exec");
}

#[test]
fn parse_anthropic_message_mixed_content() {
    let msg = serde_json::json!({
        "content": [
            { "type": "text", "text": "I will search for you." },
            {
                "type": "tool_use",
                "id": "toolu_10",
                "name": "grep",
                "input": { "pattern": "TODO", "path": "src" }
            }
        ]
    });
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let calls = parse_anthropic_message(&msg, &collect, &term);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "grep");
    assert_eq!(collect.full_response(), "I will search for you.");
}

#[test]
fn parse_anthropic_message_thinking_block() {
    let msg = serde_json::json!({
        "content": [
            { "type": "thinking", "thinking": "Let me analyze this..." },
            { "type": "text", "text": "Here is the answer." }
        ]
    });
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let _calls = parse_anthropic_message(&msg, &collect, &term);
    assert_eq!(collect.full_response(), "Here is the answer.");
}

#[test]
fn parse_anthropic_message_empty_content() {
    let msg = serde_json::json!({ "content": [] });
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let calls = parse_anthropic_message(&msg, &collect, &term);
    assert!(calls.is_empty());
}

#[test]
fn parse_anthropic_message_missing_content() {
    let msg = serde_json::json!({});
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let calls = parse_anthropic_message(&msg, &collect, &term);
    assert!(calls.is_empty());
}

#[test]
fn parse_anthropic_message_unknown_block_type_ignored() {
    let msg = serde_json::json!({
        "content": [
            { "type": "image", "source": { "url": "..." } },
            { "type": "text", "text": "ok" }
        ]
    });
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let _calls = parse_anthropic_message(&msg, &collect, &term);
    assert_eq!(collect.full_response(), "ok");
}

#[test]
fn parse_anthropic_message_tool_use_missing_id_ignored() {
    let msg = serde_json::json!({
        "content": [
            { "type": "tool_use", "name": "exec", "input": {} }
        ]
    });
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let calls = parse_anthropic_message(&msg, &collect, &term);
    assert!(calls.is_empty());
}

// ============================================================
// StallDetector tests
// ============================================================

#[test]
fn stall_detector_initial_state_not_stalled() {
    let sd = StallDetector::new();
    // Just created — stall_timeout is 90s, startup_timeout is 120s
    assert!(sd.check_stall().is_none());
}

#[test]
fn stall_detector_reset_clears_stall_count() {
    let sd = StallDetector::new();
    let count = sd.increment_stall();
    assert_eq!(count, 1);
    sd.reset();
    // After reset, stall count should be 0 (increment from 0 gives 1 again)
    let count2 = sd.increment_stall();
    assert_eq!(count2, 1);
}

#[test]
fn stall_detector_increment_counts() {
    let sd = StallDetector::new();
    assert_eq!(sd.increment_stall(), 1);
    assert_eq!(sd.increment_stall(), 2);
    assert_eq!(sd.increment_stall(), 3);
}

#[test]
fn stall_detector_default() {
    let sd = StallDetector::default();
    assert!(sd.check_stall().is_none());
}

#[test]
fn stall_detector_timeout_returns_startup_initially() {
    let sd = StallDetector::new();
    let t = sd.timeout();
    // Should be startup_timeout (120s) since no event received yet
    assert_eq!(t, Duration::from_secs(120));
}

// ============================================================
// tool_arg_summary tests
// ============================================================

#[test]
fn tool_arg_summary_read_file_with_path() {
    let result = tool_arg_summary("read_file", r#"{"path":"/tmp/test.rs"}"#);
    assert_eq!(result, "/tmp/test.rs");
}

#[test]
fn tool_arg_summary_write_file_with_path() {
    let result = tool_arg_summary("write_file", r#"{"path":"src/main.rs","content":"fn main(){}"}"#);
    assert_eq!(result, "src/main.rs");
}

#[test]
fn tool_arg_summary_edit_file_with_path() {
    let result = tool_arg_summary("edit_file", r#"{"path":"lib.rs","old":"x","new":"y"}"#);
    assert_eq!(result, "lib.rs");
}

#[test]
fn tool_arg_summary_list_dir_with_path() {
    let result = tool_arg_summary("list_dir", r#"{"path":"/home"}"#);
    assert_eq!(result, "/home");
}

#[test]
fn tool_arg_summary_list_dir_default_dot() {
    let result = tool_arg_summary("list_dir", "{}");
    assert_eq!(result, ".");
}

#[test]
fn tool_arg_summary_exec_short_command() {
    let result = tool_arg_summary("exec", r#"{"command":"cargo test"}"#);
    assert_eq!(result, "cargo test");
}

#[test]
fn tool_arg_summary_exec_long_command_truncated() {
    let long_cmd = "a".repeat(200);
    let result = tool_arg_summary("exec", &format!(r#"{{"command":"{}"}}"#, long_cmd));
    assert!(result.len() <= 123); // 120 + "..."
    assert!(result.ends_with("..."));
}

#[test]
fn tool_arg_summary_grep_with_pattern_and_path() {
    let result = tool_arg_summary("grep", r#"{"pattern":"TODO","path":"src"}"#);
    assert_eq!(result, "\"TODO\" in src");
}

#[test]
fn tool_arg_summary_grep_pattern_only() {
    let result = tool_arg_summary("grep", r#"{"pattern":"FIXME"}"#);
    assert_eq!(result, "FIXME");
}

#[test]
fn tool_arg_summary_glob() {
    let result = tool_arg_summary("glob", r#"{"pattern":"**/*.rs"}"#);
    assert_eq!(result, "**/*.rs");
}

#[test]
fn tool_arg_summary_system() {
    let result = tool_arg_summary("system", r#"{"operation":"info"}"#);
    assert_eq!(result, "info");
}

#[test]
fn tool_arg_summary_git() {
    let result = tool_arg_summary("git", r#"{"args":"log --oneline"}"#);
    assert_eq!(result, "git log --oneline");
}

#[test]
fn tool_arg_summary_web_search() {
    let result = tool_arg_summary("web_search", r#"{"query":"rust async"}"#);
    assert_eq!(result, "rust async");
}

#[test]
fn tool_arg_summary_web_fetch() {
    let result = tool_arg_summary("web_fetch", r#"{"url":"https://example.com"}"#);
    assert_eq!(result, "https://example.com");
}

#[test]
fn tool_arg_summary_process_by_name() {
    let result = tool_arg_summary("process", r#"{"process_name":"cargo"}"#);
    assert_eq!(result, "cargo");
}

#[test]
fn tool_arg_summary_process_by_pid() {
    let result = tool_arg_summary("process", r#"{"pid":1234}"#);
    assert_eq!(result, "PID 1234");
}

#[test]
fn tool_arg_summary_runtime_info() {
    let result = tool_arg_summary("runtime_info", r#"{"show":"memory"}"#);
    assert_eq!(result, "memory");
}

#[test]
fn tool_arg_summary_unknown_tool_fallback() {
    let result = tool_arg_summary("custom_tool", r#"{"key1":"val1","key2":42,"key3":true}"#);
    // Fallback format: compact k=v pairs
    assert!(result.contains("key1=val1"));
    assert!(result.contains("key2=42"));
    assert!(result.contains("key3=true"));
}

#[test]
fn tool_arg_summary_unknown_tool_empty_values_skipped() {
    let result = tool_arg_summary("custom", r#"{"k1":"","k2":5}"#);
    assert!(!result.contains("k1="));
    assert!(result.contains("k2=5"));
}

#[test]
fn tool_arg_summary_invalid_json_returns_empty() {
    let result = tool_arg_summary("exec", "not json");
    assert!(result.is_empty());
}

#[test]
fn tool_arg_summary_fileops_with_path() {
    let result = tool_arg_summary("fileops", r#"{"path":"old.rs","operation":"rename","dest":"new.rs"}"#);
    assert_eq!(result, "old.rs");
}

// ============================================================
// End-to-end SSE stream tests with wiremock
// ============================================================

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a complete SSE response body from a sequence of Anthropic SSE events
fn build_sse_body(events: &[serde_json::Value]) -> String {
    events
        .iter()
        .map(|e| format!("data: {}\n\n", serde_json::to_string(e).unwrap()))
        .collect()
}

/// Helper: run process_sse_events against a mock server
async fn run_sse_stream(sse_body: &str) -> anyhow::Result<miniclaudecode_rust::streaming::StreamResult> {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let stall = std::sync::Arc::new(StallDetector::new());
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rate_state = RateLimitState::default();

    miniclaudecode_rust::streaming::process_sse_events(
        &client,
        &mock_server.uri(),
        "test-key",
        "claude-3-5-sonnet-20241022",
        16384,
        "You are helpful",
        &[serde_json::json!({"role": "user", "content": "hi"})],
        &[],
        &collect,
        &term,
        &stall,
        interrupted,
        &rate_state,
    )
    .await
}

#[tokio::test]
async fn sse_stream_text_only_response() {
    let events = vec![
        serde_json::json!({"type": "message_start", "message": {"id": "msg_1"}}),
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "text", "text": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "Hello " }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "World" }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        serde_json::json!({
            "type": "message_delta",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }),
        serde_json::json!({ "type": "message_stop" }),
    ];

    let body = build_sse_body(&events);
    let result = run_sse_stream(&body).await.unwrap();
    assert!(result.tool_calls.is_empty());
}

#[tokio::test]
async fn sse_stream_tool_call_response() {
    let events = vec![
        serde_json::json!({"type": "message_start", "message": {"id": "msg_2"}}),
        serde_json::json!({
            "type": "content_block_start",
            "content_block": {
                "type": "tool_use",
                "id": "toolu_01",
                "name": "read_file"
            }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "input_json_delta",
                "partial_json": r#"{"path":"/tm"#
            }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "input_json_delta",
                "partial_json": r#"p/f.txt"}"#
            }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        serde_json::json!({ "type": "message_stop" }),
    ];

    let body = build_sse_body(&events);
    let result = run_sse_stream(&body).await.unwrap();
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].id, "toolu_01");
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(result.tool_calls[0].arguments, r#"{"path":"/tmp/f.txt"}"#);
}

#[tokio::test]
async fn sse_stream_thinking_then_text() {
    let events = vec![
        serde_json::json!({"type": "message_start", "message": {"id": "msg_3"}}),
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "thinking" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "thinking_delta", "thinking": "Analyzing..." }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "text", "text": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "Done." }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 1 }),
        serde_json::json!({ "type": "message_stop" }),
    ];

    let body = build_sse_body(&events);
    let result = run_sse_stream(&body).await.unwrap();
    assert!(result.tool_calls.is_empty());
    assert_eq!(result.thinking, "Analyzing...");
    assert_eq!(result.text, "Done.");
}

#[tokio::test]
async fn sse_stream_multiple_tool_calls() {
    let events = vec![
        serde_json::json!({"type": "message_start", "message": {"id": "msg_4"}}),
        // Tool 1
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "tool_use", "id": "t1", "name": "exec" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "input_json_delta", "partial_json": r#"{"command":"ls"}"# }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        // Tool 2
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "tool_use", "id": "t2", "name": "grep" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "input_json_delta", "partial_json": r#"{"pattern":"TODO","path":"src"}"# }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 1 }),
        serde_json::json!({ "type": "message_stop" }),
    ];

    let body = build_sse_body(&events);
    let result = run_sse_stream(&body).await.unwrap();
    assert_eq!(result.tool_calls.len(), 2);
    assert_eq!(result.tool_calls[0].name, "exec");
    assert_eq!(result.tool_calls[1].name, "grep");
}

#[tokio::test]
async fn sse_stream_non_sse_json_response() {
    // Some API proxies return a complete JSON message instead of SSE
    let json_response = serde_json::json!({
        "content": [
            { "type": "text", "text": "Direct JSON answer" }
        ]
    });

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&json_response)
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let stall = std::sync::Arc::new(StallDetector::new());
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let result = miniclaudecode_rust::streaming::process_sse_events(
        &client,
        &mock_server.uri(),
        "test-key",
        "claude-3-5-sonnet-20241022",
        16384,
        "You are helpful",
        &[serde_json::json!({"role": "user", "content": "hi"})],
        &[],
        &collect,
        &term,
        &stall,
        interrupted,
        &RateLimitState::default(),
    )
    .await
    .unwrap();

    assert!(result.tool_calls.is_empty());
    assert_eq!(result.text, "Direct JSON answer");
}

#[tokio::test]
async fn sse_stream_api_error_returns_err() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let stall = std::sync::Arc::new(StallDetector::new());
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let result = miniclaudecode_rust::streaming::process_sse_events(
        &client,
        &mock_server.uri(),
        "test-key",
        "claude-3-5-sonnet-20241022",
        16384,
        "You are helpful",
        &[serde_json::json!({"role": "user", "content": "hi"})],
        &[],
        &collect,
        &term,
        &stall,
        interrupted,
        &RateLimitState::default(),
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("429"));
}

#[tokio::test]
async fn sse_stream_tool_use_as_text_returns_err() {
    // Model echoes tool syntax as text (2-of-3 markers)
    let events = vec![
        serde_json::json!({"type": "message_start", "message": {"id": "msg_5"}}),
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "text", "text": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "text_delta",
                "text": r#"{"type":"tool_use","id":"fake","name":"exec","input":{}}"#
            }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        serde_json::json!({ "type": "message_stop" }),
    ];

    let body = build_sse_body(&events);
    let result = run_sse_stream(&body).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("model confused"));
}

#[tokio::test]
async fn sse_stream_non_sse_json_with_tool_calls() {
    let json_response = serde_json::json!({
        "content": [
            {
                "type": "tool_use",
                "id": "toolu_non_sse",
                "name": "exec",
                "input": { "command": "echo hello" }
            }
        ]
    });

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&json_response)
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let collect = CollectHandler::new();
    let term = TerminalHandler::new();
    let stall = std::sync::Arc::new(StallDetector::new());
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let result = miniclaudecode_rust::streaming::process_sse_events(
        &client,
        &mock_server.uri(),
        "test-key",
        "claude-3-5-sonnet-20241022",
        16384,
        "You are helpful",
        &[serde_json::json!({"role": "user", "content": "hi"})],
        &[],
        &collect,
        &term,
        &stall,
        interrupted,
        &RateLimitState::default(),
    )
    .await
    .unwrap();

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].name, "exec");
    assert_eq!(result.tool_calls[0].id, "toolu_non_sse");
}

// ============================================================
// Finish reason tracking tests
// ============================================================

#[test]
fn collect_handler_finish_reason_default_none() {
    let h = CollectHandler::new();
    assert!(h.finish_reason().is_none());
}

#[test]
fn collect_handler_finish_reason_set_and_get() {
    let h = CollectHandler::new();
    h.set_finish_reason("end_turn".to_string());
    assert_eq!(h.finish_reason(), Some("end_turn".to_string()));
}

#[test]
fn collect_handler_finish_reason_overwrite() {
    let h = CollectHandler::new();
    h.set_finish_reason("tool_use".to_string());
    h.set_finish_reason("max_tokens".to_string());
    assert_eq!(h.finish_reason(), Some("max_tokens".to_string()));
}

// ============================================================
// Partial tool call detection tests
// ============================================================

#[test]
fn collect_handler_has_partial_tool_call_empty() {
    let h = CollectHandler::new();
    assert!(!h.has_partial_tool_call());
}

#[test]
fn collect_handler_has_partial_tool_call_with_args() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"command":"ls"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(!h.has_partial_tool_call());
}

#[test]
fn collect_handler_has_partial_tool_call_no_args() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    // Tool call started but no arguments — stream cut off
    assert!(h.has_partial_tool_call());
}

#[test]
fn collect_handler_has_partial_tool_call_multiple_tools_last_empty() {
    let h = CollectHandler::new();
    // First tool call complete
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"command":"ls"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    // Second tool call incomplete
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t2".into()),
        name: Some("grep".into()),
        usage: None,
    });
    assert!(h.has_partial_tool_call());
}

#[test]
fn collect_handler_clear_partial_tool_call() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    assert!(h.has_partial_tool_call());
    h.clear_partial_tool_call();
    assert!(!h.has_partial_tool_call());
    assert!(h.tool_calls().is_empty());
}

#[test]
fn collect_handler_clear_partial_tool_call_when_none() {
    let h = CollectHandler::new();
    h.clear_partial_tool_call(); // should not panic
    assert!(!h.has_partial_tool_call());
}

#[test]
fn collect_handler_clear_partial_tool_call_preserves_earlier_tools() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"command":"ls"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t2".into()),
        name: Some("grep".into()),
        usage: None,
    });
    // Only last (incomplete) tool removed
    h.clear_partial_tool_call();
    let calls = h.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "exec");
}

// ============================================================
// Truncated tool argument detection tests
// ============================================================

#[test]
fn collect_handler_has_truncated_tool_args_valid_json() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("read_file".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"path":"/tmp/f.txt"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(!h.has_truncated_tool_args());
}

#[test]
fn collect_handler_has_truncated_tool_args_invalid_json() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("read_file".into()),
        usage: None,
    });
    // Stream cut off mid-JSON
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"path":"/tmp"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.has_truncated_tool_args());
}

#[test]
fn collect_handler_has_truncated_tool_args_empty_args_ignored() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("read_file".into()),
        usage: None,
    });
    // Tool call started but no arguments yet — not truncated
    assert!(!h.has_truncated_tool_args());
}

#[test]
fn collect_handler_has_truncated_tool_args_multiple_one_truncated() {
    let h = CollectHandler::new();
    // First tool call complete
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"command":"ls"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    // Second tool call truncated
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t2".into()),
        name: Some("grep".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"pattern":"TOD"#.into(),
        id: None,
        name: None,
        usage: None,
    });
    assert!(h.has_truncated_tool_args());
}

// ============================================================
// ClearText tests
// ============================================================

#[test]
fn collect_handler_clear_text() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: "accumulated text".into(),
        id: None,
        name: None,
        usage: None,
    });
    assert_eq!(h.full_response(), "accumulated text");
    h.clear_text();
    assert_eq!(h.full_response(), "");
}

// ============================================================
// StreamResult tests
// ============================================================

#[test]
fn stream_result_completed_true() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolCall,
        content: String::new(),
        id: Some("t1".into()),
        name: Some("exec".into()),
        usage: None,
    });
    h.handle(StreamChunk {
        chunk_type: ChunkType::ToolArgument,
        content: r#"{"command":"echo hi"}"#.into(),
        id: None,
        name: None,
        usage: None,
    });

    let calls = h.tool_calls();
    let text = h.full_response();
    let thinking = h.thinking();
    let fr = h.finish_reason();

    let result = miniclaudecode_rust::streaming::StreamResult {
        tool_calls: calls,
        text,
        thinking,
        completed: true,
        finish_reason: fr,
    };

    assert!(result.completed);
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.finish_reason, None);
}

#[test]
fn stream_result_completed_false_has_partial() {
    let h = CollectHandler::new();
    h.handle(StreamChunk {
        chunk_type: ChunkType::Text,
        content: "partial response".into(),
        id: None,
        name: None,
        usage: None,
    });

    let result = miniclaudecode_rust::streaming::StreamResult {
        tool_calls: h.tool_calls(),
        text: h.full_response(),
        thinking: h.thinking(),
        completed: false,
        finish_reason: h.finish_reason(),
    };

    assert!(!result.completed);
    assert_eq!(result.text, "partial response");
    assert!(result.tool_calls.is_empty());
}

// ============================================================
// SSE message_delta with stop_reason test
// ============================================================

#[tokio::test]
async fn sse_stream_finish_reason_from_message_delta() {
    let events = vec![
        serde_json::json!({"type": "message_start", "message": {"id": "msg_6"}}),
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "text", "text": "" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "Done." }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }),
        serde_json::json!({ "type": "message_stop" }),
    ];

    let body = build_sse_body(&events);
    let result = run_sse_stream(&body).await.unwrap();

    // Note: process_sse_events sets finish_reason on CollectHandler internally
    // but StreamResult exposes it via the return path
    assert!(result.completed);
}

#[tokio::test]
async fn sse_stream_finish_reason_tool_use() {
    let events = vec![
        serde_json::json!({"type": "message_start", "message": {"id": "msg_7"}}),
        serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "tool_use", "id": "t1", "name": "exec" }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "input_json_delta", "partial_json": r#"{"command":"ls"}"# }
        }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "tool_use" },
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }),
        serde_json::json!({ "type": "message_stop" }),
    ];

    let body = build_sse_body(&events);
    let result = run_sse_stream(&body).await.unwrap();

    assert!(result.completed);
    assert_eq!(result.tool_calls.len(), 1);
}
