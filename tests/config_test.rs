//! Integration tests for config module

use miniclaudecode_rust::config::{Config, ClaudeSettings, McpSettings, McpServerConfig, McpConfigFile, McpConfigEntry};
use miniclaudecode_rust::permissions::PermissionMode;
use std::path::PathBuf;
use tempfile::TempDir;

// ─── Config defaults ───

#[test]
fn config_default() {
    let cfg = Config::default();
    assert_eq!(cfg.model, "claude-sonnet-4-20250514");
    assert!(cfg.api_key.is_none());
    assert!(cfg.base_url.is_none());
    assert_eq!(cfg.max_turns, 30);
    assert_eq!(cfg.max_context_msgs, 100);
    assert_eq!(cfg.permission_mode, PermissionMode::Ask);
    assert!(cfg.mcp_manager.is_none());
    assert!(cfg.skill_loader.is_none());
    assert!(cfg.auto_compact_enabled);
    assert_eq!(cfg.auto_compact_threshold, 0.75);
    assert_eq!(cfg.auto_compact_buffer, 13_000);
}

#[test]
fn config_new() {
    let cfg = Config::new();
    assert_eq!(cfg.max_turns, 30);
}

#[test]
fn config_allowed_commands_default() {
    let cfg = Config::default();
    assert!(!cfg.allowed_commands.is_empty());
    assert!(cfg.allowed_commands.contains(&"ls".to_string()));
    assert!(cfg.allowed_commands.contains(&"git status".to_string()));
}

#[test]
fn config_denied_patterns_default() {
    let cfg = Config::default();
    assert!(!cfg.denied_patterns.is_empty());
    assert!(cfg.denied_patterns.iter().any(|p| p.contains("rm -rf")));
}

#[test]
fn config_project_dir_default() {
    let cfg = Config::default();
    assert_eq!(cfg.project_dir, PathBuf::from("."));
}

// ─── Config modification ───

#[test]
fn config_custom_values() {
    let cfg = Config {
        model: "custom-model".to_string(),
        api_key: Some("sk-test".to_string()),
        base_url: Some("https://custom.api.com".to_string()),
        max_turns: 50,
        max_context_msgs: 200,
        permission_mode: PermissionMode::Auto,
        project_dir: PathBuf::from("/my/project"),
        allowed_commands: vec!["ls".to_string()],
        denied_patterns: vec![],
        mcp_manager: None,
        skill_loader: None,
        file_history: None,
        auto_compact_enabled: true,
        auto_compact_threshold: 0.8,
        auto_compact_buffer: 10_000,
        max_compact_output_tokens: 15_000,
    };
    assert_eq!(cfg.model, "custom-model");
    assert_eq!(cfg.api_key, Some("sk-test".to_string()));
    assert_eq!(cfg.max_turns, 50);
    assert_eq!(cfg.permission_mode, PermissionMode::Auto);
}

#[test]
fn config_clone() {
    let cfg = Config {
        api_key: Some("key".to_string()),
        ..Config::default()
    };
    let cloned = cfg.clone();
    assert_eq!(cloned.api_key, Some("key".to_string()));
}

#[test]
fn config_debug() {
    let cfg = Config::default();
    let debug = format!("{:?}", cfg);
    assert!(debug.contains("Config"));
}

// ─── ClaudeSettings deserialization ───

#[test]
fn claude_settings_deserialize_token() {
    let json = r#"{
        "env": {
            "ANTHROPIC_AUTH_TOKEN": "sk-test-token"
        }
    }"#;
    let settings: ClaudeSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.env.anthropic_auth_token, Some("sk-test-token".to_string()));
}

#[test]
fn claude_settings_deserialize_all_env() {
    let json = r#"{
        "env": {
            "ANTHROPIC_AUTH_TOKEN": "sk-123",
            "ANTHROPIC_BASE_URL": "https://custom.api.com",
            "ANTHROPIC_MODEL": "claude-opus-4-6",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-4-6",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-6",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-4-5",
            "ANTHROPIC_REASONING_MODEL": "claude-opus-4-6"
        }
    }"#;
    let settings: ClaudeSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.env.anthropic_auth_token, Some("sk-123".to_string()));
    assert_eq!(settings.env.anthropic_base_url, Some("https://custom.api.com".to_string()));
    assert_eq!(settings.env.anthropic_model, Some("claude-opus-4-6".to_string()));
}

#[test]
fn claude_settings_deserialize_partial_env() {
    let json = r#"{
        "env": {
            "ANTHROPIC_MODEL": "claude-sonnet-4-6"
        }
    }"#;
    let settings: ClaudeSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.env.anthropic_model, Some("claude-sonnet-4-6".to_string()));
    assert!(settings.env.anthropic_auth_token.is_none());
}

#[test]
fn claude_settings_deserialize_empty() {
    let json = r#"{
        "env": {}
    }"#;
    let settings: ClaudeSettings = serde_json::from_str(json).unwrap();
    assert!(settings.env.anthropic_auth_token.is_none());
}

// ─── McpSettings deserialization ───

#[test]
fn mcp_settings_default() {
    let settings = McpSettings::default();
    assert!(settings.servers.is_empty());
}

#[test]
fn mcp_settings_deserialize_with_servers() {
    let json = r#"{
        "env": {
            "ANTHROPIC_AUTH_TOKEN": "sk-test"
        },
        "mcp": {
            "servers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem"],
                    "env": {"HOME": "/tmp"}
                }
            }
        }
    }"#;
    let settings: ClaudeSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.mcp.servers.len(), 1);
    let server = &settings.mcp.servers["filesystem"];
    assert_eq!(server.command, Some("npx".to_string()));
    assert_eq!(server.args, Some(vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()]));
}

#[test]
fn mcp_server_config_deserialize() {
    let json = r#"{
        "command": "python",
        "args": ["server.py"],
        "env": {"KEY": "value"},
        "url": "https://example.com"
    }"#;
    let config: McpServerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.command, Some("python".to_string()));
    assert_eq!(config.args, Some(vec!["server.py".to_string()]));
    assert_eq!(config.env.as_ref().unwrap()["KEY"], "value");
    assert_eq!(config.url, Some("https://example.com".to_string()));
}

// ─── McpConfigFile deserialization ───

#[test]
fn mcp_config_file_deserialize() {
    let json = r#"{
        "mcpServers": {
            "server1": {
                "command": "node",
                "args": ["app.js"],
                "env": {"NODE_ENV": "test"},
                "url": "https://example.com"
            }
        }
    }"#;
    let config: McpConfigFile = serde_json::from_str(json).unwrap();
    assert_eq!(config.mcp_servers.len(), 1);
    let entry = &config.mcp_servers["server1"];
    assert_eq!(entry.command, Some("node".to_string()));
}

#[test]
fn mcp_config_entry_deserialize() {
    let json = r#"{"command":"echo","args":["hello"],"env":{"X":"1"},"url":"https://x.com"}"#;
    let entry: McpConfigEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.command, Some("echo".to_string()));
    assert_eq!(entry.args, Some(vec!["hello".to_string()]));
    assert_eq!(entry.url, Some("https://x.com".to_string()));
}

// ─── load_config_from_file ───

#[test]
fn load_config_no_settings_file() {
    let dir = TempDir::new().unwrap();
    let result = miniclaudecode_rust::config::load_config_from_file(dir.path());
    // No settings file = returns None
    assert!(result.is_none());
}

#[test]
fn load_config_with_settings_json() {
    let dir = TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir(&claude_dir).unwrap();

    let settings = r#"{
        "env": {
            "ANTHROPIC_AUTH_TOKEN": "sk-test-key",
            "ANTHROPIC_MODEL": "claude-sonnet-4-6"
        }
    }"#;
    std::fs::write(claude_dir.join("settings.json"), settings).unwrap();

    let result = miniclaudecode_rust::config::load_config_from_file(dir.path());
    assert!(result.is_some());
    let cfg = result.unwrap();
    assert_eq!(cfg.api_key, Some("sk-test-key".to_string()));
    assert_eq!(cfg.model, "claude-sonnet-4-6");
}

#[test]
fn load_config_with_mcp_servers() {
    let dir = TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir(&claude_dir).unwrap();

    let settings = r#"{
        "env": {
            "ANTHROPIC_AUTH_TOKEN": "sk-key"
        },
        "mcp": {
            "servers": {
                "test-server": {
                    "command": "echo",
                    "args": ["hello"]
                }
            }
        }
    }"#;
    std::fs::write(claude_dir.join("settings.json"), settings).unwrap();

    let result = miniclaudecode_rust::config::load_config_from_file(dir.path());
    assert!(result.is_some());
    let cfg = result.unwrap();
    let servers = cfg.mcp_manager.as_ref().unwrap().list_servers();
    assert_eq!(servers, vec!["test-server"]);
}

#[test]
fn load_config_with_mcp_json() {
    let dir = TempDir::new().unwrap();

    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.json"), r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-key"}}"#).unwrap();

    let mcp_json = r#"{
        "mcpServers": {
            "remote-server": {
                "url": "https://mcp.example.com/sse",
                "env": {"AUTH": "token"}
            }
        }
    }"#;
    std::fs::write(dir.path().join(".mcp.json"), mcp_json).unwrap();

    let result = miniclaudecode_rust::config::load_config_from_file(dir.path());
    assert!(result.is_some());
    let cfg = result.unwrap();
    let servers = cfg.mcp_manager.as_ref().unwrap().list_servers();
    assert!(servers.contains(&"remote-server".to_string()));
}

// ─── build_system_prompt ───

#[test]
fn build_system_prompt_contains_environment_info() {
    let registry = miniclaudecode_rust::tools::Registry::new();
    miniclaudecode_rust::tools::register_builtin_tools(&registry);

    let prompt = miniclaudecode_rust::config::build_system_prompt(
        &registry,
        &PermissionMode::Ask,
        std::path::Path::new("."),
        None,
        None,
    );

    assert!(prompt.contains("miniClaudeCode"));
    assert!(prompt.contains("Environment"));
    assert!(prompt.contains("Operating Rules"));
}

#[test]
fn build_system_prompt_contains_tools() {
    let registry = miniclaudecode_rust::tools::Registry::new();
    miniclaudecode_rust::tools::register_builtin_tools(&registry);

    let prompt = miniclaudecode_rust::config::build_system_prompt(
        &registry,
        &PermissionMode::Ask,
        std::path::Path::new("."),
        None,
        None,
    );

    assert!(prompt.contains("read_file"));
    assert!(prompt.contains("write_file"));
    assert!(prompt.contains("edit_file"));
}

#[test]
fn build_system_prompt_contains_permission_mode() {
    let registry = miniclaudecode_rust::tools::Registry::new();

    for (mode, desc_keyword) in [
        (PermissionMode::Ask, "ASK"),
        (PermissionMode::Auto, "AUTO"),
        (PermissionMode::Plan, "PLAN"),
    ] {
        let prompt = miniclaudecode_rust::config::build_system_prompt(
            &registry,
            &mode,
            std::path::Path::new("."),
            None,
            None,
        );
        assert!(prompt.contains(desc_keyword), "Should contain {} mode", desc_keyword);
    }
}

#[test]
fn build_system_prompt_empty_registry() {
    let registry = miniclaudecode_rust::tools::Registry::new();

    let prompt = miniclaudecode_rust::config::build_system_prompt(
        &registry,
        &PermissionMode::Ask,
        std::path::Path::new("."),
        None,
        None,
    );

    assert!(prompt.contains("miniClaudeCode"));
    assert!(prompt.contains("Environment"));
}

#[test]
fn build_system_prompt_with_claude_md() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "# My Project\n\nThis is a test project.").unwrap();

    let registry = miniclaudecode_rust::tools::Registry::new();

    let prompt = miniclaudecode_rust::config::build_system_prompt(
        &registry,
        &PermissionMode::Ask,
        dir.path(),
        None,
        None,
    );

    assert!(prompt.contains("CLAUDE.md"));
    assert!(prompt.contains("My Project"));
}
