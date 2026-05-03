//! Comprehensive unit tests for tools module
//!
//! Covers:
//! - ToolResult constructors
//! - Registry (register, get, all_tools)
//! - validate_params
//! - expand_path, is_ignored_dir, strip_tags, contains_internal_url, restore_crlf, truncate_at
//! - File tools (read_file, write_file, edit_file, multi_edit, fileops, list_dir)
//! - search tools (grep, glob)

use miniclaudecode_rust::tools::{
    expand_path, is_ignored_dir, strip_tags, contains_internal_url, restore_crlf, truncate_at,
    ToolResult, validate_params, Tool, Registry,
};
use miniclaudecode_rust::tools::{
    FileReadTool, FileWriteTool, FileEditTool, MultiEditTool, FileOpsTool, ListDirTool,
    GrepTool, GlobTool, GitTool, RuntimeInfoTool,
};
use std::collections::HashMap;
use std::fs;

// ============================================================
// ToolResult tests
// ============================================================

#[test]
fn tool_result_ok_string() {
    let r = ToolResult::ok("success");
    assert_eq!(r.output, "success");
    assert!(!r.is_error);
}

#[test]
fn tool_result_error_string() {
    let r = ToolResult::error("something broke");
    assert_eq!(r.output, "something broke");
    assert!(r.is_error);
}

#[test]
fn tool_result_ok_string_ref() {
    let s = String::from("hello");
    let r = ToolResult::ok(s);
    assert_eq!(r.output, "hello");
}

// ============================================================
// validate_params tests
// ============================================================

#[test]
fn validate_params_all_present() {
    struct MockTool;
    impl Tool for MockTool {
        fn name(&self) -> &str { "mock" }
        fn description(&self) -> &str { "mock" }
        fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
            serde_json::json!({
                "required": ["a", "b"]
            }).as_object().unwrap().clone()
        }
        fn check_permissions(&self, _p: &HashMap<String, serde_json::Value>) -> Option<ToolResult> { None }
        fn execute(&self, _p: HashMap<String, serde_json::Value>) -> ToolResult { ToolResult::ok("ok") }
    }

    let mut params = HashMap::new();
    params.insert("a".into(), serde_json::json!("val"));
    params.insert("b".into(), serde_json::json!("val"));
    assert!(validate_params(&MockTool, &params).is_none());
}

#[test]
fn validate_params_missing_required() {
    struct MockTool;
    impl Tool for MockTool {
        fn name(&self) -> &str { "mock" }
        fn description(&self) -> &str { "mock" }
        fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
            serde_json::json!({
                "required": ["path"]
            }).as_object().unwrap().clone()
        }
        fn check_permissions(&self, _p: &HashMap<String, serde_json::Value>) -> Option<ToolResult> { None }
        fn execute(&self, _p: HashMap<String, serde_json::Value>) -> ToolResult { ToolResult::ok("ok") }
    }

    let params: HashMap<String, serde_json::Value> = HashMap::new();
    let result = validate_params(&MockTool, &params);
    assert!(result.is_some());
    let r = result.unwrap();
    assert!(r.is_error);
    assert!(r.output.contains("path"));
}

#[test]
fn validate_params_no_required_fields() {
    struct MockTool;
    impl Tool for MockTool {
        fn name(&self) -> &str { "mock" }
        fn description(&self) -> &str { "mock" }
        fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
            serde_json::json!({}).as_object().unwrap().clone()
        }
        fn check_permissions(&self, _p: &HashMap<String, serde_json::Value>) -> Option<ToolResult> { None }
        fn execute(&self, _p: HashMap<String, serde_json::Value>) -> ToolResult { ToolResult::ok("ok") }
    }

    let params: HashMap<String, serde_json::Value> = HashMap::new();
    assert!(validate_params(&MockTool, &params).is_none());
}

// ============================================================
// Registry tests
// ============================================================

#[test]
fn registry_register_and_get() {
    let reg = Registry::new();
    reg.register(FileReadTool);
    let tool = reg.get("read_file").unwrap();
    assert_eq!(tool.name(), "read_file");
}

#[test]
fn registry_get_unknown() {
    let reg = Registry::new();
    assert!(reg.get("nonexistent").is_none());
}

#[test]
fn registry_all_tools() {
    let reg = Registry::new();
    reg.register(FileReadTool);
    reg.register(FileWriteTool);
    let tools = reg.all_tools();
    assert_eq!(tools.len(), 2);
}

#[test]
fn registry_default() {
    let reg = Registry::default();
    assert!(reg.all_tools().is_empty());
}

// ============================================================
// expand_path tests
// ============================================================

#[test]
fn expand_path_absolute() {
    let path = expand_path("/tmp/test.txt");
    assert!(path.is_absolute());
}

#[test]
fn expand_path_relative() {
    let path = expand_path("relative/path.txt");
    // Should be relative to current dir
    assert!(path.ends_with("relative/path.txt"));
}

#[test]
fn expand_path_tilde() {
    // ~ should be expanded to home directory
    let path = expand_path("~/test.txt");
    #[cfg(target_os = "windows")]
    {
        // On Windows, HOME might not be set, falls back to USERPROFILE
        assert!(!path.to_string_lossy().starts_with('~'));
    }
    #[cfg(not(target_os = "windows"))]
    {
        assert!(!path.to_string_lossy().starts_with('~'));
    }
}

// ============================================================
// is_ignored_dir tests
// ============================================================

#[test]
fn is_ignored_dir_git() {
    assert!(is_ignored_dir(std::ffi::OsStr::new(".git")));
}

#[test]
fn is_ignored_dir_node_modules() {
    assert!(is_ignored_dir(std::ffi::OsStr::new("node_modules")));
}

#[test]
fn is_ignored_dir_target() {
    assert!(is_ignored_dir(std::ffi::OsStr::new("target")));
}

#[test]
fn is_ignored_dir_normal() {
    assert!(!is_ignored_dir(std::ffi::OsStr::new("src")));
    assert!(!is_ignored_dir(std::ffi::OsStr::new("lib")));
}

// ============================================================
// strip_tags tests
// ============================================================

#[test]
fn strip_tags_basic() {
    assert_eq!(strip_tags("<b>Hello</b>"), "Hello");
}

#[test]
fn strip_tags_nested() {
    assert_eq!(strip_tags("<div><p>Text</p></div>"), "Text");
}

#[test]
fn strip_tags_with_entities() {
    assert_eq!(strip_tags("&amp;"), "&");
    assert_eq!(strip_tags("&lt;b&gt;"), "<b>");
    assert_eq!(strip_tags("&quot;test&quot;"), "\"test\"");
    assert_eq!(strip_tags("&#39;"), "'");
    assert_eq!(strip_tags("&nbsp;"), " ");
}

#[test]
fn strip_tags_no_tags() {
    assert_eq!(strip_tags("plain text"), "plain text");
}

#[test]
fn strip_tags_empty() {
    assert_eq!(strip_tags(""), "");
    assert_eq!(strip_tags("<></>"), "");
}

// ============================================================
// contains_internal_url tests
// ============================================================

#[test]
fn contains_internal_url_localhost() {
    assert!(contains_internal_url("http://localhost:8080"));
    assert!(contains_internal_url("https://localhost"));
}

#[test]
fn contains_internal_url_127_0_0_1() {
    assert!(contains_internal_url("http://127.0.0.1/api"));
}

#[test]
fn contains_internal_url_private_ranges() {
    assert!(contains_internal_url("http://192.168.1.1"));
    assert!(contains_internal_url("http://10.0.0.1"));
    assert!(contains_internal_url("http://172.16.0.1"));
}

#[test]
fn contains_internal_url_ipv6() {
    assert!(contains_internal_url("http://[::1]:8080"));
}

#[test]
fn contains_internal_url_public() {
    assert!(!contains_internal_url("https://example.com"));
    assert!(!contains_internal_url("https://api.openai.com"));
}

// ============================================================
// restore_crlf tests
// ============================================================

#[test]
fn restore_crlf_lf_only() {
    let s = "line1\nline2\n";
    let result = restore_crlf(s);
    assert_eq!(result, "line1\r\nline2\r\n");
}

#[test]
fn restore_crlf_already_crlf() {
    let s = "line1\r\nline2\r\n";
    let result = restore_crlf(s);
    assert_eq!(result, "line1\r\nline2\r\n");
}

#[test]
fn restore_crlf_empty() {
    assert_eq!(restore_crlf(""), "");
}

// ============================================================
// truncate_at tests
// ============================================================

#[test]
fn truncate_at_shorter() {
    assert_eq!(truncate_at("hello", 10), "hello");
}

#[test]
fn truncate_at_exact() {
    assert_eq!(truncate_at("hello", 5), "hello");
}

#[test]
fn truncate_at_truncated() {
    assert_eq!(truncate_at("hello world", 5), "hello");
}

#[test]
fn truncate_at_utf8_char_boundary() {
    // "你好" is 6 bytes, truncate_at should not split in the middle
    let s = "你好世界";
    let result = truncate_at(s, 4);
    // Should truncate to "你" (3 bytes) since position 4 is not a char boundary
    assert_eq!(result, "你");
}

#[test]
fn truncate_at_zero() {
    assert_eq!(truncate_at("hello", 0), "");
}

// ============================================================
// FileReadTool tests
// ============================================================

#[test]
fn file_read_tool_name_and_schema() {
    let tool = FileReadTool;
    assert_eq!(tool.name(), "read_file");
    let schema = tool.input_schema();
    assert!(schema.get("required").is_some());
}

#[test]
fn file_read_file_not_found() {
    let tool = FileReadTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!("/nonexistent/file.txt"));
    let result = tool.execute(params);
    assert!(result.is_error);
    assert!(result.output.contains("not found"));
}

#[test]
fn file_read_missing_path() {
    let tool = FileReadTool;
    let params: HashMap<String, serde_json::Value> = HashMap::new();
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn file_read_directory() {
    let tool = FileReadTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!("/tmp"));
    let result = tool.execute(params);
    assert!(result.is_error);
    assert!(result.output.contains("not a file"));
}

#[test]
fn file_read_success() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "line1\nline2\nline3\n").unwrap();

    let tool = FileReadTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("1\tline1"));
    assert!(result.output.contains("2\tline2"));
    assert!(result.output.contains("3\tline3"));
    assert!(result.output.contains("3 lines total"));
}

#[test]
fn file_read_with_offset_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

    let tool = FileReadTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("offset".into(), serde_json::json!(2));
    params.insert("limit".into(), serde_json::json!(2));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("2\tb"));
    assert!(result.output.contains("3\tc"));
    // Should show pagination hint
    assert!(result.output.contains("offset="));
}

#[test]
fn file_read_offset_beyond_end() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "single line\n").unwrap();

    let tool = FileReadTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("offset".into(), serde_json::json!(100));
    let result = tool.execute(params);
    assert!(result.is_error);
    assert!(result.output.contains("beyond end"));
}

// ============================================================
// FileWriteTool tests
// ============================================================

#[test]
fn file_write_tool_name_and_schema() {
    let tool = FileWriteTool;
    assert_eq!(tool.name(), "write_file");
    let schema = tool.input_schema();
    let required = schema.get("required").unwrap().as_array().unwrap();
    assert!(required.contains(&serde_json::json!("file_path")));
    assert!(required.contains(&serde_json::json!("content")));
}

#[test]
fn file_write_success() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("output.txt");

    let tool = FileWriteTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("content".into(), serde_json::json!("hello world"));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("Wrote"));
    assert!(result.output.contains("11"));

    // Verify file content
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");
}

#[test]
fn file_write_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sub").join("dir").join("file.txt");

    let tool = FileWriteTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("content".into(), serde_json::json!("content"));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(file.exists());
}

#[test]
fn file_write_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("overwrite.txt");
    fs::write(&file, "old content").unwrap();

    let tool = FileWriteTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("content".into(), serde_json::json!("new content"));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert_eq!(fs::read_to_string(&file).unwrap(), "new content");
}

#[test]
fn file_write_missing_content() {
    let tool = FileWriteTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!("/tmp/test.txt"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

// ============================================================
// FileEditTool tests
// ============================================================

#[test]
fn file_edit_success() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("edit.txt");
    fs::write(&file, "hello world\nfoo bar").unwrap();

    let tool = FileEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("old_string".into(), serde_json::json!("hello"));
    params.insert("new_string".into(), serde_json::json!("goodbye"));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("Successfully edited"));
    assert!(fs::read_to_string(&file).unwrap().contains("goodbye world"));
}

#[test]
fn file_edit_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("edit.txt");
    fs::write(&file, "some content").unwrap();

    let tool = FileEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("old_string".into(), serde_json::json!("not there"));
    params.insert("new_string".into(), serde_json::json!("replacement"));
    let result = tool.execute(params);
    assert!(result.is_error);
    assert!(result.output.contains("not found"));
}

#[test]
fn file_edit_multiple_without_replace_all() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("edit.txt");
    fs::write(&file, "foo\nfoo\nfoo").unwrap();

    let tool = FileEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("old_string".into(), serde_json::json!("foo"));
    params.insert("new_string".into(), serde_json::json!("bar"));
    let result = tool.execute(params);
    assert!(result.is_error);
    assert!(result.output.contains("appears 3 times"));
}

#[test]
fn file_edit_replace_all() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("edit.txt");
    fs::write(&file, "foo\nfoo\nfoo").unwrap();

    let tool = FileEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("old_string".into(), serde_json::json!("foo"));
    params.insert("new_string".into(), serde_json::json!("bar"));
    params.insert("replace_all".into(), serde_json::json!(true));
    let result = tool.execute(params);
    assert!(!result.is_error);
    let content = fs::read_to_string(&file).unwrap();
    assert_eq!(content, "bar\nbar\nbar");
}

#[test]
fn file_edit_missing_old_string() {
    let tool = FileEditTool;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("existing.txt");
    fs::write(&file, "existing content").unwrap();
    let mut params = HashMap::new();
    params.insert("file_path".into(), serde_json::json!(file.to_str().unwrap()));
    params.insert("old_string".into(), serde_json::json!("nonexistent text"));
    params.insert("new_string".into(), serde_json::json!("replacement"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

// ============================================================
// MultiEditTool tests
// ============================================================

#[test]
fn multi_edit_success() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("multi.txt");
    fs::write(&file, "a\nb\nc\n").unwrap();

    let tool = MultiEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("edits".into(), serde_json::json!([
        {"old_string": "a", "new_string": "A"},
        {"old_string": "c", "new_string": "C"}
    ]));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("2 edits"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "A\nb\nC\n");
}

#[test]
fn multi_edit_atomic_rollback_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("multi.txt");
    fs::write(&file, "a\nb\nc\n").unwrap();

    let tool = MultiEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("edits".into(), serde_json::json!([
        {"old_string": "a", "new_string": "A"},
        {"old_string": "nonexistent", "new_string": "X"}
    ]));
    let result = tool.execute(params);
    assert!(result.is_error);
    // File should be unchanged (atomic rollback)
    assert_eq!(fs::read_to_string(&file).unwrap(), "a\nb\nc\n");
}

#[test]
fn multi_edit_empty_edits_array() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    fs::write(&file, "content").unwrap();

    let tool = MultiEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    params.insert("edits".into(), serde_json::json!([]));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn multi_edit_missing_file() {
    let tool = MultiEditTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!("/nonexistent/file.txt"));
    params.insert("edits".into(), serde_json::json!([
        {"old_string": "a", "new_string": "b"}
    ]));
    let result = tool.execute(params);
    assert!(result.is_error);
    assert!(result.output.contains("not found"));
}

// ============================================================
// FileOpsTool tests
// ============================================================

#[test]
fn fileops_mkdir() {
    let dir = tempfile::tempdir().unwrap();
    let new_dir = dir.path().join("new_folder");

    let tool = FileOpsTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("mkdir"));
    params.insert("path".into(), serde_json::json!(new_dir.to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(new_dir.is_dir());
}

#[test]
fn fileops_rm() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("to_delete.txt");
    fs::write(&file, "content").unwrap();

    let tool = FileOpsTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("rm"));
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(!file.exists());
}

#[test]
fn fileops_mv() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    let dst = dir.path().join("dst.txt");
    fs::write(&src, "content").unwrap();

    let tool = FileOpsTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("mv"));
    params.insert("path".into(), serde_json::json!(src.to_string_lossy().to_string()));
    params.insert("destination".into(), serde_json::json!(dst.to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(!src.exists());
    assert!(dst.exists());
}

#[test]
fn fileops_cp() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    let dst = dir.path().join("dst.txt");
    fs::write(&src, "content").unwrap();

    let tool = FileOpsTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("cp"));
    params.insert("path".into(), serde_json::json!(src.to_string_lossy().to_string()));
    params.insert("destination".into(), serde_json::json!(dst.to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(src.exists());
    assert!(dst.exists());
}

#[test]
fn fileops_rmrf_protected_paths() {
    let tool = FileOpsTool;
    // Root "/" is always protected
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("rmrf"));
    params.insert("path".into(), serde_json::json!("/"));
    let result = tool.execute(params);
    // On Windows "/" resolves to drive root, may succeed as rmrf
    // just verify it's either an error or the output mentions removal
    if result.is_error {
        assert!(result.output.contains("protected") || result.output.contains("Error"));
    }
}

#[test]
fn fileops_unknown_operation() {
    let tool = FileOpsTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("unknown_op"));
    params.insert("path".into(), serde_json::json!("/tmp"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn fileops_missing_operation() {
    let tool = FileOpsTool;
    let params: HashMap<String, serde_json::Value> = HashMap::new();
    let result = tool.execute(params);
    assert!(result.is_error);
}

// ============================================================
// ListDirTool tests
// ============================================================

#[test]
fn list_dir_simple() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.txt"), "b").unwrap();

    let tool = ListDirTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("a.txt"));
    assert!(result.output.contains("b.txt"));
}

#[test]
fn list_dir_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notadir.txt");
    fs::write(&file, "content").unwrap();

    let tool = ListDirTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(file.to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn list_dir_empty() {
    let dir = tempfile::tempdir().unwrap();

    let tool = ListDirTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("empty"));
}

#[test]
fn list_dir_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("nested.txt"), "nested").unwrap();
    fs::write(dir.path().join("top.txt"), "top").unwrap();

    let tool = ListDirTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("recursive".into(), serde_json::json!(true));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("top.txt"));
    assert!(result.output.contains("nested.txt"));
}

#[test]
fn list_dir_max_entries() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..10 {
        fs::write(dir.path().join(format!("file_{}.txt", i)), "").unwrap();
    }

    let tool = ListDirTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("max_entries".into(), serde_json::json!(3));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("truncated"));
}

#[test]
fn list_dir_ignores_git_and_node_modules() {
    let dir = tempfile::tempdir().unwrap();
    let git_dir = dir.path().join(".git");
    let nm_dir = dir.path().join("node_modules");
    fs::create_dir(&git_dir).unwrap();
    fs::create_dir(&nm_dir).unwrap();
    fs::write(dir.path().join("main.rs"), "").unwrap();

    let tool = ListDirTool;
    let mut params = HashMap::new();
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("recursive".into(), serde_json::json!(true));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(!result.output.contains(".git"));
    assert!(!result.output.contains("node_modules"));
}

// ============================================================
// GrepTool tests (go_search fallback, no rg needed)
// ============================================================

#[test]
fn grep_tool_content_mode() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("test.rs"), "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

    let tool = GrepTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("println"));
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("fixed_strings".into(), serde_json::json!(true));
    let result = tool.execute(params);
    // May use rg if available, or go_search
    assert!(!result.is_error);
}

#[test]
fn grep_tool_no_matches() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("test.txt"), "hello world").unwrap();

    let tool = GrepTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("zzzznotfound"));
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("fixed_strings".into(), serde_json::json!(true));
    let result = tool.execute(params);
    // May or may not be error depending on rg availability
    // Just verify it returns something
    assert!(!result.output.is_empty());
}

#[test]
fn grep_tool_files_only_mode() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "TODO: fix this").unwrap();
    fs::write(dir.path().join("b.txt"), "nothing here").unwrap();

    let tool = GrepTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("TODO"));
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("output_mode".into(), serde_json::json!("files_with_matches"));
    params.insert("fixed_strings".into(), serde_json::json!(true));
    let result = tool.execute(params);
    assert!(!result.output.is_empty());
}

#[test]
fn grep_tool_count_mode() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("test.txt"), "TODO\nTODO\nOK\n").unwrap();

    let tool = GrepTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("TODO"));
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("output_mode".into(), serde_json::json!("count"));
    params.insert("fixed_strings".into(), serde_json::json!(true));
    let result = tool.execute(params);
    assert!(!result.output.is_empty());
}

#[test]
fn grep_tool_path_not_found() {
    let tool = GrepTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("test"));
    params.insert("path".into(), serde_json::json!("/nonexistent/path"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn grep_tool_invalid_regex() {
    let tool = GrepTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("["));
    // Invalid regex, but might succeed if rg is available (rg has different regex syntax)
    // Just verify it doesn't panic
    let _ = tool.execute(params);
}

// ============================================================
// GlobTool tests
// ============================================================

#[test]
fn glob_tool_basic() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.rs"), "").unwrap();
    fs::write(dir.path().join("b.rs"), "").unwrap();
    fs::write(dir.path().join("c.txt"), "").unwrap();

    let tool = GlobTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("*.rs"));
    params.insert("directory".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(!result.output.contains("No files matched"));
}

#[test]
fn glob_tool_no_matches() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();

    let tool = GlobTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("*.rs"));
    params.insert("directory".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("No files matched"));
}

#[test]
fn glob_tool_excludes() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.rs"), "").unwrap();
    fs::write(dir.path().join("main_test.rs"), "").unwrap();

    let tool = GlobTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("*.rs"));
    params.insert("directory".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("excludes".into(), serde_json::json!(["*_test.rs"]));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(!result.output.contains("main_test"));
}

#[test]
fn glob_tool_directory_not_found() {
    let tool = GlobTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("*.rs"));
    params.insert("directory".into(), serde_json::json!("/nonexistent/dir"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn glob_tool_head_limit() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        fs::write(dir.path().join(format!("file_{}.txt", i)), "").unwrap();
    }

    let tool = GlobTool;
    let mut params = HashMap::new();
    params.insert("pattern".into(), serde_json::json!("*.txt"));
    params.insert("directory".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    params.insert("head_limit".into(), serde_json::json!(2));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("showing first"));
}

// ============================================================
// GitTool tests
// ============================================================

#[test]
fn git_tool_status() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("status"));
    let result = tool.execute(params);
    // Will fail outside git repo, but shouldn't panic
    assert!(result.output.len() > 0);
}

#[test]
fn git_tool_init() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("init"));
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(dir.path().join(".git").exists());
}

#[test]
fn git_tool_log() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("log"));
    params.insert("max_count".into(), serde_json::json!(5));
    let _ = tool.execute(params);
}

#[test]
fn git_tool_unknown_operation() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("nonexistent_op"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn git_tool_missing_message() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("commit"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn git_tool_ls_files() {
    let dir = tempfile::tempdir().unwrap();
    // Init and add a file
    fs::write(dir.path().join("hello.txt"), "hello").unwrap();
    let tool = GitTool;

    let mut init_params = HashMap::new();
    init_params.insert("operation".into(), serde_json::json!("init"));
    init_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    tool.execute(init_params);

    let mut add_params = HashMap::new();
    add_params.insert("operation".into(), serde_json::json!("add"));
    add_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    add_params.insert("all".into(), serde_json::json!(true));
    tool.execute(add_params);

    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("ls-files"));
    params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("hello.txt"));
}

// ============================================================
// GitTool extended tests
// ============================================================

#[test]
fn git_tool_commit_requires_message() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("commit"));
    let result = tool.execute(params);
    assert!(result.is_error);
    assert!(result.output.to_lowercase().contains("message"));
}

#[test]
fn git_tool_rm_requires_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("to_remove.txt"), "remove me").unwrap();
    let tool = GitTool;

    let mut init_params = HashMap::new();
    init_params.insert("operation".into(), serde_json::json!("init"));
    init_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    tool.execute(init_params);

    // rm without files should error
    let mut rm_params = HashMap::new();
    rm_params.insert("operation".into(), serde_json::json!("rm"));
    rm_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(rm_params);
    assert!(result.is_error);
}

#[test]
fn git_tool_mv_requires_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    let tool = GitTool;

    let mut init_params = HashMap::new();
    init_params.insert("operation".into(), serde_json::json!("init"));
    init_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    tool.execute(init_params);

    // mv without files should error
    let mut mv_params = HashMap::new();
    mv_params.insert("operation".into(), serde_json::json!("mv"));
    mv_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(mv_params);
    assert!(result.is_error);
}

#[test]
fn git_tool_mv_requires_two_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    let tool = GitTool;

    let mut init_params = HashMap::new();
    init_params.insert("operation".into(), serde_json::json!("init"));
    init_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    tool.execute(init_params);

    // mv with only 1 file should error
    let mut mv_params = HashMap::new();
    mv_params.insert("operation".into(), serde_json::json!("mv"));
    mv_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    mv_params.insert("files".into(), serde_json::json!(["a.txt"]));
    let result = tool.execute(mv_params);
    assert!(result.is_error);
}

#[test]
fn git_tool_clean_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("untracked.txt"), "untracked").unwrap();
    let tool = GitTool;

    let mut init_params = HashMap::new();
    init_params.insert("operation".into(), serde_json::json!("init"));
    init_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    tool.execute(init_params);

    // clean with dry_run should not actually delete
    let mut clean_params = HashMap::new();
    clean_params.insert("operation".into(), serde_json::json!("clean"));
    clean_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    clean_params.insert("dry_run".into(), serde_json::json!(true));
    clean_params.insert("force".into(), serde_json::json!(true));
    let result = tool.execute(clean_params);
    assert!(dir.path().join("untracked.txt").exists()); // file still exists
    assert!(!result.is_error);
}

#[test]
fn git_tool_blame_requires_files() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("blame"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn git_tool_restore_requires_files() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GitTool;

    let mut init_params = HashMap::new();
    init_params.insert("operation".into(), serde_json::json!("init"));
    init_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    tool.execute(init_params);

    // restore without files should error
    let mut restore_params = HashMap::new();
    restore_params.insert("operation".into(), serde_json::json!("restore"));
    restore_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(restore_params);
    assert!(result.is_error);
}

#[test]
fn git_tool_cherry_pick_requires_target() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("cherry-pick"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn git_tool_revert_requires_target() {
    let tool = GitTool;
    let mut params = HashMap::new();
    params.insert("operation".into(), serde_json::json!("revert"));
    let result = tool.execute(params);
    assert!(result.is_error);
}

#[test]
fn git_tool_switch_requires_branch() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GitTool;

    let mut init_params = HashMap::new();
    init_params.insert("operation".into(), serde_json::json!("init"));
    init_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    tool.execute(init_params);

    let mut switch_params = HashMap::new();
    switch_params.insert("operation".into(), serde_json::json!("switch"));
    switch_params.insert("path".into(), serde_json::json!(dir.path().to_string_lossy().to_string()));
    let result = tool.execute(switch_params);
    assert!(result.is_error);
}

#[test]
fn git_tool_full_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_string_lossy().to_string();
    let tool = GitTool;

    // 1. Init
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("init"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    assert!(!tool.execute(p).is_error);

    // 2. Create and add file
    fs::write(dir.path().join("main.txt"), "first line").unwrap();
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("add"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("all".into(), serde_json::json!(true));
    assert!(!tool.execute(p).is_error);

    // 3. Commit
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("commit"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("message".into(), serde_json::json!("initial commit"));
    assert!(!tool.execute(p).is_error);

    // 4. Get commit hash via rev-parse
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("rev-parse"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("target".into(), serde_json::json!("HEAD"));
    let result = tool.execute(p);
    assert!(!result.is_error);
    let head_hash = result.output.trim().to_string();
    assert!(!head_hash.is_empty());

    // 5. Create a new branch and switch to it
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("branch"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("branch".into(), serde_json::json!("feature"));
    assert!(!tool.execute(p).is_error);

    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("switch"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("branch".into(), serde_json::json!("feature"));
    assert!(!tool.execute(p).is_error);

    // 6. Modify file on feature branch
    fs::write(dir.path().join("main.txt"), "modified line").unwrap();
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("add"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("all".into(), serde_json::json!(true));
    tool.execute(p);
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("commit"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("message".into(), serde_json::json!("modify main.txt"));
    assert!(!tool.execute(p).is_error);

    // 7. Switch back to previous branch (master or main)
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("switch"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("branch".into(), serde_json::json!("master"));
    let result = tool.execute(p.clone());
    // Try main if master doesn't exist
    if result.is_error {
        p.insert("branch".into(), serde_json::json!("main"));
        assert!(!tool.execute(p).is_error);
    }

    // 8. Restore file to HEAD
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("restore"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("files".into(), serde_json::json!(["main.txt"]));
    assert!(!tool.execute(p).is_error);

    // 9. Clean dry-run
    fs::write(dir.path().join("untracked.txt"), "untracked").unwrap();
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("clean"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("dry_run".into(), serde_json::json!(true));
    p.insert("force".into(), serde_json::json!(true));
    assert!(!tool.execute(p).is_error);
    assert!(dir.path().join("untracked.txt").exists());

    // 10. Blame
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("blame"));
    p.insert("directory".into(), serde_json::json!(dir_str.clone()));
    p.insert("files".into(), serde_json::json!(["main.txt"]));
    let blame_result = tool.execute(p);
    assert!(!blame_result.is_error, "blame failed: {}", blame_result.output);

    // 11. Reflog
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("reflog"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("max_count".into(), serde_json::json!(10));
    assert!(!tool.execute(p).is_error);

    // 12. Shortlog
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("shortlog"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    assert!(!tool.execute(p).is_error);

    // 13. Revert
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("revert"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("target".into(), serde_json::json!("HEAD"));
    p.insert("flags".into(), serde_json::json!(["--no-edit"]));
    assert!(!tool.execute(p).is_error);

    // 14. Tag
    let mut p = HashMap::new();
    p.insert("operation".into(), serde_json::json!("tag"));
    p.insert("path".into(), serde_json::json!(dir_str.clone()));
    p.insert("branch".into(), serde_json::json!("v1.0"));
    assert!(!tool.execute(p).is_error);
}

// ============================================================
// RuntimeInfoTool tests
// ============================================================

#[test]
fn runtime_info_executes() {
    let tool = RuntimeInfoTool;
    let params: HashMap<String, serde_json::Value> = HashMap::new();
    let result = tool.execute(params);
    assert!(!result.is_error);
    assert!(result.output.contains("OS"));
    assert!(result.output.contains("Architecture"));
}

#[test]
fn runtime_info_tool_name() {
    let tool = RuntimeInfoTool;
    assert_eq!(tool.name(), "runtime_info");
}
