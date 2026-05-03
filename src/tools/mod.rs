//! Tools module - implements all built-in tools

pub mod coercion;

mod exec_tool;
mod file_read;
mod file_write;
mod file_edit;
mod multi_edit;
mod fileops;
mod glob_tool;
mod grep_tool;
mod list_dir;
pub mod git_tool;
mod system_tool;
mod process;
mod runtime_info;
mod terminal_tool;
mod web_search;
mod web_fetch;
mod exa_search;
mod mcp_tools;
pub mod skill_tools;
pub mod file_history_tools;
pub mod memory_tool;
pub mod task_tool;
pub mod agent_tool;
mod brief_tool;
pub mod tool_search_tool;
pub mod agent_store;
pub mod agent_tools;
pub mod todo_write;
pub mod send_message;
pub mod ask_user_question;

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
pub use git_tool::{find_git_root, get_branch, is_bare_repo, is_git_repo, get_git_status, has_uncommitted_changes, get_default_branch, get_current_commit_hash, is_dirty, get_git_context, get_git_context_for_prompt};
pub use runtime_info::RuntimeInfoTool;
pub use web_search::WebSearchTool;

use crate::config::Config;
use serde_json;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// ToolResult holds the output of a tool execution with structured metadata
#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    /// Structured metadata for compaction summaries
    pub metadata: ToolResultMetadata,
}

/// Metadata about a tool execution, used for generating high-quality
/// compaction summaries (e.g., `[exec] ran "npm test" -> exit 0, 47 lines, 1.2s`)
#[derive(Debug, Clone, Default)]
pub struct ToolResultMetadata {
    /// Name of the tool that was executed
    pub tool_name: String,
    /// Exit code (for exec/shell tools)
    pub exit_code: Option<i32>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Number of output lines (for truncation summary)
    pub output_lines: usize,
    /// Whether the output was truncated
    pub truncated: bool,
}

impl ToolResultMetadata {
    /// Generate a one-line summary for compaction
    pub fn to_compact_summary(&self, output: &str) -> String {
        let status = if self.exit_code.map_or(false, |c| c != 0) {
            "error"
        } else if self.is_error_from_output(output) {
            "error"
        } else {
            "ok"
        };

        let line_count = if self.output_lines > 0 {
            self.output_lines
        } else {
            output.lines().count()
        };

        let duration_str = if self.duration_ms >= 1000 {
            format!("{:.1}s", self.duration_ms as f64 / 1000.0)
        } else if self.duration_ms > 0 {
            format!("{}ms", self.duration_ms)
        } else {
            String::new()
        };

        let duration_part = if duration_str.is_empty() { String::new() } else { format!(", {}", duration_str) };

        if self.tool_name.is_empty() {
            format!("-> {}, {} lines{}", status, line_count, duration_part)
        } else {
            format!("[{}] -> {}, {} lines{}", self.tool_name, status, line_count, duration_part)
        }
    }

    fn is_error_from_output(&self, output: &str) -> bool {
        output.contains("Error:") || output.contains("error:") || output.contains("FAILED")
    }
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
            metadata: ToolResultMetadata::default(),
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
            metadata: ToolResultMetadata::default(),
        }
    }

    /// Create a ToolResult with metadata
    pub fn with_metadata(output: impl Into<String>, is_error: bool, metadata: ToolResultMetadata) -> Self {
        Self {
            output: output.into(),
            is_error,
            metadata,
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

/// FileReadInfo tracks both the file's mtime (for staleness checks) and when it was read (for recency sorting).
#[derive(Clone)]
struct FileReadInfo {
    mtime: SystemTime,
    read_time: SystemTime,
}

/// Registry collects tool instances and provides lookup
pub struct Registry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
    /// Tracks which files have been read by read_file, their mtime at read time,
    /// and the read timestamp (for get_recently_read_files / post-compact recovery)
    files_read: Arc<RwLock<HashMap<String, FileReadInfo>>>,
    /// Shared tools list for ToolSearchTool (populated after registration is complete)
    tools_list: RwLock<Option<Arc<RwLock<Vec<Arc<dyn Tool>>>>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            files_read: Arc::new(RwLock::new(HashMap::new())),
            tools_list: RwLock::new(None),
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

    /// Register a tool directly from an Arc (used by sub-agent registry filtering)
    pub fn register_tool_from_arc(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        let mut tools = self.tools.write().unwrap();
        tools.insert(name, tool);
    }

    /// Set the shared tools list for ToolSearchTool.
    /// Called during registration of ToolSearchTool.
    pub fn set_tools_list(&self, list: Arc<RwLock<Vec<Arc<dyn Tool>>>>) {
        let mut guard = self.tools_list.write().unwrap();
        *guard = Some(list);
    }

    /// Populate the ToolSearchTool's shared tools list with all currently
    /// registered tools. Call this AFTER all tools are registered.
    /// For sub-agents with a filtered registry, also call this to make
    /// ToolSearchTool return only the filtered set.
    pub fn finalize_tool_search(&self) {
        let list_guard = self.tools_list.read().unwrap();
        if let Some(list) = list_guard.as_ref() {
            let tools = self.all_tools();
            *list.write().unwrap() = tools;
        }
    }

    /// Clone the registry for use in sub-agent spawning.
    /// Creates a new Registry and copies all tools from this one.
    /// The cloned ToolSearchTool will use a fresh tools list reflecting the child's tools.
    pub fn clone_for_spawn(&self) -> Registry {
        let child = Registry::new();
        // Copy the tools_list Arc reference so the cloned ToolSearchTool
        // in the child shares the same list as ToolSearchTool in the parent.
        {
            let parent_list = self.tools_list.read().unwrap();
            let mut child_list = child.tools_list.write().unwrap();
            *child_list = parent_list.clone();
        }
        // Register all tools from parent into child
        for tool in self.all_tools() {
            child.register_tool_from_arc(tool);
        }
        // Populate the shared list with the child's tools
        child.finalize_tool_search();
        child
    }

    /// Mark a file as having been read, storing its current mtime and the read timestamp
    pub fn mark_file_read(&self, path: &str) {
        let normalized = normalize_file_path(path);
        let mtime = std::fs::metadata(expand_path(path))
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let read_time = SystemTime::now();
        self.files_read.write().unwrap().insert(normalized, FileReadInfo { mtime, read_time });
    }

    /// Check if a file has been read before and hasn't been modified since
    /// Returns Ok(()) if safe to edit, or Err(error_message) if not
    pub fn check_file_stale(&self, path: &str) -> Result<(), String> {
        let normalized = normalize_file_path(path);
        let fp = expand_path(path);

        // New file creation: file doesn't exist yet, allow without read
        if !fp.exists() {
            return Ok(());
        }

        let guard = self.files_read.read().unwrap();
        let stored_info = guard.get(&normalized).cloned();
        drop(guard);

        let stored = stored_info.ok_or("Error: file has not been read yet. Read it first with read_file before editing.".to_string())?;

        match std::fs::metadata(&fp) {
            Ok(meta) => {
                let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if current_mtime == stored.mtime {
                    Ok(())
                } else {
                    Err("Error: file has been modified since read, either by the user or by a linter. Read it again before attempting to write it.".to_string())
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File was deleted -- not a staleness issue for new file creation
                Ok(())
            }
            Err(e) => {
                Err(format!("Error: cannot check file status: {}", e))
            }
        }
    }

    /// Clear the read-file tracking (e.g., on /clear)
    pub fn clear_files_read(&self) {
        self.files_read.write().unwrap().clear();
    }

    /// Returns paths of recently read files, sorted by most recently read first.
    /// Returns up to max_files paths. Used by post-compact recovery to re-inject
    /// file content after compaction.
    pub fn get_recently_read_files(&self, max_files: usize) -> Vec<String> {
        let guard = self.files_read.read().unwrap();
        let mut entries: Vec<_> = guard.iter()
            .map(|(path, info)| (path.clone(), info.read_time))
            .collect();
        drop(guard);

        // Sort by read_time descending (most recent first)
        entries.sort_by(|a, b| b.1.cmp(&a.1));

        entries.into_iter()
            .take(max_files)
            .map(|(path, _)| path)
            .collect()
    }
}

/// Normalize a file path for consistent comparison (lowercase on Windows, forward slashes)
fn normalize_file_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    #[cfg(target_os = "windows")]
    let normalized = p.to_lowercase();
    #[cfg(not(target_os = "windows"))]
    let normalized = p;
    normalized
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register all built-in tools
pub fn register_builtin_tools(registry: &Registry) {
    registry.register(exec_tool::ExecTool::new());
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
    registry.register(brief_tool::BriefTool::new());
    registry.register(ask_user_question::AskUserQuestionTool::new());

    // ToolSearchTool: uses a shared tools list that gets populated
    // after all tools are registered (see finalize_tool_search).
    let (tool_search, tools_list) = tool_search_tool::ToolSearchTool::with_shared_tools();
    registry.register(tool_search);
    // Store the shared list in the registry for later population
    registry.set_tools_list(tools_list);
}

/// Register MCP and skills tools
pub fn register_mcp_and_skills(registry: &Registry, cfg: &Config) {
    if let Some(mcp_manager) = &cfg.mcp_manager {
        registry.register(mcp_tools::ListMcpTools::new(mcp_manager.clone()));
        registry.register(mcp_tools::McpToolCaller::new(mcp_manager.clone()));
        registry.register(mcp_tools::McpServerStatus::new(mcp_manager.clone()));
    }

    if let Some(skill_loader) = &cfg.skill_loader {
        let arc_loader = Arc::new(skill_loader.clone());
        registry.register(skill_tools::ReadSkillTool::new(arc_loader.clone()));
        registry.register(skill_tools::ListSkillsTool::new(arc_loader.clone()));
        registry.register(skill_tools::SearchSkillTool::new(arc_loader));
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

/// Register memory tools (Phase 4: Session Memory)
pub fn register_memory_tools(registry: &Registry, session_memory: &Arc<crate::session_memory::SessionMemory>) {
    registry.register(memory_tool::MemoryAddTool::new(Arc::clone(session_memory)));
    registry.register(memory_tool::MemorySearchTool::new(Arc::clone(session_memory)));
}

/// Register task tools (TaskCreate/TaskList/TaskGet/TaskUpdate/TaskStop)
pub fn register_task_tools(registry: &Registry, store: &crate::work_task::SharedWorkTaskStore) {
    let store_clone = Arc::clone(store);
    registry.register(task_tool::TaskCreateTool::new(Arc::new(move |subject, desc, active_form, meta| {
        store_clone.create_task(&subject, &desc, &active_form, meta)
    })));

    let store_clone = Arc::clone(store);
    registry.register(task_tool::TaskListTool::new(Arc::new(move || {
        store_clone.list_tasks()
    })));

    let store_clone = Arc::clone(store);
    registry.register(task_tool::TaskGetTool::new(Arc::new(move |id| {
        store_clone.get_task_info(&id)
    })));

    let store_clone = Arc::clone(store);
    registry.register(task_tool::TaskUpdateTool::new(Arc::new(move |id, updates| {
        store_clone.update_task(&id, &updates)
    })));

    // TaskStop: placeholder that just marks the task as deleted
    let store_clone = Arc::clone(store);
    registry.register(task_tool::TaskStopTool::new(Arc::new(move |id| {
        let mut updates = std::collections::HashMap::new();
        updates.insert("status".to_string(), serde_json::json!("deleted"));
        store_clone.update_task(&id, &updates)
    })));
}

/// Register agent tool with spawn callback
pub fn register_agent_tool(registry: &Registry, spawn_func: agent_tool::AgentSpawnFunc) {
    registry.register(agent_tool::AgentTool::with_spawn_func_arc(spawn_func));
}

/// Register agent management tools (agent_list, agent_get, agent_kill)
pub fn register_agent_management_tools(registry: &Registry, store: &agent_store::SharedAgentTaskStore) {
    agent_tools::register_agent_tools(registry, store);
}

/// Register TodoWrite tool
pub fn register_todo_write_tools(registry: &Registry, todo_list: &Arc<crate::context::TodoList>) {
    registry.register(todo_write::TodoWriteTool::new(Arc::clone(todo_list)));
}

/// Register SendMessage tool
pub fn register_send_message_tool(registry: &Registry, store: &agent_store::SharedAgentTaskStore) {
    registry.register(send_message::SendMessageTool::new(Arc::clone(store)));
}

/// Register bash background task tools (task_stop, task_output) and the exec tool
/// with a background callback.
/// Returns the notification channel receiver (the sender is wired into the exec callback).
pub fn register_bash_task_tools(
    registry: &Registry,
    task_store: crate::task_store::SharedTaskStore,
) -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    // Register task_stop tool
    registry.register(exec_tool::TaskStopTool::new(
        exec_tool::make_task_stop_func(Arc::clone(&task_store)),
    ));

    // Register task_output tool
    registry.register(exec_tool::TaskOutputTool::new(
        exec_tool::make_task_output_func(Arc::clone(&task_store)),
    ));

    // Register exec tool with background callback
    let callback = exec_tool::make_bash_bg_callback(task_store, tx);
    registry.register(exec_tool::ExecTool::with_background_callback(callback));

    rx
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

    // On Windows, bare drive letter like "E:" means current dir on that drive.
    // Normalize to "E:\" to reference the drive root.
    let normalized = if p.len() == 2 && p.chars().nth(1) == Some(':') {
        format!("{}\\", p)
    } else {
        p
    };

    let path = std::path::Path::new(&normalized);
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

/// Check that a resolved file path is within the working directory.
/// Returns Ok(()) if allowed, or Err(error_message) if the path escapes the project.
pub fn is_path_allowed(path: &str) -> Result<(), String> {
    let resolved = expand_path(path);
    let wd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;

    // Resolve symlinks on both sides for robustness
    let abs_wd = std::fs::canonicalize(&wd).unwrap_or_else(|_| wd.clone());
    let abs_resolved = match std::fs::canonicalize(&resolved) {
        Ok(p) => p,
        Err(_) => {
            // File doesn't exist yet - check parent directory instead
            if let Some(parent) = resolved.parent() {
                if let Ok(canonical_parent) = parent.canonicalize() {
                    canonical_parent.join(resolved.file_name().unwrap_or_default())
                } else {
                    resolved.clone()
                }
            } else {
                resolved.clone()
            }
        }
    };

    let rel = abs_resolved.strip_prefix(&abs_wd)
        .map_err(|_| format!("path {:?} is outside the project directory", path))?;
    // rel is empty when path equals the project directory itself (e.g. "."),
    // which is perfectly valid — only block paths that escape via ".."
    if rel.starts_with("..") {
        Err(format!("path {:?} is outside the project directory", path))
    } else {
        Ok(())
    }
}
