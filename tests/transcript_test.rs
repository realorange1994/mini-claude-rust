//! Integration tests for transcript module

use miniclaudecode_rust::transcript::{Transcript, TranscriptEntry, ToolCall};
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
    t.add_assistant("Hi there!".to_string(), vec![]).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].content, "Hello");
    assert_eq!(entries[1].role, "assistant");
    assert_eq!(entries[1].content, "Hi there!");
}

#[test]
fn transcript_add_user() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_user("Test message".to_string()).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].content, "Test message");
    assert!(entries[0].tool_calls.is_empty());
}

#[test]
fn transcript_add_assistant() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    let tool_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: r#"{"path": "test.txt"}"#.to_string(),
        result: None,
    }];

    t.add_assistant("I'll read the file.".to_string(), tool_calls).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "assistant");
    assert_eq!(entries[0].tool_calls.len(), 1);
    assert_eq!(entries[0].tool_calls[0].id, "call_1");
    assert_eq!(entries[0].tool_calls[0].name, "read_file");
}

#[test]
fn transcript_add_tool_result() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_tool_result(
        "call_1".to_string(),
        "read_file".to_string(),
        r#"{"path": "test.txt"}"#.to_string(),
        "File content here".to_string(),
    ).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "tool");
    assert_eq!(entries[0].content, "File content here");
    assert_eq!(entries[0].tool_calls.len(), 1);
    assert_eq!(entries[0].tool_calls[0].id, "call_1");
    assert_eq!(entries[0].tool_calls[0].name, "read_file");
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
    t.add_assistant("Second".to_string(), vec![]).unwrap();
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

    let entry = TranscriptEntry {
        timestamp: chrono::Utc::now(),
        role: "system".to_string(),
        content: "System prompt".to_string(),
        tool_calls: vec![],
    };

    t.write_entry(&entry).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "system");
}

#[test]
fn transcript_replay() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    t.add_user("Hello".to_string()).unwrap();
    t.add_assistant("Hi".to_string(), vec![]).unwrap();

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
fn transcript_tool_call_with_result() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    let t = Transcript::new(&path);

    let tool_calls = vec![ToolCall {
        id: "call_2".to_string(),
        name: "exec".to_string(),
        arguments: r#"{"command": "ls"}"#.to_string(),
        result: Some("file1.txt\nfile2.txt".to_string()),
    }];

    t.add_assistant("Running ls".to_string(), tool_calls).unwrap();

    let entries = t.read_all().unwrap();
    assert_eq!(entries[0].tool_calls[0].result, Some("file1.txt\nfile2.txt".to_string()));
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
