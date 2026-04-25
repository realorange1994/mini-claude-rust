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

    // Restore to previous version
    let content = fh.restore(&file).unwrap();
    assert!(content.is_some());
    assert_eq!(content.unwrap(), "v1");

    // Verify file on disk
    let disk_content = fs::read_to_string(&file).unwrap();
    assert_eq!(disk_content, "v1");
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

    // Create more than max_snapshots (10)
    for i in 2..=15 {
        fs::write(&file, format!("v{}", i)).unwrap();
        fh.snapshot(&file).unwrap();
    }

    // Should be capped at max_snapshots
    assert!(fh.count(&file) <= 10);
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
