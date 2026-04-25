//! Integration tests for MCP module

use miniclaudecode_rust::mcp::{Manager, Tool};
use std::collections::HashMap;

// ─── Manager tests ───

#[test]
fn manager_new_empty() {
    let manager = Manager::new();
    assert!(manager.list_servers().is_empty());
    assert!(manager.list_tools().is_empty());
}

#[test]
fn manager_register_server() {
    let manager = Manager::new();
    manager.register("test-server", "echo", &["".to_string()], HashMap::new());
    assert_eq!(manager.list_servers(), vec!["test-server"]);
}

#[test]
fn manager_register_multiple() {
    let manager = Manager::new();
    manager.register("server-a", "echo", &["a".to_string()], HashMap::new());
    manager.register("server-b", "echo", &["b".to_string()], HashMap::new());
    let servers = manager.list_servers();
    assert_eq!(servers.len(), 2);
    assert!(servers.contains(&"server-a".to_string()));
    assert!(servers.contains(&"server-b".to_string()));
}

#[test]
fn manager_get_server_status_unknown() {
    let manager = Manager::new();
    assert_eq!(manager.get_server_status("unknown"), "not found");
}

#[test]
fn manager_get_server_status_not_connected() {
    let manager = Manager::new();
    manager.register("my-server", "echo", &["".to_string()], HashMap::new());
    // Server registered but not started, so no tools discovered
    assert_eq!(manager.get_server_status("my-server"), "disconnected");
}

#[test]
fn manager_clone() {
    let manager = Manager::new();
    manager.register("s1", "echo", &["".to_string()], HashMap::new());
    let cloned = manager.clone();
    assert_eq!(cloned.list_servers(), vec!["s1"]);
}

#[test]
fn manager_default() {
    let manager = Manager::default();
    assert!(manager.list_servers().is_empty());
}

#[test]
fn manager_debug() {
    let manager = Manager::new();
    manager.register("srv", "echo", &["".to_string()], HashMap::new());
    let debug_str = format!("{:?}", manager);
    assert!(debug_str.contains("srv"));
}

// ─── Tool struct tests ───

#[test]
fn tool_serialization() {
    let tool = Tool {
        name: "my_tool".to_string(),
        description: "Does something useful".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "arg": { "type": "string" }
            }
        }),
    };
    assert_eq!(tool.name, "my_tool");
    assert_eq!(tool.description, "Does something useful");
}

#[test]
fn tool_default_input_schema() {
    let tool = Tool {
        name: "simple".to_string(),
        description: "No args".to_string(),
        input_schema: serde_json::Value::Null,
    };
    assert!(tool.input_schema.is_null());
}

// ─── parse_tool_result tests ───

#[test]
fn parse_tool_result_text_content() {
    let json = serde_json::json!({
        "content": [
            { "type": "text", "text": "Hello world" }
        ],
        "isError": false
    });
    let result = parse_tool_result(&json).unwrap();
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 1);
    assert_eq!(get_text(&result.content[0]), "Hello world");
}

#[test]
fn parse_tool_result_error() {
    let json = serde_json::json!({
        "content": [
            { "type": "text", "text": "Something went wrong" }
        ],
        "isError": true
    });
    let result = parse_tool_result(&json).unwrap();
    assert!(result.is_error);
    assert_eq!(get_text(&result.content[0]), "Something went wrong");
}

#[test]
fn parse_tool_result_missing_content() {
    let json = serde_json::json!({
        "isError": false
    });
    let result = parse_tool_result(&json);
    assert!(result.is_none());
}

#[test]
fn parse_tool_result_empty_content() {
    let json = serde_json::json!({
        "content": [],
        "isError": false
    });
    let result = parse_tool_result(&json).unwrap();
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 0);
}

#[test]
fn parse_tool_result_multiple_text_blocks() {
    let json = serde_json::json!({
        "content": [
            { "type": "text", "text": "First" },
            { "type": "text", "text": "Second" }
        ],
        "isError": false
    });
    let result = parse_tool_result(&json).unwrap();
    assert_eq!(result.content.len(), 2);
    assert_eq!(get_text(&result.content[0]), "First");
    assert_eq!(get_text(&result.content[1]), "Second");
}

#[test]
fn parse_tool_result_non_text_ignored() {
    let json = serde_json::json!({
        "content": [
            { "type": "text", "text": "Valid" },
            { "type": "image", "data": "base64..." }
        ],
        "isError": false
    });
    let result = parse_tool_result(&json).unwrap();
    assert_eq!(result.content.len(), 1);
    assert_eq!(get_text(&result.content[0]), "Valid");
}

#[test]
fn parse_tool_result_missing_is_error_defaults_false() {
    let json = serde_json::json!({
        "content": [
            { "type": "text", "text": "OK" }
        ]
    });
    let result = parse_tool_result(&json).unwrap();
    assert!(!result.is_error);
}

#[test]
fn parse_tool_result_text_missing_text_field() {
    let json = serde_json::json!({
        "content": [
            { "type": "text" }
        ]
    });
    let result = parse_tool_result(&json).unwrap();
    assert_eq!(result.content.len(), 0);
}

// Need to re-export parse_tool_result for testing
use miniclaudecode_rust::mcp::parse_tool_result;
use miniclaudecode_rust::mcp::ToolResultContent;

fn get_text(block: &ToolResultContent) -> String {
    match block {
        ToolResultContent::Text { text } => text.clone(),
    }
}
