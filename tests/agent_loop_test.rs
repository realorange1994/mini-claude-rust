//! Integration tests for agent_loop module
//! Tests utility functions that don't require an actual API connection.

use miniclaudecode_rust::agent_loop;

// ─── is_transient_error ───

#[test]
fn is_transient_error_connection() {
    assert!(agent_loop::is_transient_error("connection refused"));
}

#[test]
fn is_transient_error_timeout() {
    assert!(agent_loop::is_transient_error("request timed out"));
}

#[test]
fn is_transient_error_timed_out() {
    assert!(agent_loop::is_transient_error("operation timed out"));
}

#[test]
fn is_transient_error_network() {
    assert!(agent_loop::is_transient_error("network unreachable"));
}

#[test]
fn is_transient_error_rate_limit() {
    assert!(agent_loop::is_transient_error("rate limit exceeded"));
}

#[test]
fn is_transient_error_429() {
    assert!(agent_loop::is_transient_error("HTTP 429 Too Many Requests"));
}

#[test]
fn is_transient_error_500() {
    assert!(agent_loop::is_transient_error("HTTP 500 Internal Server Error"));
}

#[test]
fn is_transient_error_502() {
    assert!(agent_loop::is_transient_error("HTTP 502 Bad Gateway"));
}

#[test]
fn is_transient_error_503() {
    assert!(agent_loop::is_transient_error("HTTP 503 Service Unavailable"));
}

#[test]
fn is_transient_error_504() {
    assert!(agent_loop::is_transient_error("HTTP 504 Gateway Timeout"));
}

#[test]
fn is_transient_error_upstream() {
    assert!(agent_loop::is_transient_error("upstream connection reset"));
}

#[test]
fn is_transient_error_reset() {
    assert!(agent_loop::is_transient_error("connection reset by peer"));
}

#[test]
fn is_transient_error_broken_pipe() {
    assert!(agent_loop::is_transient_error("broken pipe"));
}

#[test]
fn is_transient_error_temporary() {
    assert!(agent_loop::is_transient_error("temporary failure"));
}

#[test]
fn is_transient_error_transient() {
    assert!(agent_loop::is_transient_error("transient error occurred"));
}

#[test]
fn is_not_transient_error_api_error() {
    assert!(!agent_loop::is_transient_error("API error: invalid request"));
}

#[test]
fn is_not_transient_error_not_found() {
    assert!(!agent_loop::is_transient_error("tool not found: xyz"));
}

#[test]
fn is_not_transient_error_auth() {
    assert!(!agent_loop::is_transient_error("authentication failed"));
}

#[test]
fn is_not_transient_error_empty() {
    assert!(!agent_loop::is_transient_error(""));
}

#[test]
fn is_transient_error_case_insensitive() {
    assert!(agent_loop::is_transient_error("CONNECTION REFUSED"));
    assert!(agent_loop::is_transient_error("Connection Refused"));
}

// ─── limit_str ───

#[test]
fn limit_str_shorter_than_max() {
    assert_eq!(agent_loop::limit_str("hello", 10), "hello");
}

#[test]
fn limit_str_exact_length() {
    assert_eq!(agent_loop::limit_str("hello", 5), "hello");
}

#[test]
fn limit_str_truncated() {
    let result = agent_loop::limit_str("hello world", 5);
    assert_eq!(result, "hello...");
}

#[test]
fn limit_str_zero_max() {
    let result = agent_loop::limit_str("hello", 0);
    assert_eq!(result, "...");
}

// ─── tool_arg_summary ───

#[test]
fn tool_arg_summary_read_file() {
    assert_eq!(
        agent_loop::tool_arg_summary("read_file", r#"{"path": "test.txt"}"#),
        "test.txt"
    );
}

#[test]
fn tool_arg_summary_write_file() {
    assert_eq!(
        agent_loop::tool_arg_summary("write_file", r#"{"path": "/tmp/out.txt", "content": "hello"}"#),
        "/tmp/out.txt"
    );
}

#[test]
fn tool_arg_summary_edit_file() {
    assert_eq!(
        agent_loop::tool_arg_summary("edit_file", r#"{"path": "src/main.rs"}"#),
        "src/main.rs"
    );
}

#[test]
fn tool_arg_summary_multi_edit() {
    assert_eq!(
        agent_loop::tool_arg_summary("multi_edit", r#"{"path": "config.yaml"}"#),
        "config.yaml"
    );
}

#[test]
fn tool_arg_summary_fileops() {
    assert_eq!(
        agent_loop::tool_arg_summary("fileops", r#"{"path": "/tmp", "operation": "mkdir"}"#),
        "/tmp"
    );
}

#[test]
fn tool_arg_summary_list_dir_with_path() {
    assert_eq!(
        agent_loop::tool_arg_summary("list_dir", r#"{"path": "/home"}"#),
        "/home"
    );
}

#[test]
fn tool_arg_summary_list_dir_default() {
    assert_eq!(
        agent_loop::tool_arg_summary("list_dir", "{}"),
        "."
    );
}

#[test]
fn tool_arg_summary_exec_command() {
    assert_eq!(
        agent_loop::tool_arg_summary("exec", r#"{"command": "ls -la"}"#),
        "ls -la"
    );
}

#[test]
fn tool_arg_summary_exec_long_command_truncated() {
    let long_cmd = "a".repeat(200);
    let result = agent_loop::tool_arg_summary("exec", &format!(r#"{{"command": "{}"}}"#, long_cmd));
    assert!(result.len() <= 123); // 120 + "..."
    assert!(result.ends_with("..."));
}

#[test]
fn tool_arg_summary_terminal_command() {
    assert_eq!(
        agent_loop::tool_arg_summary("terminal", r#"{"command": "top"}"#),
        "top"
    );
}

#[test]
fn tool_arg_summary_grep_with_pattern() {
    assert_eq!(
        agent_loop::tool_arg_summary("grep", r#"{"pattern": "TODO"}"#),
        "TODO"
    );
}

#[test]
fn tool_arg_summary_grep_with_pattern_and_path() {
    assert_eq!(
        agent_loop::tool_arg_summary("grep", r#"{"pattern": "TODO", "path": "src/"}"#),
        "\"TODO\" in src/"
    );
}

#[test]
fn tool_arg_summary_glob() {
    assert_eq!(
        agent_loop::tool_arg_summary("glob", r#"{"pattern": "*.rs"}"#),
        "*.rs"
    );
}

#[test]
fn tool_arg_summary_system() {
    assert_eq!(
        agent_loop::tool_arg_summary("system", r#"{"operation": "info"}"#),
        "info"
    );
}

#[test]
fn tool_arg_summary_git() {
    assert_eq!(
        agent_loop::tool_arg_summary("git", r#"{"args": "status"}"#),
        "git status"
    );
}

#[test]
fn tool_arg_summary_process_by_name() {
    assert_eq!(
        agent_loop::tool_arg_summary("process", r#"{"name": "chrome"}"#),
        "name=chrome"
    );
}

#[test]
fn tool_arg_summary_process_by_pid() {
    assert_eq!(
        agent_loop::tool_arg_summary("process", r#"{"pid": 1234}"#),
        "PID 1234"
    );
}

#[test]
fn tool_arg_summary_web_search() {
    assert_eq!(
        agent_loop::tool_arg_summary("web_search", r#"{"query": "rust async"}"#),
        "rust async"
    );
}

#[test]
fn tool_arg_summary_exa_search() {
    assert_eq!(
        agent_loop::tool_arg_summary("exa_search", r#"{"query": "latest news"}"#),
        "latest news"
    );
}

#[test]
fn tool_arg_summary_web_fetch() {
    assert_eq!(
        agent_loop::tool_arg_summary("web_fetch", r#"{"url": "https://example.com"}"#),
        "https://example.com"
    );
}

#[test]
fn tool_arg_summary_runtime_info() {
    let result = agent_loop::tool_arg_summary("runtime_info", "{}");
    assert_eq!(result, "");
}

#[test]
fn tool_arg_summary_unknown_tool() {
    assert_eq!(
        agent_loop::tool_arg_summary("unknown_tool", "{}"),
        ""
    );
}

#[test]
fn tool_arg_summary_unknown_tool_with_params() {
    let result = agent_loop::tool_arg_summary("custom_tool", r#"{"arg1": "value1", "arg2": 42, "flag": true}"#);
    // HashMap order is not guaranteed, check all parts are present
    assert!(result.contains("arg1=value1"));
    assert!(result.contains("arg2=42"));
    assert!(result.contains("flag=true"));
}

#[test]
fn tool_arg_summary_invalid_json() {
    assert_eq!(
        agent_loop::tool_arg_summary("read_file", "not json"),
        ""
    );
}

// ─── tool_result_preview ───

#[test]
fn tool_result_preview_exec_with_output() {
    let result = agent_loop::tool_result_preview("exec", "file1.txt\nfile2.txt\nfile3.txt");
    assert!(!result.is_empty());
}

#[test]
fn tool_result_preview_exec_empty_output() {
    let result = agent_loop::tool_result_preview("exec", "");
    assert_eq!(result, "(no output)");
}

#[test]
fn tool_result_preview_exec_with_headers() {
    let output = "STDOUT:\nhello world\nSTDERR:\nsome warning";
    let result = agent_loop::tool_result_preview("exec", output);
    assert_eq!(result, "hello world");
}

#[test]
fn tool_result_preview_read_file() {
    let result = agent_loop::tool_result_preview("read_file", "File: test.txt\nLine 1: hello");
    assert!(result.starts_with("File:"));
}

#[test]
fn tool_result_preview_list_dir() {
    let result = agent_loop::tool_result_preview("list_dir", "dir1/\ndir2/\nfile.txt");
    assert!(!result.is_empty());
}

#[test]
fn tool_result_preview_write_file() {
    let result = agent_loop::tool_result_preview("write_file", "Written: /tmp/test.txt");
    assert!(result.contains("/tmp/test.txt"));
}

#[test]
fn tool_result_preview_edit_file() {
    let result = agent_loop::tool_result_preview("edit_file", "Edited: src/main.rs:42");
    assert!(result.contains("src/main.rs"));
}

#[test]
fn tool_result_preview_unknown_tool() {
    assert_eq!(
        agent_loop::tool_result_preview("unknown_tool", "some output"),
        "some output"
    );
}

#[test]
fn tool_result_preview_unknown_tool_long() {
    let long_output = "a".repeat(200);
    let result = agent_loop::tool_result_preview("unknown_tool", &long_output);
    assert!(result.ends_with("..."));
}
