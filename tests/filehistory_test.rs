//! Integration tests for filehistory module

use miniclaudecode_rust::filehistory::FileHistory;
use chrono::Datelike;
use std::fs;
use tempfile::TempDir;

// ─── FileHistory basic operations ───

#[test]
fn filehistory_new() {
    let fh = FileHistory::new();
    drop(fh);
}

#[test]
fn filehistory_default() {
    let fh = FileHistory::default();
    drop(fh);
}

#[test]
fn filehistory_snapshot_nonexistent_file() {
    let fh = FileHistory::new();
    let result = fh.snapshot(std::path::Path::new("/nonexistent/file.txt"));
    assert!(result.is_ok());
    assert!(result.unwrap().is_none()); // No snapshot for nonexistent file
}

#[test]
fn filehistory_snapshot_existing_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "Hello world").unwrap();

    let fh = FileHistory::new();
    let result = fh.snapshot(&file).unwrap();
    assert!(result.is_some());
    let snapshot = result.unwrap();
    assert_eq!(snapshot.content, "Hello world");
    assert_eq!(snapshot.path, file);
}

#[test]
fn filehistory_no_snapshot_if_content_unchanged() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "Same content").unwrap();

    let fh = FileHistory::new();

    // First snapshot
    let s1 = fh.snapshot(&file).unwrap();
    assert!(s1.is_some());

    // Second snapshot without changing content
    let s2 = fh.snapshot(&file).unwrap();
    assert!(s2.is_none()); // No change, so no new snapshot
}

#[test]
fn filehistory_snapshot_after_change() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "Version 1").unwrap();

    let fh = FileHistory::new();
    let s1 = fh.snapshot(&file).unwrap();
    assert!(s1.is_some());

    // Change content
    fs::write(&file, "Version 2").unwrap();
    let s2 = fh.snapshot(&file).unwrap();
    assert!(s2.is_some());

    // Both snapshots should have different checksums
    assert_ne!(s1.unwrap().checksum, s2.unwrap().checksum);
}

#[test]
fn filehistory_count() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();

    // Nonexistent file
    let nonexistent = dir.path().join("missing.txt");
    assert_eq!(fh.count(&nonexistent), 0);

    // After first snapshot
    fh.snapshot(&file).unwrap();
    assert_eq!(fh.count(&file), 1);

    // Change and snapshot again
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();
    assert_eq!(fh.count(&file), 2);
}

#[test]
fn filehistory_restore() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap(); // Snapshots "v1"

    // Change file
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap(); // Snapshots "v2"

    // Restore to previous version (content-aware: 1 step back = v1)
    let content = fh.restore(&file).unwrap();
    assert!(content.is_some());
    assert_eq!(content.unwrap(), "v1");

    // Verify file on disk
    let disk_content = fs::read_to_string(&file).unwrap();
    assert_eq!(disk_content, "v1");

    // Verify history is preserved (restore snapshots current before restoring)
    assert!(fh.count(&file) >= 3); // v1, v2, v2(restore)

    // Content-aware restore: since disk is now v1 and restore snapshot has v2 checksum,
    // another restore goes back 1 distinct content step from v2 → v1 again
    // (v2(restore) is collapsed with v2 in content-aware logic)
    // To redo back to v2, we need to change the file first
    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    // Now restore should go back to v2 (1 distinct step from v3)
    let content2 = fh.restore(&file).unwrap();
    assert!(content2.is_some());
    assert_eq!(content2.unwrap(), "v2");
}

#[test]
fn filehistory_restore_no_previous_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "only version").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Only one snapshot, no previous version to restore
    let content = fh.restore(&file).unwrap();
    assert!(content.is_none());
}

#[test]
fn filehistory_restore_nonexistent_file() {
    let fh = FileHistory::new();
    let result = fh.restore(std::path::Path::new("/nonexistent"));
    assert!(result.is_ok());
    let content = result.unwrap();
    assert!(content.is_none());
}

#[test]
fn filehistory_rewind() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    assert_eq!(fh.count(&file), 3);

    // Rewind 2 steps back
    let content = fh.rewind(&file, 2).unwrap();
    assert!(content.is_some());
    assert_eq!(content.unwrap(), "v1");

    // Verify history is preserved (rewind snapshots current before rewinding)
    assert_eq!(fh.count(&file), 4); // v1, v2, v3, v3(restore)

    // Verify redo is possible - can still read v3
    let snapshots = fh.get_snapshots(&file);
    assert_eq!(snapshots[2].content, "v3"); // Original v3 still accessible
}

#[test]
fn filehistory_rewind_no_steps() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "content").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Rewind 0 steps
    let content = fh.rewind(&file, 0).unwrap();
    assert!(content.is_none()); // Nothing to rewind
}

#[test]
fn filehistory_rewind_nonexistent() {
    let fh = FileHistory::new();
    let result = fh.rewind(std::path::Path::new("/nonexistent"), 1).unwrap();
    assert!(result.is_none());
}

#[test]
fn filehistory_clear() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();
    assert_eq!(fh.count(&file), 1);

    fh.clear(&file);
    assert_eq!(fh.count(&file), 0);
}

#[test]
fn filehistory_clear_all() {
    let dir = TempDir::new().unwrap();
    let file1 = dir.path().join("test1.txt");
    let file2 = dir.path().join("test2.txt");
    fs::write(&file1, "content1").unwrap();
    fs::write(&file2, "content2").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file1).unwrap();
    fh.snapshot(&file2).unwrap();

    assert_eq!(fh.count(&file1), 1);
    assert_eq!(fh.count(&file2), 1);

    fh.clear_all();
    assert_eq!(fh.count(&file1), 0);
    assert_eq!(fh.count(&file2), 0);
}

#[test]
fn filehistory_max_snapshots_limit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();

    // Create more than max_snapshots (50)
    for i in 2..=55 {
        fs::write(&file, format!("v{}", i)).unwrap();
        fh.snapshot(&file).unwrap();
    }

    // Should be capped at max_snapshots
    assert!(fh.count(&file) <= 50);
}

#[test]
fn filehistory_clone() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "content").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    let cloned = fh.clone();
    assert_eq!(cloned.count(&file), 1);
}

#[test]
fn filehistory_snapshot_checksum() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "test data").unwrap();

    let fh = FileHistory::new();
    let snapshot = fh.snapshot(&file).unwrap().unwrap();
    assert!(!snapshot.checksum.is_empty());
    assert!(snapshot.timestamp.year() > 2020);
}

#[test]
fn filehistory_snapshot_current_new_file() {
    // Bug 6: new files should enter history after creation
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("new_file.txt");

    let fh = FileHistory::new();

    // File doesn't exist yet - snapshot() returns None
    assert!(fh.snapshot(&file).unwrap().is_none());
    assert_eq!(fh.count(&file), 0);

    // Create the file
    fs::write(&file, "initial content").unwrap();

    // snapshot_current captures the file's current state
    let result = fh.snapshot_current(&file).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().content, "initial content");
    assert_eq!(fh.count(&file), 1);

    // Edit and snapshot_current again
    fs::write(&file, "edited content").unwrap();
    let result = fh.snapshot_current(&file).unwrap();
    assert!(result.is_some());
    assert_eq!(fh.count(&file), 2);
}

#[test]
fn filehistory_restore_preserves_history() {
    // Bug 5: restore should not delete history
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    assert_eq!(fh.count(&file), 3);

    // Restore should preserve history
    fh.restore(&file).unwrap();
    assert!(fh.count(&file) >= 3); // Should be 3 or more, not less

    // Verify v3 content is still accessible in history
    let snapshots = fh.get_snapshots(&file);
    let has_v3 = snapshots.iter().any(|s| s.content == "v3");
    assert!(has_v3, "v3 content should still be in history after restore");
}

#[test]
fn filehistory_snapshot_with_description() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "line1\n").unwrap();

    let fh = FileHistory::new();
    fh.snapshot_with_desc(&file, "initial commit".to_string()).unwrap();

    fs::write(&file, "line1\nline2\n").unwrap();
    fh.snapshot_with_desc(&file, "edit: added line2".to_string()).unwrap();

    let snapshots = fh.get_snapshots(&file);
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].description, "initial commit");
    assert_eq!(snapshots[1].description, "edit: added line2");
}

#[test]
fn filehistory_diff() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "line1\nline2\nline3\n").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "line1\nline2_modified\nline3\nline4\n").unwrap();
    fh.snapshot(&file).unwrap();

    // Diff between v1 and v2
    let result = fh.diff(&file, 1, 2);
    assert!(result.is_some());
    let diff = result.unwrap();
    assert_eq!(diff.from_version, 1);
    assert_eq!(diff.to_version, 2);
    // Should have at least one hunk
    assert!(!diff.hunks.is_empty());
}

#[test]
fn filehistory_resolve_version() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();
    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    // Numeric
    assert_eq!(fh.resolve_version(&file, "v1"), Some(1));
    assert_eq!(fh.resolve_version(&file, "v3"), Some(3));
    assert_eq!(fh.resolve_version(&file, "2"), Some(2));

    // current/latest
    assert_eq!(fh.resolve_version(&file, "current"), Some(3));
    assert_eq!(fh.resolve_version(&file, "latest"), Some(3));

    // lastN
    assert_eq!(fh.resolve_version(&file, "last1"), Some(2));
    assert_eq!(fh.resolve_version(&file, "last2"), Some(1));

    // Invalid
    assert_eq!(fh.resolve_version(&file, "v99"), None);
}

#[test]
fn filehistory_tag() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Add tag
    assert!(fh.add_tag(&file, "before-refactor"));

    // Verify tag is in description
    let snapshots = fh.get_snapshots(&file);
    assert!(snapshots[0].description.contains("before-refactor"), "description was: '{}'", snapshots[0].description);

    // List tags
    let tags = fh.list_tags(&file);
    assert!(!tags.is_empty(), "expected at least 1 tag, got {}", tags.len());
    assert_eq!(tags[0].1, "before-refactor");
}

#[test]
fn filehistory_search_added_removed() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "line1\nline2\n").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // Remove line2, add line3
    fs::write(&file, "line1\nline3\n").unwrap();
    fh.snapshot(&file).unwrap();

    // Search for "line3" being added
    let results = fh.search(&file, "line3", miniclaudecode_rust::filehistory::SearchMode::Added, false);
    assert!(!results.is_empty());

    // Search for "line2" being removed
    let results = fh.search(&file, "line2", miniclaudecode_rust::filehistory::SearchMode::Removed, false);
    assert!(!results.is_empty());
}

#[test]
fn filehistory_summary_and_timeline() {
    let dir = TempDir::new().unwrap();
    let file1 = dir.path().join("file1.txt");
    let file2 = dir.path().join("file2.txt");
    fs::write(&file1, "content1").unwrap();
    fs::write(&file2, "content2").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file1).unwrap();
    fh.snapshot(&file2).unwrap();

    let summary = fh.get_summary(None);
    assert_eq!(summary.len(), 2);

    let timeline = fh.get_timeline(None);
    assert_eq!(timeline.len(), 2);
}

#[test]
fn filehistory_checkout() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v3").unwrap();
    fh.snapshot(&file).unwrap();

    // Checkout v1 via rewind
    let result = fh.rewind(&file, 2).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap(), "v1");

    // File on disk should be v1
    let disk = fs::read_to_string(&file).unwrap();
    assert_eq!(disk, "v1");

    // History should be preserved (with restore snapshot)
    assert!(fh.count(&file) >= 3);
}

#[test]
fn filehistory_diff_simple_addition() {
    // Bug: diff shows common lines as both removed and re-added
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");

    // v1: 3 lines
    fs::write(&file, "Hello World\n这是第一行\n这是第二行").unwrap();
    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    // v2: 4 lines (added one line)
    fs::write(&file, "Hello World\n这是第一行\n这是第二行\n这是第三行 - 新增内容").unwrap();
    fh.snapshot(&file).unwrap();

    let result = fh.diff(&file, 1, 2).unwrap();
    assert_eq!(result.hunks.len(), 1);
    let hunk = &result.hunks[0];
    assert_eq!(hunk.from_count, 3);
    assert_eq!(hunk.to_count, 4);

    let adds: Vec<_> = hunk.lines.iter().filter(|l| l.starts_with("+ ")).collect();
    let removes: Vec<_> = hunk.lines.iter().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(removes.len(), 0, "Should have no removals, got: {:?}", removes);
    assert_eq!(adds.len(), 1, "Should have exactly 1 addition, got: {:?}", adds);
}

#[test]
fn filehistory_checkout_direct() {
    // Bug: checkout tool uses rewind() which skips same-checksum versions
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");

    fs::write(&file, "v1 content").unwrap();
    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2 content").unwrap();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v3 content").unwrap();
    fh.snapshot(&file).unwrap();

    // Direct checkout to v1
    let content = fh.checkout(&file, 1).unwrap().unwrap();
    assert_eq!(content, "v1 content");
    assert_eq!(fs::read_to_string(&file).unwrap(), "v1 content");

    // Direct checkout to v2
    let content = fh.checkout(&file, 2).unwrap().unwrap();
    assert_eq!(content, "v2 content");
    assert_eq!(fs::read_to_string(&file).unwrap(), "v2 content");

    // Direct checkout to v3
    let content = fh.checkout(&file, 3).unwrap().unwrap();
    assert_eq!(content, "v3 content");
    assert_eq!(fs::read_to_string(&file).unwrap(), "v3 content");
}

#[test]
fn filehistory_annotate() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "v1").unwrap();

    let fh = FileHistory::new();
    fh.snapshot(&file).unwrap();

    fs::write(&file, "v2").unwrap();
    fh.snapshot_with_desc(&file, "edit: fix login bug".to_string()).unwrap();

    // Annotate v1
    assert!(fh.annotate_snapshot(&file, 1, "initial version"));
    let snapshots = fh.get_snapshots(&file);
    assert!(snapshots[0].description.contains("initial version"));

    // Annotate v2 (append to existing description)
    assert!(fh.annotate_snapshot(&file, 2, "reviewed by team"));
    let snapshots = fh.get_snapshots(&file);
    assert!(snapshots[1].description.contains("edit: fix login bug"));
    assert!(snapshots[1].description.contains("reviewed by team"));
    assert!(snapshots[1].description.contains(" | "), "description was: '{}'", snapshots[1].description);

    // Annotate nonexistent version
    assert!(!fh.annotate_snapshot(&file, 99, "should fail"));

    // Annotate nonexistent file
    assert!(!fh.annotate_snapshot(std::path::Path::new("/nonexistent"), 1, "msg"));

    // Empty message
    assert!(!fh.annotate_snapshot(&file, 1, ""));
}

#[test]
fn filehistory_batch_pattern() {
    let dir = TempDir::new().unwrap();

    // Create multiple files with different extensions
    let rs1 = dir.path().join("main.rs");
    let rs2 = dir.path().join("lib.rs");
    let py1 = dir.path().join("script.py");

    fs::write(&rs1, "fn main() {}").unwrap();
    fs::write(&rs2, "pub fn lib() {}").unwrap();
    fs::write(&py1, "print('hello')").unwrap();

    let fh = FileHistory::new();

    // Snapshot all files
    fh.snapshot(&rs1).unwrap();
    fs::write(&rs1, "fn main() { println!(\"v2\"); }").unwrap();
    fh.snapshot(&rs1).unwrap();

    fh.snapshot(&rs2).unwrap();
    fh.snapshot(&py1).unwrap();

    // Verify listing
    let all = fh.list_all_files();
    assert_eq!(all.len(), 3);

    // Pattern matching via glob
    let glob_pattern = glob::Pattern::new("*.rs").unwrap();
    let rs_files: Vec<_> = all.iter()
        .filter(|p| glob_pattern.matches(&p.to_string_lossy()))
        .collect();
    assert_eq!(rs_files.len(), 2);

    let py_files: Vec<_> = all.iter()
        .filter(|p| glob_pattern.matches(&p.to_string_lossy()) == false)
        .collect();
    assert_eq!(py_files.len(), 1);
}
