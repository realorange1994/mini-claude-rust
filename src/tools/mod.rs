//! Tools module - implements all built-in tools

mod exec_tool;
mod file_read;
mod file_write;
mod file_edit;
mod multi_edit;
mod fileops;
mod glob_tool;
mod grep_tool;
mod list_dir;
mod git_tool;
mod system_tool;
mod process;
mod runtime_info;
mod terminal_tool;
mod web_search;
mod web_fetch;
mod exa_search;
mod mcp_tools;
mod skill_tools;
mod file_history_tools;

// Re-export tool structs for integration tests
pub use exec_tool::ExecTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use file_edit::FileEditTool;
pub use multi_edit::MultiEditTool;
pub use fileops::FileOpsTool;
pub use list_dir::ListDirTool;
pub use grep_tool::GrepTool;
pub use glob_tool::GlobTool;
pub use git_tool::GitTool;
pub use runtime_info::RuntimeInfoTool;
pub use web_search::WebSearchTool;

use crate::config::Config;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// ToolResult holds the output of a tool execution
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }
}

/// ValidateParams checks that required parameters are present.
pub fn validate_params(tool: &dyn Tool, params: &HashMap<String, serde_json::Value>) -> Option<ToolResult> {
    let schema = tool.input_schema();
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        for key in required {
            if let Some(key_str) = key.as_str() {
                if !params.contains_key(key_str) {
                    return Some(ToolResult::error(format!(
                        "Error: missing required parameter: \"{}\"", key_str
                    )));
                }
            }
        }
    }
    None
}

/// Tool is the interface all tools must implement
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Map<String, serde_json::Value>;
    fn check_permissions(&self, params: &HashMap<String, serde_json::Value>) -> Option<ToolResult>;
    fn execute(&self, params: HashMap<String, serde_json::Value>) -> ToolResult;
}

/// Registry collects tool instances and provides lookup
pub struct Registry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }

    pub fn register<T: Tool + 'static>(&self, tool: T) {
        let name = tool.name().to_string();
        let mut tools = self.tools.write().unwrap();
        tools.insert(name, Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let tools = self.tools.read().unwrap();
        tools.get(name).cloned()
    }

    pub fn all_tools(&self) -> Vec<Arc<dyn Tool>> {
        let tools = self.tools.read().unwrap();
        tools.values().cloned().collect()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register all built-in tools
pub fn register_builtin_tools(registry: &Registry) {
    registry.register(exec_tool::ExecTool);
    registry.register(file_read::FileReadTool);
    registry.register(file_write::FileWriteTool);
    registry.register(file_edit::FileEditTool);
    registry.register(multi_edit::MultiEditTool);
    registry.register(fileops::FileOpsTool);
    registry.register(glob_tool::GlobTool);
    registry.register(grep_tool::GrepTool);
    registry.register(list_dir::ListDirTool);
    registry.register(git_tool::GitTool);
    registry.register(system_tool::SystemTool);
    registry.register(process::ProcessTool);
    registry.register(runtime_info::RuntimeInfoTool);
    registry.register(terminal_tool::TerminalTool);
    registry.register(web_search::WebSearchTool);
    registry.register(web_fetch::WebFetchTool);
    registry.register(exa_search::ExaSearchTool);
}

/// Register MCP and skills tools
pub fn register_mcp_and_skills(registry: &Registry, cfg: &Config) {
    if let Some(mcp_manager) = &cfg.mcp_manager {
        let arc_manager = Arc::new(mcp_manager.clone());
        registry.register(mcp_tools::ListMcpTools::new(arc_manager.clone()));
        registry.register(mcp_tools::McpToolCaller::new(arc_manager.clone()));
        registry.register(mcp_tools::McpServerStatus::new(arc_manager));
    }

    if let Some(skill_loader) = &cfg.skill_loader {
        let arc_loader = Arc::new(skill_loader.clone());
        registry.register(skill_tools::ReadSkillTool::new(arc_loader.clone()));
        registry.register(skill_tools::ListSkillsTool::new(arc_loader));
    }

    if let Some(arc_history) = &cfg.file_history {
        registry.register(file_history_tools::FileHistoryTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryReadTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryGrepTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileRestoreTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileRewindTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryDiffTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistorySearchTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistorySummaryTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryTimelineTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryTagTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryAnnotateTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryBatchTool::new(arc_history.clone()));
        registry.register(file_history_tools::FileHistoryCheckoutTool::new(arc_history.clone()));
    }
}

// ─── Shared utility functions ───

/// Expand `~` to home directory and resolve relative paths.
/// Works on both Unix and Windows (HOME → USERPROFILE → HOMEDRIVE+HOMEPATH).
pub fn expand_path(p: &str) -> std::path::PathBuf {
    let p = if p.starts_with('~') {
        if let Ok(home) = std::env::var("HOME") {
            p.replacen('~', &home, 1)
        } else if let Ok(home) = std::env::var("USERPROFILE") {
            p.replacen('~', &home, 1)
        } else if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
            p.replacen('~', &format!("{}{}", drive, path), 1)
        } else {
            p.to_string()
        }
    } else {
        p.to_string()
    };

    let path = std::path::Path::new(&p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::Path::new(".").to_path_buf())
            .join(path)
    }
}

/// Check if a directory name should be ignored during traversal.
/// Common build artifacts, dependency directories, and cache directories.
pub fn is_ignored_dir(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        ".git" | "node_modules" | "__pycache__" | ".venv" | "venv"
            | "dist" | "build" | ".DS_Store" | ".tox"
            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache"
            | ".coverage" | "htmlcov" | "target"
    )
}

/// Strip HTML tags and decode common entities.
pub fn strip_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;

    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }

    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Check if a string contains internal/private network URLs or IPs.
/// Uses both string matching and regex for comprehensive detection.
pub fn contains_internal_url(s: &str) -> bool {
    use std::sync::OnceLock;
    use regex::Regex;

    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)(localhost|127\.0\.0\.1|0\.0\.0\.0|10\.\d{1,3}\.\d{1,3}\.\d{1,3}|172\.(1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}|192\.168\.\d{1,3}\.\d{1,3}|\[::1\]|\[::ffff:127\.0\.0\.1\]|0x7f000001|0177\.0\.0\.1)"
        ).unwrap()
    });

    // Also check for URL-encoded variants
    let decoded = s.replace("%61", "a")  // %61 = 'a'
        .replace("%41", "A");            // %41 = 'A'

    re.is_match(s) || re.is_match(&decoded)
}

/// Restore CRLF line endings in a string that was normalized to LF.
/// Uses O(n) algorithm instead of O(n²) with chars().nth().
pub fn restore_crlf(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + s.len() / 10);
    let mut prev_was_cr = false;
    for c in s.chars() {
        if c == '\n' && !prev_was_cr {
            result.push('\r');
        }
        prev_was_cr = c == '\r';
        result.push(c);
    }
    result
}

/// Safely truncate a string to at most `max` bytes without adding ellipsis.
pub fn truncate_at(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
