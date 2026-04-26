//! Integration tests for file history tools layer + core API gaps
//!
//! Existing tests (filehistory_test.rs) cover FileHistory direct calls only.
//! This file adds:
//! - Core API gaps (persistence, tombstones, checkout, annotate, diff edge cases, etc.)
//! - Tool layer tests (all 13 tools via execute())

use miniclaudecode_rust::filehistory::FileHistory;
use miniclaudecode_rust::tools::file_history_tools::*;
use miniclaudecode_rust::tools::Tool;
use std::collections::HashMap;
use std::sync::Arc;
use std::fs;
use tempfile::TempDir;
use serde_json::json;

// ─── Helper: build params HashMap ───
fn params(pairs: Vec<(&str, serde_json::Value)>) -> HashMap<String, serde_json::Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

// ═══════════════════════════════════════════════════════════
// Part 1: Core API gaps (FileHistory direct calls)
// ═══════════════════════════════════════════════════════════

// ─── Disk persistence ───

#[test]
fn core_disk_persistence() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("persist.txt");
    fs::write(&file, "v1").unwrap();

    // First instance: snapshot
    let fh1 = FileHistory::new_with_dir(dir.path());
    fh1.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh1.snapshot(&file).unwrap();

    // Create new instance pointing to same directory
    let fh2 = FileHistory::new_with_dir(dir.path());
    assert_eq!(fh2.count(&file), 2, "History should survive reload");

    let snapshots = fh2.get_snapshots(&file);
    assert_eq!(snapshots[0].content, "v1");
    assert_eq!(snapshots[1].content, "v2");
}

// ─── Tombstone (deleted snapshots) ───

#[test]
fn core_tombstone_behavior() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tomb.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    assert_eq!(fh.count(&file), 3);

    // Snapshots should not be tombstoned by default
    let snapshots = fh.get_snapshots(&file);
    assert_eq!(snapshots.len(), 3);
    for snap in &snapshots {
        assert!(!snap.deleted, "Snapshots should not be tombstoned by default");
    }
}

// ─── checkout() method ───

#[test]
fn core_checkout_writes_and_snapshots() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("checkout.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    assert_eq!(fh.count(&file), 3);

    // Checkout v1
    let content = fh.checkout(&file, 1).unwrap();
    assert!(content.is_some());
    assert_eq!(content.unwrap(), "v1");

    // File on disk should be v1
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1");

    // New snapshot should have been created (for redo)
    assert_eq!(fh.count(&file), 4);
}

#[test]
fn core_checkout_current_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("checkout_cur.txt");
    fs::write(&file, "only").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Checkout the only (current) version
    let content = fh.checkout(&file, 1).unwrap();
    assert!(content.is_some());
    assert_eq!(content.unwrap(), "only");
    assert_eq!(fs::read_to_string(&file).unwrap(), "only");
}

#[test]
fn core_checkout_invalid_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("checkout_inv.txt");
    fs::write(&file, "content").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Version 99 doesn't exist
    let result = fh.checkout(&file, 99).unwrap();
    assert!(result.is_none());
}

// ─── annotate_snapshot with version specifiers ───

#[test]
fn core_annotate_multiple_versions() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("annotate.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    // Annotate v1
    assert!(fh.annotate_snapshot(&file, 1, "first annotation"));

    // Annotate v2
    assert!(fh.annotate_snapshot(&file, 2, "second annotation"));

    let snapshots = fh.get_snapshots(&file);
    assert!(snapshots[0].description.contains("first annotation"));
    assert!(snapshots[1].description.contains("second annotation"));
}

#[test]
fn core_annotate_appends_with_pipe() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("annotate_pipe.txt");
    fs::write(&file, "content").unwrap();

    let fh = FileHistory::new();
    fh.snapshot_with_desc(&file, "original desc".to_string()).unwrap();

    // First annotation
    fh.annotate_snapshot(&file, 1, "note1");

    // Second annotation should append with |
    fh.annotate_snapshot(&file, 1, "note2");

    let snapshots = fh.get_snapshots(&file);
    let desc = &snapshots[0].description;
    assert!(desc.contains("note1"));
    assert!(desc.contains("note2"));
    assert!(desc.contains(" | "), "Annotations should be separated by |");
}

#[test]
fn core_annotate_empty_message() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("annotate_empty.txt");
    fs::write(&file, "content").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Empty message should return false
    let result = fh.annotate_snapshot(&file, 1, "");
    assert!(!result);
}

// ─── diff() edge cases ───

#[test]
fn core_diff_identical_files() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_same.txt");
    fs::write(&file, "same content").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Same content snapshot won't be created, so we need to manually
    // use diff on same version numbers
    // v1 -> v1 should produce empty hunks
    let diff = fh.diff(&file, 1, 1);
    assert!(diff.is_some());
    let diff = diff.unwrap();
    assert!(diff.hunks.is_empty(), "Same version diff should have no hunks");
}

#[test]
fn core_diff_completely_different() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_total.txt");
    fs::write(&file, "line A\nline B\nline C").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "line X\nline Y\nline Z").unwrap();
    fh.snapshot(&file).unwrap();

    let diff = fh.diff(&file, 1, 2).expect("diff should exist");
    // All lines removed and all lines added
    let all_lines: Vec<&str> = diff.hunks.iter()
        .flat_map(|h| h.lines.iter())
        .map(|s| s.as_str())
        .collect();

    let has_removals = all_lines.iter().any(|l| l.starts_with("- "));
    let has_additions = all_lines.iter().any(|l| l.starts_with("+ "));
    assert!(has_removals, "Should have removals");
    assert!(has_additions, "Should have additions");
}

#[test]
fn core_diff_hunk_splitting_distant_changes() {
    // Create a file with changes at line 1 and line 50 → should produce 2 separate hunks
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_split.txt");

    // Build a 60-line file
    let mut lines: Vec<String> = (1..=60).map(|i| format!("line {}", i)).collect();
    fs::write(&file, lines.join("\n")).unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Change line 1 and line 50
    lines[0] = "CHANGED LINE 1".to_string();
    lines[49] = "CHANGED LINE 50".to_string();
    fs::write(&file, lines.join("\n")).unwrap();
    fh.snapshot(&file).unwrap();

    let diff = fh.diff(&file, 1, 2).expect("diff should exist");
    assert!(diff.hunks.len() >= 2,
        "Distant changes should produce separate hunks, got {} hunks", diff.hunks.len());
}

#[test]
fn core_diff_hunk_merging_nearby_changes() {
    // Changes at line 1 and line 3 should produce 1 merged hunk
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_merge.txt");

    let mut lines: Vec<String> = (1..=10).map(|i| format!("line {}", i)).collect();
    fs::write(&file, lines.join("\n")).unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Change line 1 and line 3
    lines[0] = "CHANGED 1".to_string();
    lines[2] = "CHANGED 3".to_string();
    fs::write(&file, lines.join("\n")).unwrap();
    fh.snapshot(&file).unwrap();

    let diff = fh.diff(&file, 1, 2).expect("diff should exist");
    assert_eq!(diff.hunks.len(), 1,
        "Nearby changes should produce a single merged hunk, got {} hunks", diff.hunks.len());
}

// ─── resolve_version with tags ───

#[test]
fn core_resolve_version_with_tags() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("resolve_tag.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Add tag "stable" to v1 (it's the latest at this point)
    fh.add_tag(&file, "stable");

    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    // Resolve "stable" should return version 1 (tagged on v1 before v2 was created)
    let resolved = fh.resolve_version(&file, "stable");
    assert_eq!(resolved, Some(1), "Tag 'stable' should resolve to v1");
}

// ─── clear_under_dir ───

#[test]
fn core_clear_under_dir() {
    let dir = TempDir::new().unwrap();
    let sub1 = dir.path().join("sub1");
    let sub2 = dir.path().join("sub2");
    fs::create_dir_all(&sub1).unwrap();
    fs::create_dir_all(&sub2).unwrap();

    let file1 = sub1.join("a.txt");
    let file2 = sub2.join("b.txt");
    fs::write(&file1, "content1").unwrap();
    fs::write(&file2, "content2").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file1).unwrap();
    fh.snapshot(&file2).unwrap();

    assert_eq!(fh.count(&file1), 1);
    assert_eq!(fh.count(&file2), 1);

    // Clear only under sub1
    fh.clear_under_dir(&sub1);

    assert_eq!(fh.count(&file1), 0, "History for sub1 file should be cleared");
    assert_eq!(fh.count(&file2), 1, "History for sub2 file should remain");
}

// ─── snapshot_current_with_desc ───

#[test]
fn core_snapshot_current_with_desc() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("snap_desc.txt");
    fs::write(&file, "new file content").unwrap();

    let fh = FileHistory::new();
    let result = fh.snapshot_current_with_desc(&file, "initial creation".to_string()).unwrap();
    assert!(result.is_some(), "snapshot_current_with_desc should create entry for new file");

    let snapshots = fh.get_snapshots(&file);
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].description.contains("initial creation"));
    assert_eq!(snapshots[0].content, "new file content");
}

// ═══════════════════════════════════════════════════════════
// Part 2: Tool layer tests (execute() with params HashMap)
// ═══════════════════════════════════════════════════════════

// ─── 2a. FileHistoryTool ───

#[test]
fn tool_history_path_with_history() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("hist.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(!result.is_error, "Should show history");
    assert!(result.output.contains("2 versions"));
}

#[test]
fn tool_history_path_no_history() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("nohist.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(result.output.contains("No history"));
}

#[test]
fn tool_history_pattern_filter() {
    let dir = TempDir::new().unwrap();
    let rs_file = dir.path().join("test.rs");
    let py_file = dir.path().join("test.py");
    fs::write(&rs_file, "fn main() {}").unwrap();
    fs::write(&py_file, "print('hi')").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&rs_file).unwrap();
    fh.snapshot(&py_file).unwrap();

    let tool = FileHistoryTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("*.rs")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("test.rs"));
    assert!(!result.output.contains("test.py"));
}

#[test]
fn tool_history_no_history_at_all() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(!result.is_error);
    // Should handle empty history gracefully
}

#[test]
fn tool_history_pagination() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("paginate.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    for i in 1..=5 {
        if i > 1 {
            fs::write(&file, format!("v{}", i)).unwrap();
        }
        fh.snapshot(&file).unwrap();
    }

    let tool = FileHistoryTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("limit", json!(2)),
        ("offset", json!(0)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("showing 1-2"));
}

// ─── 2b. FileHistoryReadTool ───

#[test]
fn tool_read_valid_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("read.txt");
    fs::write(&file, "line1\nline2\nline3").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "modified").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryReadTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!(1)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("line1"));
}

#[test]
fn tool_read_invalid_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("read_inv.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryReadTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!(99)),
    ]));
    assert!(result.is_error);
    assert!(result.output.contains("Invalid version"));
}

#[test]
fn tool_read_missing_path() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryReadTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(result.is_error);
    assert!(result.output.contains("path is required"));
}

// ─── 2c. FileHistoryGrepTool ───

#[test]
fn tool_grep_pattern_match() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("grep.txt");
    fs::write(&file, "hello world\nfoo bar\nhello again").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryGrepTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("hello")),
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("hello"));
}

#[test]
fn tool_grep_no_match() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("grep_nomatch.txt");
    fs::write(&file, "hello world").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryGrepTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("nonexistent_xyz")),
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("No matches") || result.output.contains("0 matches"));
}

#[test]
fn tool_grep_invalid_regex() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("grep_bad.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryGrepTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("***invalid***")),
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(result.is_error);
    assert!(result.output.contains("Invalid regex") || result.output.contains("regex"));
}

#[test]
fn tool_grep_missing_pattern() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryGrepTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(result.is_error);
    assert!(result.output.contains("pattern is required"));
}

// ─── 2d. FileRestoreTool ───

#[test]
fn tool_restore_success() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("restore.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileRestoreTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("v1"));
}

#[test]
fn tool_restore_no_history() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("restore_none.txt");
    fs::write(&file, "only").unwrap();

    let fh = Arc::new(FileHistory::new());
    let tool = FileRestoreTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    // Should error or indicate no history
    assert!(result.is_error || result.output.contains("No history") || result.output.contains("No previous"));
}

#[test]
fn tool_restore_missing_path() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileRestoreTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(result.is_error);
}

// ─── 2e. FileRewindTool ───

#[test]
fn tool_rewind_success() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("rewind.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileRewindTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("steps", json!(2)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("v1"));
}

#[test]
fn tool_rewind_too_many_steps() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("rewind_deep.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileRewindTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("steps", json!(5)),
    ]));
    assert!(result.is_error);
}

#[test]
fn tool_rewind_missing_params() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileRewindTool::new(fh);

    // Missing path
    let result = tool.execute(params(vec![("steps", json!(1))]));
    assert!(result.is_error);

    // Missing steps
    let result = tool.execute(params(vec![("path", json!("/some/path.txt"))]));
    assert!(result.is_error);
}

// ─── 2f. FileHistoryDiffTool ───

#[test]
fn tool_diff_v1_to_v2() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_tool.txt");
    fs::write(&file, "original").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "modified content").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v1")),
        ("to", json!("v2")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("+") || result.output.contains("-"));
}

#[test]
fn tool_diff_stat_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_stat.txt");
    fs::write(&file, "a\nb\nc").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "a\nb\nc\nd\ne").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v1")),
        ("to", json!("v2")),
        ("mode", json!("stat")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("+"));
}

#[test]
fn tool_diff_invalid_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_bad.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v99")),
    ]));
    assert!(result.is_error);
    assert!(result.output.contains("Cannot resolve") || result.output.contains("version"));
}

#[test]
fn tool_diff_chain_with_to2() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_chain.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    for i in 1..=3 {
        if i > 1 {
            fs::write(&file, format!("v{}", i)).unwrap();
        }
        fh.snapshot(&file).unwrap();
    }

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v1")),
        ("to", json!("v2")),
        ("to2", json!("v3")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("Chain diff") || result.output.contains("v1") && result.output.contains("v3"));
}

#[test]
fn tool_diff_same_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_same.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "changed").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v1")),
        ("to", json!("v1")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("same version") || result.output.contains("No differences"));
}

#[test]
fn tool_diff_no_history() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_nohist.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(result.is_error);
    assert!(result.output.contains("No history"));
}

// ─── 2g. FileHistorySearchTool ───

#[test]
fn tool_search_added() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("search_add.txt");
    fs::write(&file, "hello").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "hello world new text").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistorySearchTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("query", json!("world")),
        ("mode", json!("added")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("world"));
}

#[test]
fn tool_search_removed() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("search_rm.txt");
    fs::write(&file, "hello world goodbye").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "hello goodbye").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistorySearchTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("query", json!("world")),
        ("mode", json!("removed")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("world"));
}

#[test]
fn tool_search_no_results() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("search_none.txt");
    fs::write(&file, "hello").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "hello world").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistorySearchTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("query", json!("zzz_not_found_zzz")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("No versions where") || result.output.contains("No results") || result.output.contains("not found") || result.output.contains("No occurrences"));
}

#[test]
fn tool_search_missing_params() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistorySearchTool::new(fh);

    // Missing path
    let result = tool.execute(params(vec![("query", json!("test"))]));
    assert!(result.is_error);

    // Missing query
    let result = tool.execute(params(vec![("path", json!("/path"))]));
    assert!(result.is_error);
}

// ─── 2h. FileHistorySummaryTool ───

#[test]
fn tool_summary_basic() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("summary.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistorySummaryTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(!result.is_error);
    assert!(result.output.contains("summary") || result.output.contains("Summary"));
}

#[test]
fn tool_summary_with_since() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("summary_since.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistorySummaryTool::new(fh);
    let result = tool.execute(params(vec![
        ("since", json!("1h")),
    ]));
    assert!(!result.is_error);
}

// ─── 2i. FileHistoryTimelineTool ───

#[test]
fn tool_timeline_basic() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("timeline.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryTimelineTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(!result.is_error);
}

#[test]
fn tool_timeline_with_since() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("timeline_since.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryTimelineTool::new(fh);
    let result = tool.execute(params(vec![
        ("since", json!("30m")),
    ]));
    assert!(!result.is_error);
}

#[test]
fn tool_timeline_limit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("timeline_lim.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    for i in 1..=5 {
        if i > 1 {
            fs::write(&file, format!("v{}", i)).unwrap();
        }
        fh.snapshot(&file).unwrap();
    }

    let tool = FileHistoryTimelineTool::new(fh);
    let result = tool.execute(params(vec![
        ("limit", json!(2)),
    ]));
    assert!(!result.is_error);
}

#[test]
fn tool_timeline_empty() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryTimelineTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(!result.is_error);
    assert!(result.output.contains("No changes") || result.output.contains("No timeline"));
}

// ─── 2j. FileHistoryTagTool ───

#[test]
fn tool_tag_add() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tag_add.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryTagTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("tag", json!("my-tag")),
        ("action", json!("add")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("Tag") || result.output.contains("added"));
}

#[test]
fn tool_tag_list() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tag_list.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fh.add_tag(&file, "alpha");
    fh.add_tag(&file, "beta");

    let tool = FileHistoryTagTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("action", json!("list")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("alpha"));
    assert!(result.output.contains("beta"));
}

#[test]
fn tool_tag_delete() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tag_del.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fh.add_tag(&file, "to-delete");

    let tool = FileHistoryTagTool::new(fh.clone());
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("tag", json!("to-delete")),
        ("action", json!("delete")),
        ("version", json!(1)),
    ]));
    assert!(!result.is_error);

    // Verify tag is gone
    let tags = fh.list_tags(&file);
    assert!(!tags.iter().any(|(_, t)| t == "to-delete"));
}

#[test]
fn tool_tag_search() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tag_search.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fh.add_tag(&file, "shared-tag");

    let tool = FileHistoryTagTool::new(fh);
    let result = tool.execute(params(vec![
        ("tag", json!("shared-tag")),
        ("action", json!("search")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("shared-tag"));
}

#[test]
fn tool_tag_missing_path_for_add() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryTagTool::new(fh);
    let result = tool.execute(params(vec![
        ("tag", json!("test")),
        ("action", json!("add")),
    ]));
    assert!(result.is_error);
}

#[test]
fn tool_tag_missing_version_for_delete() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tag_del_miss.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryTagTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("tag", json!("test")),
        ("action", json!("delete")),
    ]));
    assert!(result.is_error);
}

#[test]
fn tool_tag_missing_tag_for_search() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryTagTool::new(fh);
    let result = tool.execute(params(vec![
        ("action", json!("search")),
    ]));
    assert!(result.is_error);
}

// ─── 2k. FileHistoryAnnotateTool ───

#[test]
fn tool_annotate_v1() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ann_v1.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryAnnotateTool::new(fh.clone());
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("v1")),
        ("message", json!("annotating v1")),
    ]));
    assert!(!result.is_error);
    let snapshots = fh.get_snapshots(&file);
    assert!(snapshots[0].description.contains("annotating v1"));
}

#[test]
fn tool_annotate_current() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ann_cur.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "changed").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryAnnotateTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("current")),
        ("message", json!("latest annotation")),
    ]));
    assert!(!result.is_error);
}

#[test]
fn tool_annotate_invalid_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ann_inv.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryAnnotateTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("v99")),
        ("message", json!("should fail")),
    ]));
    assert!(result.is_error);
}

#[test]
fn tool_annotate_empty_message() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ann_empty.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryAnnotateTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("v1")),
        ("message", json!("")),
    ]));
    // The tool passes empty message through; annotate_snapshot returns false for empty
    // The tool should handle this gracefully (either error or indicate no change)
    assert!(result.is_error || result.output.contains("empty") || result.output.contains("no change"));
}

// ─── 2l. FileHistoryBatchTool ───

#[test]
fn tool_batch_list() {
    let dir = TempDir::new().unwrap();
    let rs1 = dir.path().join("one.rs");
    let rs2 = dir.path().join("two.rs");
    fs::write(&rs1, "fn a() {}").unwrap();
    fs::write(&rs2, "fn b() {}").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&rs1).unwrap();
    fh.snapshot(&rs2).unwrap();

    let tool = FileHistoryBatchTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("*.rs")),
        ("action", json!("list")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("one.rs") || result.output.contains("two.rs"));
}

#[test]
fn tool_batch_read() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("batch_read.rs");
    fs::write(&file, "fn main() {}").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "fn main() { println!(\"hi\"); }").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryBatchTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("*.rs")),
        ("action", json!("read")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("main"));
}

#[test]
fn tool_batch_diff() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("batch_diff.rs");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2 with more content").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryBatchTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("*.rs")),
        ("action", json!("diff")),
    ]));
    assert!(!result.is_error);
}

#[test]
fn tool_batch_no_matching_files() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryBatchTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("*.xyz")),
        ("action", json!("list")),
    ]));
    assert!(result.output.contains("No files with history match pattern"));
}

// ─── 2m. FileHistoryCheckoutTool ───

#[test]
fn tool_checkout_success() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("co_success.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryCheckoutTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("v1")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("v1"));
    // Verify file on disk
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1");
}

#[test]
fn tool_checkout_current() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("co_current.txt");
    fs::write(&file, "only version").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryCheckoutTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("current")),
    ]));
    assert!(!result.is_error);
}

#[test]
fn tool_checkout_invalid_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("co_invalid.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryCheckoutTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("v99")),
    ]));
    assert!(result.is_error);
}

#[test]
fn tool_checkout_no_history() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("co_nohist.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryCheckoutTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("v1")),
    ]));
    assert!(result.is_error);
    assert!(result.output.contains("No history"));
}

// ═══════════════════════════════════════════════════════════
// Part 3: Additional edge cases (supplementary)
// ═══════════════════════════════════════════════════════════

#[test]
fn tool_read_pagination_shows_hint() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("read_page.txt");
    let content = (1..=20).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
    fs::write(&file, content).unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryReadTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("limit", json!(5)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("1-5 of 20"));
    assert!(result.output.contains("more lines"));
}

#[test]
fn tool_history_grep_specific_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("grep_ver.txt");
    fs::write(&file, "hello v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "world v2").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryGrepTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("v1")),
        ("path", json!(file.to_str().unwrap())),
        ("version", json!(1)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("v1"));
}

#[test]
fn tool_history_grep_context_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("grep_ctx.txt");
    fs::write(&file, "before\nTARGET\nafter").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryGrepTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("TARGET")),
        ("path", json!(file.to_str().unwrap())),
        ("context", json!(1)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("before"));
    assert!(result.output.contains("after"));
}

#[test]
fn tool_history_grep_ignore_case() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("grep_case.txt");
    fs::write(&file, "HeLLo WoRLd").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryGrepTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("hello")),
        ("path", json!(file.to_str().unwrap())),
        ("ignore_case", json!(true)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("HeLLo"));
}

#[test]
fn tool_diff_name_only_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_name.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2 changed").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v1")),
        ("to", json!("v2")),
        ("mode", json!("name-only")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("diff_name.txt"));
}

#[test]
fn tool_diff_chain_stat_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_chain_stat.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    for i in 1..=3 {
        if i > 1 {
            fs::write(&file, format!("v{} changed", i)).unwrap();
        }
        fh.snapshot(&file).unwrap();
    }

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v1")),
        ("to", json!("v2")),
        ("to2", json!("v3")),
        ("mode", json!("stat")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("Chain diff"));
    assert!(result.output.contains("Total:"));
}

#[test]
fn tool_diff_version_specifiers() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("diff_spec.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    for i in 1..=3 {
        if i > 1 {
            fs::write(&file, format!("v{}", i)).unwrap();
        }
        fh.snapshot(&file).unwrap();
    }

    let tool = FileHistoryDiffTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("last1")),
        ("to", json!("current")),
    ]));
    assert!(!result.is_error);
}

#[test]
fn tool_search_no_history() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistorySearchTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!("/no/history.txt")),
        ("query", json!("test")),
    ]));
    assert!(result.is_error);
    assert!(result.output.contains("No history"));
}

#[test]
fn tool_summary_no_files() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistorySummaryTool::new(fh);
    let result = tool.execute(HashMap::new());
    assert!(!result.is_error);
    assert!(result.output.contains("No files"));
}

#[test]
fn tool_tag_list_filter_by_tag() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tag_filter.txt");
    fs::write(&file, "content").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fh.add_tag(&file, "alpha");
    fh.add_tag(&file, "beta");

    let tool = FileHistoryTagTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("action", json!("list")),
        ("tag", json!("alpha")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("alpha"));
}

#[test]
fn tool_batch_read_specific_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("batch_ver.rs");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryBatchTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("*.rs")),
        ("action", json!("read")),
        ("version", json!(1)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("v1"));
}

#[test]
fn tool_checkout_default_is_last1() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("co_default.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryCheckoutTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(!result.is_error);
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1");
}

#[test]
fn tool_checkout_already_at_current() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("co_already.txt");
    fs::write(&file, "only").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryCheckoutTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!("v1")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("Already"));
}

#[test]
fn edge_empty_file_diff() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edge_empty.txt");
    fs::write(&file, "").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "now has content").unwrap();
    fh.snapshot(&file).unwrap();

    let diff = fh.diff(&file, 1, 2).unwrap();
    assert!(!diff.hunks.is_empty());
    let hunk = &diff.hunks[0];
    assert!(hunk.lines.iter().any(|l| l.starts_with("+ ")));
}

#[test]
fn edge_unicode_content() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edge_unicode.txt");
    fs::write(&file, "你好世界\n🎉 emoji test\n日本語").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryReadTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("你好世界"));
    assert!(result.output.contains("🎉"));
}

#[test]
fn edge_large_file_read_with_limit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edge_large.txt");
    let content = (1..=500).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
    fs::write(&file, content).unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryReadTool::new(fh);
    let result = tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("limit", json!(10)),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("10 of 500"));
    assert!(result.output.contains("more lines"));
}

#[test]
fn edge_multiple_tools_same_arc() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edge_arc.txt");
    fs::write(&file, "v1").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    // Three tools sharing the same Arc<FileHistory>
    let list_tool = FileHistoryTool::new(fh.clone());
    let read_tool = FileHistoryReadTool::new(fh.clone());
    let diff_tool = FileHistoryDiffTool::new(fh.clone());

    let r1 = list_tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
    ]));
    assert!(!r1.is_error);

    let r2 = read_tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("version", json!(1)),
    ]));
    assert!(!r2.is_error);

    let r3 = diff_tool.execute(params(vec![
        ("path", json!(file.to_str().unwrap())),
        ("from", json!("v1")),
        ("to", json!("v2")),
    ]));
    assert!(!r3.is_error);
}

#[test]
fn edge_glob_pattern_double_star() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("src");
    fs::create_dir(&sub).unwrap();
    let file = sub.join("main.rs");
    fs::write(&file, "fn main() {}").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();

    let tool = FileHistoryBatchTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("**/*.rs")),
    ]));
    assert!(!result.is_error);
    assert!(result.output.contains("main.rs"));
}

#[test]
fn edge_batch_invalid_glob() {
    let fh = Arc::new(FileHistory::new());
    let tool = FileHistoryBatchTool::new(fh);
    let result = tool.execute(params(vec![
        ("pattern", json!("***/invalid[")),
    ]));
    assert!(result.is_error);
}

#[test]
fn edge_diff_v1_v3_single_hunk_merge() {
    // Regression: v1→v3 should produce a single hunk when changes are nearby
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edge_merge.txt");
    fs::write(&file, "Hello World\n这是第一行\n这是第二行").unwrap();

    let fh = Arc::new(FileHistory::new());
    fh.snapshot(&file).unwrap();
    fh.snapshot(&file).unwrap(); // v2: same content

    fs::write(&file, "Hello Universe\n这是第一行\n这是第二行\n这是第五行").unwrap();
    fh.snapshot(&file).unwrap(); // v3

    let count = fh.count(&file);
    assert!(count >= 2);
    let diff = fh.diff(&file, 1, count).unwrap();
    assert_eq!(diff.hunks.len(), 1, "Expected 1 hunk for nearby changes, got {}", diff.hunks.len());
    let hunk = &diff.hunks[0];
    assert_eq!(hunk.from_count, 3);
    assert_eq!(hunk.to_count, 4);
}
