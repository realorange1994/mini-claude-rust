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
}
