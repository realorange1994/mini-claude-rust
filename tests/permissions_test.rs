//! Integration tests for permissions module
//! Note: Tests that would trigger interactive prompts (ask_user) are excluded
//! as they cannot run in automated test environments.
//!
//! The check flow in Ask mode is:
//! 1. tool.check_permissions() -> if warning, ask_user (BLOCKS)
//! 2. check_denied_patterns() -> if match, deny immediately
//! 3. is_dangerous_tool -> ask_user (BLOCKS)
//!
//! So we can only test:
//! - Auto mode (all allowed)
//! - Plan mode (read-only allowed, write blocked)
//! - Tool's check_permissions() directly (without PermissionGate)
//! - Denied patterns in scenarios where tool.check_permissions() returns None

use miniclaudecode_rust::config::Config;
use miniclaudecode_rust::permissions::{PermissionGate, PermissionMode};
use miniclaudecode_rust::tools::{Registry, ExecTool, FileWriteTool, FileEditTool, FileReadTool, ListDirTool, GrepTool, GlobTool, GitTool, RuntimeInfoTool, WebSearchTool, Tool};
use std::collections::HashMap;

// ─── PermissionMode ───

#[test]
fn permission_mode_from_str_ask() {
    assert_eq!(PermissionMode::from_str("ask"), PermissionMode::Ask);
}

#[test]
fn permission_mode_from_str_auto() {
    assert_eq!(PermissionMode::from_str("auto"), PermissionMode::Auto);
}

#[test]
fn permission_mode_from_str_plan() {
    assert_eq!(PermissionMode::from_str("plan"), PermissionMode::Plan);
}

#[test]
fn permission_mode_from_str_case_insensitive() {
    assert_eq!(PermissionMode::from_str("ASK"), PermissionMode::Ask);
    assert_eq!(PermissionMode::from_str("Auto"), PermissionMode::Auto);
    assert_eq!(PermissionMode::from_str("PLAN"), PermissionMode::Plan);
}

#[test]
fn permission_mode_from_str_unknown_defaults_to_ask() {
    assert_eq!(PermissionMode::from_str("unknown"), PermissionMode::Ask);
    assert_eq!(PermissionMode::from_str(""), PermissionMode::Ask);
}

#[test]
fn permission_mode_as_str() {
    assert_eq!(PermissionMode::Ask.as_str(), "ask");
    assert_eq!(PermissionMode::Auto.as_str(), "auto");
    assert_eq!(PermissionMode::Plan.as_str(), "plan");
}

#[test]
fn permission_mode_display() {
    assert_eq!(format!("{}", PermissionMode::Ask), "ask");
    assert_eq!(format!("{}", PermissionMode::Auto), "auto");
    assert_eq!(format!("{}", PermissionMode::Plan), "plan");
}

#[test]
fn permission_mode_from_str_trait() {
    let mode: PermissionMode = "auto".parse().unwrap();
    assert_eq!(mode, PermissionMode::Auto);
}

// ─── PermissionGate: Auto mode ───

#[test]
fn permission_gate_auto_allows_everything() {
    let mut config = Config::default();
    config.permission_mode = PermissionMode::Auto;
    let gate = PermissionGate::new(config);

    let registry = Registry::new();
    registry.register(ExecTool::new());
    registry.register(FileWriteTool);

    let tools = registry.all_tools();

    for tool in tools {
        let mut params = HashMap::new();
        params.insert("path".to_string(), serde_json::json!("/tmp/test.txt"));
        let result = gate.check(tool.as_ref(), params);
        // In auto mode, all tools should be allowed (None = allowed)
        assert!(result.is_none(), "{} should be allowed in auto mode", tool.name());
    }
}

// ─── PermissionGate: Plan mode ───

#[test]
fn permission_gate_plan_allows_read_only() {
    let mut config = Config::default();
    config.permission_mode = PermissionMode::Plan;
    let gate = PermissionGate::new(config);

    let registry = Registry::new();
    registry.register(FileReadTool);
    registry.register(ListDirTool);
    registry.register(GrepTool);
    registry.register(GlobTool);
    registry.register(GitTool);
    registry.register(WebSearchTool);
    registry.register(RuntimeInfoTool);

    let tools = registry.all_tools();

    for tool in tools {
        let params = HashMap::new();
        let result = gate.check(tool.as_ref(), params);
        assert!(result.is_none(), "{} should be allowed in plan mode", tool.name());
    }
}

#[test]
fn permission_gate_plan_blocks_write() {
    let mut config = Config::default();
    config.permission_mode = PermissionMode::Plan;
    let gate = PermissionGate::new(config);

    let registry = Registry::new();
    registry.register(FileWriteTool);
    registry.register(FileEditTool);
    registry.register(ExecTool::new());

    let tools = registry.all_tools();

    for tool in tools {
        let mut params = HashMap::new();
        if tool.name() == "write_file" || tool.name() == "edit_file" {
            params.insert("path".to_string(), serde_json::json!("/tmp/test.txt"));
        } else if tool.name() == "exec" {
            params.insert("command".to_string(), serde_json::json!("ls"));
        }
        let result = gate.check(tool.as_ref(), params);
        assert!(result.is_some(), "{} should be blocked in plan mode", tool.name());
        assert!(result.unwrap().output.contains("PLAN mode"));
    }
}

// ─── ExecTool check_permissions (direct test, no PermissionGate) ───

#[test]
fn exec_tool_check_permissions_dangerous_rm() {
    let tool = ExecTool::new();
    let mut params = HashMap::new();
    params.insert("command".to_string(), serde_json::json!("rm -rf /home"));
    let result = tool.check_permissions(&params);
    assert!(result.is_some());
    let output = &result.unwrap().output;
    assert!(
        output.to_lowercase().contains("dangerous") ||
        output.to_lowercase().contains("destructive") ||
        output.to_lowercase().contains("home"),
        "Expected dangerous/destructive/home error, got: {}", output
    );
}

#[test]
fn exec_tool_check_permissions_git_destruction() {
    let tool = ExecTool::new();
    let mut params = HashMap::new();
    params.insert("command".to_string(), serde_json::json!("rm -rf .git"));
    let result = tool.check_permissions(&params);
    assert!(result.is_some());
    let output = result.unwrap().output;
    // The error could come from destructive detection or .git regex
    assert!(
        output.contains(".git") || output.contains("git directory") ||
        output.to_lowercase().contains("destructive") || output.to_lowercase().contains("dangerous"),
        "Expected .git/destructive/dangerous error, got: {}", output
    );
}

#[test]
fn exec_tool_check_permissions_shutdown() {
    let tool = ExecTool::new();
    let mut params = HashMap::new();
    params.insert("command".to_string(), serde_json::json!("shutdown now"));
    let result = tool.check_permissions(&params);
    assert!(result.is_some());
    let output = &result.unwrap().output;
    assert!(
        output.to_lowercase().contains("dangerous") || output.to_lowercase().contains("destructive") || output.to_lowercase().contains("shutdown"),
        "Expected dangerous/destructive/shutdown error, got: {}", output
    );
}

#[test]
fn exec_tool_check_permissions_safe_command() {
    let tool = ExecTool::new();
    let mut params = HashMap::new();
    params.insert("command".to_string(), serde_json::json!("ls -la"));
    let result = tool.check_permissions(&params);
    // Safe command should return None (no warning)
    assert!(result.is_none());
}

#[test]
fn exec_tool_check_permissions_git_status() {
    let tool = ExecTool::new();
    let mut params = HashMap::new();
    params.insert("command".to_string(), serde_json::json!("git status"));
    let result = tool.check_permissions(&params);
    assert!(result.is_none());
}

#[test]
fn exec_tool_check_permissions_echo() {
    let tool = ExecTool::new();
    let mut params = HashMap::new();
    params.insert("command".to_string(), serde_json::json!("echo hello"));
    let result = tool.check_permissions(&params);
    assert!(result.is_none());
}

// ─── PermissionGate clone and mode methods ───

#[test]
fn permission_gate_clone() {
    let config = Config::default();
    let gate = PermissionGate::new(config);
    let cloned = gate.clone();
    assert_eq!(gate.mode(), cloned.mode());
}

#[test]
fn permission_gate_mode() {
    let mut config = Config::default();
    config.permission_mode = PermissionMode::Auto;
    let gate = PermissionGate::new(config);
    assert_eq!(gate.mode(), PermissionMode::Auto);
}

#[test]
fn permission_gate_set_mode() {
    let mut config = Config::default();
    config.permission_mode = PermissionMode::Ask;
    let mut gate = PermissionGate::new(config);
    assert_eq!(gate.mode(), PermissionMode::Ask);

    gate.set_mode(PermissionMode::Auto);
    assert_eq!(gate.mode(), PermissionMode::Auto);
}

// ─── PermissionMode serialization ───

#[test]
fn permission_mode_serialize() {
    let mode = PermissionMode::Ask;
    let json = serde_json::to_string(&mode).unwrap();
    assert_eq!(json, "\"ask\"");
}

#[test]
fn permission_mode_deserialize() {
    let mode: PermissionMode = serde_json::from_str("\"auto\"").unwrap();
    assert_eq!(mode, PermissionMode::Auto);
}

#[test]
fn permission_mode_serialize_deserialize_roundtrip() {
    for mode in [PermissionMode::Ask, PermissionMode::Auto, PermissionMode::Plan] {
        let json = serde_json::to_string(&mode).unwrap();
        let restored: PermissionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, restored);
    }
}

// ─── PermissionGate: Auto mode with various commands ───

#[test]
fn permission_gate_auto_allows_dangerous_commands() {
    // In Auto mode, even dangerous commands are allowed
    // (the gate doesn't check, tool's check_permissions is not called by gate)
    let mut config = Config::default();
    config.permission_mode = PermissionMode::Auto;
    let gate = PermissionGate::new(config);

    let registry = Registry::new();
    registry.register(ExecTool::new());
    let tool = registry.get("exec").unwrap();

    let mut params = HashMap::new();
    params.insert("command".to_string(), serde_json::json!("rm -rf /"));
    let result = gate.check(tool.as_ref(), params);
    // In auto mode, all tools should be allowed
    assert!(result.is_none());
}