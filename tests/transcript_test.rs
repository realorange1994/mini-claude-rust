//! Integration tests for transcript module (Go format compatible)

use miniclaudecode_rust::transcript::{Transcript, Entry, TYPE_USER, TYPE_ASSISTANT, TYPE_TOOL_USE, TYPE_TOOL_RESULT, TYPE_SYSTEM};
use std::path::PathBuf;
use tempfile::TempDir;

// ─── Transcript creation ───

#[test]
fn transcript_new() {
    let path = PathBuf::from("/tmp/test.jsonl");
    let t = Transcript::new(&path);
    // Should not panic
    drop(t);
}

#[test]
fn transcript_default() {
    let t = Transcript::default();
    let entries = t.read_all().unwrap();
    assert!(entries.is_empty());
}

// ─── Write and read entries ───

#[test]
fn transcript_write_and_read() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_user("Hello".to_string()).unwrap();
    t.add_assistant("Hi there!".to_string(), Some("M2.7".to_string())).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].type_, TYPE_USER);
    assert_eq!(entries[0].content, "Hello");
    assert_eq!(entries[1].type_, TYPE_ASSISTANT);
    assert_eq!(entries[1].content, "Hi there!");
    assert_eq!(entries[1].model, Some("M2.7".to_string()));
}

#[test]
fn transcript_add_user() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_user("Test message".to_string()).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].type_, TYPE_USER);
    assert_eq!(entries[0].content, "Test message");
    assert!(entries[0].tool_name.is_none());
}

#[test]
fn transcript_add_assistant() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_assistant("I'll help you.".to_string(), Some("M2.7".to_string())).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].type_, TYPE_ASSISTANT);
    assert_eq!(entries[0].content, "I'll help you.");
    assert_eq!(entries[0].model, Some("M2.7".to_string()));
}

#[test]
fn transcript_add_tool_use() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    let mut args = std::collections::HashMap::new();
    args.insert("path".to_string(), serde_json::json!("test.txt"));
    t.add_tool_use("call_1".to_string(), "read_file".to_string(), args).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].type_, TYPE_TOOL_USE);
    assert_eq!(entries[0].tool_name, Some("read_file".to_string()));
    assert_eq!(entries[0].tool_id, Some("call_1".to_string()));
    assert!(entries[0].tool_args.is_some());
    // tool_use should NOT have content
    assert!(entries[0].content.is_empty());
}

#[test]
fn transcript_add_tool_result() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_tool_result(
        "call_1".to_string(),
        "read_file".to_string(),
        "File content here".to_string(),
    ).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].type_, TYPE_TOOL_RESULT);
    assert_eq!(entries[0].content, "File content here");
    assert_eq!(entries[0].tool_id, Some("call_1".to_string()));
    assert_eq!(entries[0].tool_name, Some("read_file".to_string()));
}

#[test]
fn transcript_add_system() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_system("model=M2.7, mode=auto".to_string()).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].type_, TYPE_SYSTEM);
    assert_eq!(entries[0].content, "model=M2.7, mode=auto");
}

#[test]
fn transcript_read_nonexistent_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nonexistent.jsonl");
    let t = Transcript::new(&path);

    let entries = t.read_all().unwrap();
    assert!(entries.is_empty());
}

#[test]
fn transcript_multiple_writes_append() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_user("First".to_string()).unwrap();
    t.add_assistant("Second".to_string(), None).unwrap();
    t.add_user("Third".to_string()).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].content, "First");
    assert_eq!(entries[1].content, "Second");
    assert_eq!(entries[2].content, "Third");
}

#[test]
fn transcript_write_entry_directly() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    let entry = Entry::system("System prompt".to_string());
    t.write_entry(&entry).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].type_, TYPE_SYSTEM);
}

#[test]
fn transcript_replay() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_user("Hello".to_string()).unwrap();
    t.add_assistant("Hi".to_string(), None).unwrap();

    let mut count = 0;
    t.replay(|_entry| { count += 1 }).unwrap();
    assert_eq!(count, 2);
}

#[test]
fn transcript_replay_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.jsonl");
    let t = Transcript::new(&path);

    let mut count = 0;
    t.replay(|_| { count += 1 }).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn transcript_timestamp() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    let before = chrono::Utc::now();
    t.add_user("Test".to_string()).unwrap();

    let entries = t.read_all().unwrap();
    assert!(entries[0].timestamp >= before);
}

// ─── Go format compatibility tests ───

#[test]
fn transcript_parse_go_format_user() {
    // Go format: {"type":"user","content":"Hello","timestamp":"..."}
    let json = r#"{"type":"user","content":"Hello from Go","timestamp":"2026-04-11T17:53:01.663238+08:00"}"#;
    let entry: Entry = serde_json::from_str(json).unwrap();

    assert_eq!(entry.type_, TYPE_USER);
    assert_eq!(entry.content, "Hello from Go");
}

#[test]
fn transcript_parse_go_format_tool_use() {
    // Go format: {"type":"tool_use","tool_name":"read_file","tool_id":"toolu_123","tool_args":{"path":"test.txt"},"timestamp":"..."}
    let json = r#"{"type":"tool_use","tool_name":"read_file","tool_id":"toolu_123","tool_args":{"path":"test.txt"},"timestamp":"2026-04-11T17:53:01.663238+08:00"}"#;
    let entry: Entry = serde_json::from_str(json).unwrap();

    assert_eq!(entry.type_, TYPE_TOOL_USE);
    assert!(entry.is_tool_use());
    assert_eq!(entry.tool_name, Some("read_file".to_string()));
    assert_eq!(entry.tool_id, Some("toolu_123".to_string()));
    // tool_use has no content
    assert!(entry.content.is_empty());
}

#[test]
fn transcript_parse_go_format_tool_result() {
    // Go format: {"type":"tool_result","content":"File content","tool_name":"read_file","tool_id":"toolu_123","timestamp":"..."}
    let json = r#"{"type":"tool_result","content":"File content here","tool_name":"read_file","tool_id":"toolu_123","timestamp":"2026-04-11T17:53:01.663238+08:00"}"#;
    let entry: Entry = serde_json::from_str(json).unwrap();

    assert_eq!(entry.type_, TYPE_TOOL_RESULT);
    assert!(entry.is_tool_result());
    assert_eq!(entry.content, "File content here");
    assert_eq!(entry.tool_id, Some("toolu_123".to_string()));
}

#[test]
fn transcript_parse_go_format_assistant() {
    // Go format: {"type":"assistant","content":"Hello","model":"M2.7","timestamp":"..."}
    let json = r#"{"type":"assistant","content":"I will help you","model":"M2.7","timestamp":"2026-04-11T17:53:01.663238+08:00"}"#;
    let entry: Entry = serde_json::from_str(json).unwrap();

    assert_eq!(entry.type_, TYPE_ASSISTANT);
    assert_eq!(entry.content, "I will help you");
    assert_eq!(entry.model, Some("M2.7".to_string()));
}

#[test]
fn transcript_parse_go_format_system() {
    // Go format: {"type":"system","content":"model=M2.7, mode=auto","timestamp":"..."}
    let json = r#"{"type":"system","content":"model=M2.7, mode=auto","timestamp":"2026-04-11T17:53:01.663238+08:00"}"#;
    let entry: Entry = serde_json::from_str(json).unwrap();

    assert_eq!(entry.type_, TYPE_SYSTEM);
    assert_eq!(entry.content, "model=M2.7, mode=auto");
}

// ─── Output format tests (verify Go format output) ───

#[test]
fn transcript_output_go_format_user() {
    let entry = Entry::user("Hello".to_string());
    let json = serde_json::to_string(&entry).unwrap();

    // Should have "type":"user" and "content":"Hello"
    assert!(json.contains(r#""type":"user""#));
    assert!(json.contains(r#""content":"Hello""#));
    assert!(json.contains(r#""timestamp""#));
    // Should NOT have optional fields that are empty
    assert!(!json.contains(r#""tool_name""#));
    assert!(!json.contains(r#""tool_id""#));
    assert!(!json.contains(r#""model""#));
}

#[test]
fn transcript_output_go_format_tool_use() {
    let mut args = std::collections::HashMap::new();
    args.insert("path".to_string(), serde_json::json!("test.txt"));
    let entry = Entry::tool_use("call_1".to_string(), "read_file".to_string(), args);
    let json = serde_json::to_string(&entry).unwrap();

    // Should have type, tool_name, tool_id, tool_args
    assert!(json.contains(r#""type":"tool_use""#));
    assert!(json.contains(r#""tool_name":"read_file""#));
    assert!(json.contains(r#""tool_id":"call_1""#));
    assert!(json.contains(r#""tool_args""#));
    // Should NOT have content (it's empty for tool_use)
    assert!(!json.contains(r#""content""#));
}

#[test]
fn transcript_output_go_format_tool_result() {
    let entry = Entry::tool_result("call_1".to_string(), "read_file".to_string(), "File content".to_string());
    let json = serde_json::to_string(&entry).unwrap();

    // Should have type, content, tool_name, tool_id
    assert!(json.contains(r#""type":"tool_result""#));
    assert!(json.contains(r#""content":"File content""#));
    assert!(json.contains(r#""tool_name":"read_file""#));
    assert!(json.contains(r#""tool_id":"call_1""#));
    // Should NOT have tool_args (it's None for tool_result)
    assert!(!json.contains(r#""tool_args""#));
}
