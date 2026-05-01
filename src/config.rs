use chrono::Local;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::permissions::PermissionMode;
pub use crate::tools::Registry;
pub use crate::mcp::Manager as McpManager;
pub use crate::skills::Loader as SkillLoader;
pub use crate::skills::SkillTracker;
pub use crate::filehistory::FileHistory;
pub use crate::session_memory::SessionMemory;

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub max_turns: usize,
    pub max_context_msgs: usize,
    pub permission_mode: PermissionMode,
    pub allowed_commands: Vec<String>,
    pub denied_patterns: Vec<String>,
    pub project_dir: PathBuf,
    pub mcp_manager: Option<Arc<McpManager>>,
    pub skill_loader: Option<SkillLoader>,
    pub file_history: Option<Arc<FileHistory>>,
    // Compaction config
    pub auto_compact_enabled: bool,
    pub auto_compact_threshold: f64,
    pub auto_compact_buffer: usize,
    pub max_compact_output_tokens: usize,
    // Micro-compact config (Phase 1: time-based tool result clearing)
    pub micro_compact_enabled: bool,
    pub micro_compact_keep_recent: usize,
    pub micro_compact_placeholder: String,
    // Post-compact recovery config (Phase 2)
    pub post_compact_recover_files: bool,
    pub post_compact_max_files: usize,
    pub post_compact_max_file_chars: usize,
    pub post_compact_max_skill_chars: usize,
    pub post_compact_max_total_skill_chars: usize,
    // History snip config (Phase 3)
    pub post_compact_history_snip_count: usize,
    // Session memory (Phase 4)
    pub session_memory: Option<Arc<SessionMemory>>,
    // Reactive compact: trigger compaction when token delta exceeds this threshold
    pub reactive_compact_threshold: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: String::new(),
            api_key: None,
            base_url: None,
            max_turns: 90,
            max_context_msgs: 100,
            permission_mode: PermissionMode::Ask,
            allowed_commands: vec![
                "ls".to_string(),
                "cat".to_string(),
                "head".to_string(),
                "tail".to_string(),
                "wc".to_string(),
                "find".to_string(),
                "grep".to_string(),
                "rg".to_string(),
                "git status".to_string(),
                "git diff".to_string(),
                "git log".to_string(),
                "git branch".to_string(),
                "python".to_string(),
                "python3".to_string(),
                "pip".to_string(),
                "npm".to_string(),
                "node".to_string(),
                "echo".to_string(),
                "pwd".to_string(),
                "which".to_string(),
                "env".to_string(),
                "date".to_string(),
            ],
            denied_patterns: vec![
                r"rm -rf /".to_string(),
                r"rm -rf ~".to_string(),
                r"sudo rm".to_string(),
                r"git push --force".to_string(),
                r"git reset --hard".to_string(),
                r"> /dev/sda".to_string(),
                r"mkfs".to_string(),
                r"dd if=".to_string(),
            ],
            project_dir: PathBuf::from("."),
            mcp_manager: None,
            skill_loader: None,
            file_history: None,
            auto_compact_enabled: true,
            auto_compact_threshold: 0.75,
            auto_compact_buffer: 13_000,
            max_compact_output_tokens: 20_000,
            micro_compact_enabled: true,
            micro_compact_keep_recent: 5,
            micro_compact_placeholder: "[Old tool result content cleared]".to_string(),
            post_compact_recover_files: true,
            post_compact_max_files: 5,
            post_compact_max_file_chars: 50_000,
            post_compact_max_skill_chars: 5_000,
            post_compact_max_total_skill_chars: 25_000,
            post_compact_history_snip_count: 3,
            session_memory: None,
            reactive_compact_threshold: 5000,
        }
    }
}

impl Config {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }
}

// Settings file structures
#[derive(Debug, Deserialize)]
pub struct ClaudeSettings {
    pub env: EnvSettings,
    #[serde(default)]
    pub mcp: McpSettings,
}

#[derive(Debug, Deserialize)]
pub struct EnvSettings {
    #[serde(rename = "ANTHROPIC_AUTH_TOKEN")]
    pub anthropic_auth_token: Option<String>,
    #[serde(rename = "ANTHROPIC_BASE_URL")]
    pub anthropic_base_url: Option<String>,
    #[serde(rename = "ANTHROPIC_MODEL")]
    pub anthropic_model: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct McpSettings {
    #[serde(default)]
    pub servers: std::collections::HashMap<String, McpServerConfig>,
}

#[derive(Debug, Deserialize)]
pub struct McpServerConfig {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<std::collections::HashMap<String, String>>,
    #[allow(dead_code)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct McpConfigFile {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: std::collections::HashMap<String, McpConfigEntry>,
}

#[derive(Debug, Deserialize)]
pub struct McpConfigEntry {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<std::collections::HashMap<String, String>>,
    pub url: Option<String>,
}

pub fn load_config_from_file(project_dir: &Path) -> Option<Config> {
    let mut cfg = Config::default();
    cfg.project_dir = project_dir.to_path_buf();

    let settings_path = project_dir.join(".claude").join("settings.json");
    if let Ok(data) = std::fs::read_to_string(&settings_path) {
        if let Ok(settings) = serde_json::from_str::<ClaudeSettings>(&data) {
            if let Some(key) = settings.env.anthropic_auth_token {
                cfg.api_key = Some(key);
            }
            if let Some(url) = settings.env.anthropic_base_url {
                cfg.base_url = Some(url);
            }
            if let Some(model) = settings.env.anthropic_model {
                cfg.model = model;
            }

            // Load MCP servers from settings.json
            let mcp_manager = crate::mcp::Manager::new();
            for (name, srv) in settings.mcp.servers {
                if let Some(cmd) = srv.command {
                    let args = srv.args.unwrap_or_default();
                    let env = srv.env.unwrap_or_default();
                    mcp_manager.register(&name, &cmd, &args, env);
                }
            }
            cfg.mcp_manager = Some(Arc::new(mcp_manager));
        }
    }

    // Load MCP config from .mcp.json
    let mcp_path = project_dir.join(".mcp.json");
    if let Ok(data) = std::fs::read_to_string(&mcp_path) {
        if let Ok(mcp_cfg) = serde_json::from_str::<McpConfigFile>(&data) {
            let mcp_manager = cfg.mcp_manager.get_or_insert_with(|| Arc::new(crate::mcp::Manager::new()));
            for (name, entry) in mcp_cfg.mcp_servers {
                if let Some(url) = entry.url {
                    mcp_manager.register_remote(&name, &url, entry.env.unwrap_or_default());
                } else if let Some(cmd) = entry.command {
                    let args = entry.args.unwrap_or_default();
                    mcp_manager.register(&name, &cmd, &args, entry.env.unwrap_or_default());
                }
            }
        }
    }

    // Start MCP servers after all loading is done
    if let Some(ref mgr) = cfg.mcp_manager {
        if !mgr.list_servers().is_empty() {
            if let Err(e) = mgr.start_all() {
                eprintln!("MCP start error: {}", e);
            }
        }
    }

    // Initialize skill loader
    let workspace = project_dir.join("skills");
    let mut skill_loader = crate::skills::Loader::new(&workspace);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let builtin_skills = exe_dir.join("skills");
            if builtin_skills.exists() {
                skill_loader.set_builtin_dir(&builtin_skills);
            }
        }
    }
    skill_loader.refresh();
    cfg.skill_loader = Some(skill_loader);

    // Initialize file history for undo/rewind (with disk persistence)
    let snapshots_dir = project_dir.join(".claude").join("snapshots");
    cfg.file_history = Some(Arc::new(FileHistory::new_with_dir(&snapshots_dir)));

    // Return found if any config was loaded
    if cfg.api_key.is_some() || !cfg.model.is_empty() || cfg.mcp_manager.as_ref().map_or(false, |m| !m.list_servers().is_empty()) {
        Some(cfg)
    } else {
        None
    }
}

/// Cached system prompt wrapper.
/// Caches the system prompt and only rebuilds it when marked dirty
/// (e.g., after compaction events). This is critical for Anthropic
/// prefix caching to work effectively.
pub struct CachedSystemPrompt {
    cached: std::sync::RwLock<Option<String>>,
    dirty: std::sync::atomic::AtomicBool,
}

impl CachedSystemPrompt {
    pub fn new() -> Self {
        Self {
            cached: std::sync::RwLock::new(None),
            dirty: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// Get the system prompt, rebuilding if dirty
    pub fn get_or_build(
        &self,
        registry: &Registry,
        permission_mode: &PermissionMode,
        project_dir: &Path,
        model_name: &str,
        skill_loader: Option<&SkillLoader>,
        skill_tracker: Option<&SkillTracker>,
        session_memory: Option<&SessionMemory>,
    ) -> String {
        if !self.dirty.load(std::sync::atomic::Ordering::SeqCst) {
            if let Some(cached) = self.cached.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                return cached.clone();
            }
        }

        // Rebuild the prompt
        let prompt = build_system_prompt(
            registry, permission_mode, project_dir, model_name, skill_loader, skill_tracker, session_memory,
        );

        // Cache it
        *self.cached.write().unwrap_or_else(|e| e.into_inner()) = Some(prompt.clone());
        self.dirty.store(false, std::sync::atomic::Ordering::SeqCst);

        prompt
    }

    /// Mark the prompt as needing rebuild (call after compaction)
    pub fn mark_dirty(&self) {
        self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for CachedSystemPrompt {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build_system_prompt(
    registry: &Registry,
    permission_mode: &PermissionMode,
    project_dir: &Path,
    model_name: &str,
    skill_loader: Option<&SkillLoader>,
    skill_tracker: Option<&SkillTracker>,
    session_memory: Option<&SessionMemory>,
) -> String {
    // Use compile-time version (avoids spawning rustc on every API call)
    let rust_version = concat!("rustc ", env!("CARGO_PKG_VERSION"));

    let mut prompt = format!(
        r#"You are miniClaudeCode (model: {}), a lightweight AI coding assistant that operates in the terminal.

## Environment
- OS: {} / {} / {}
- Working Directory: {}
- Shell: PowerShell on Windows, sh/bash on Unix
- Current Date/Time: {} ({})

You have access to the following tools to help the user with software engineering tasks:
"#,
        model_name,
        std::env::consts::OS,
        rust_version,
        std::env::consts::ARCH,
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        Local::now().format("%z")
    );

    // Add tool list
    for tool in registry.all_tools() {
        prompt.push_str(&format!("- **{}**: {}\n", tool.name(), tool.description()));
    }

    // Operating Rules
    prompt.push_str("\n## Operating Rules\n\n");
    prompt.push_str("### Behavioral Guidelines\n\n");
    prompt.push_str("1. **Think Before Coding** -- Don't assume. Don't hide confusion. State assumptions explicitly. If multiple interpretations exist, present them. If something is unclear, stop and ask.\n");
    prompt.push_str("2. **Simplicity First** -- Write the minimum code that solves the problem. No features beyond what was asked. No abstractions for single-use code. No error handling for impossible scenarios.\n");
    prompt.push_str("3. **Surgical Changes** -- Touch only what you must. Don't \"improve\" adjacent code, comments, or formatting. Don't refactor things that aren't broken. Match existing style. Remove only imports/variables/functions that YOUR changes made unused.\n");
    prompt.push_str("4. **Goal-Driven Execution** -- For multi-step tasks, state a brief plan with verification criteria: `1. [Step] → verify: [check]`. Define success criteria before starting.\n\n");
    prompt.push_str("### Tool Rules\n\n");
    prompt.push_str("5. Always read a file before editing it.\n");
    prompt.push_str("6. Use tools to accomplish tasks -- don't just describe what to do.\n");
    prompt.push_str("7. When running bash commands, prefer non-destructive read operations.\n");
    prompt.push_str("8. For file edits, provide enough context in old_string to uniquely match.\n");
    prompt.push_str("9. Be concise and direct in your responses.\n");
    prompt.push_str("10. On Windows, use PowerShell syntax and commands (e.g., Get-ChildItem, Test-Path, Copy-Item). On Unix, use bash commands.\n");
    prompt.push_str("11. Prefer built-in tools over exec commands. For git operations, use the git tool instead of exec. For file searches, use grep and glob instead of exec. Always choose the most appropriate built-in tool when available.\n");
    prompt.push_str("12. **Sub-Agent Dispatching** -- When the user requests dispatching, delegating, or assigning a task to a sub-agent (indicated by keywords like: 派遣, 安排, 让, 要, 使, dispatch, delegate, spawn, launch agent, sub-agent), you MUST use the \"agent\" tool. Do NOT use mcp_call_tool, coze_llm, minimax_llm, or any MCP tool for sub-agent dispatching. The \"agent\" tool creates autonomous sub-agents with their own context and tool access. MCP LLM tools are only for calling external LLM APIs (generation/embedding/search), NOT for creating sub-agents.\n\n");

    // Common tool parameter
    prompt.push_str("## Tool Parameters\n\n");
    prompt.push_str("All tools accept an optional **`timeout`** parameter (integer, seconds, range 1-300, default 30) to override the execution timeout. Use a larger timeout for operations that may take longer, such as scanning large directories with `grep` or `glob`.\n\n");

    // Task Management Rules
    prompt.push_str("## Task Management Rules\n\n");
    prompt.push_str("13. **When to Create Tasks** -- Use task_create for complex multi-step tasks (3+ distinct steps or actions). When the user provides multiple tasks (numbered or comma-separated), immediately capture them as tasks. When starting work on a task, mark it as in_progress BEFORE beginning work. After completing a task, mark it as completed and add any new follow-up tasks discovered.\n");
    prompt.push_str("14. **Task Workflow** -- Create tasks with clear, specific subjects in imperative form (e.g., \"Fix authentication bug\"). Use task_update to set status: pending → in_progress → completed. ONLY mark as completed when FULLY accomplished — if tests fail, implementation is partial, or you encountered unresolved errors, keep the task in_progress. If blocked, create a new task describing what needs to be resolved. After completing a task, check task_list to find the next available task. Do not batch up multiple tasks before marking them as completed — mark each one done as soon as it is finished.\n");
    prompt.push_str("15. **Background Command Execution** -- For long-running commands, use run_in_background=true with the exec tool. You will receive a task ID and output file path immediately; you do not need to check the output right away. When the background task completes, you will be notified via a task-notification message. Use task_output to retrieve results. Use task_stop to stop a running background task if needed. Do NOT use sleep to poll for results — use run_in_background and wait for the notification.\n\n");

    // Permission mode
    let mode_upper = permission_mode.to_string().to_uppercase();
    let mode_desc = match permission_mode {
        PermissionMode::Ask => "In ASK mode, potentially dangerous operations will require user confirmation.",
        PermissionMode::Auto => "In AUTO mode, all operations are auto-approved (use with caution).",
        PermissionMode::Plan => "In PLAN mode, only read-only operations are allowed. Write operations are blocked.",
    };
    prompt.push_str(&format!("## Current Permission Mode: {}\n{}\n", mode_upper, mode_desc));

    // Project instructions from CLAUDE.md
    let claude_md = project_dir.join("CLAUDE.md");
    if claude_md.exists() {
        if let Ok(content) = std::fs::read_to_string(&claude_md) {
            prompt.push_str("\n## Project Instructions (from CLAUDE.md)\n\n");
            prompt.push_str(content.trim());
            prompt.push('\n');
        }
    }

    // Session memory section (Phase 4)
    if let Some(memory) = session_memory {
        let mem_section = memory.format_for_prompt();
        if !mem_section.is_empty() {
            prompt.push_str(&mem_section);
        }
    }

    // Skill System Guidance
    if skill_loader.is_some() {
        prompt.push_str("\n## Skill System Guidance\n\n");
        prompt.push_str("BLOCKING REQUIREMENT: When a skill matches the user's request, you MUST invoke the relevant skill tool BEFORE generating any other response. Do NOT proceed with alternative approaches until you have checked for relevant skills.\n\n");
        prompt.push_str("Your visible tool list is partial by design. The tool registry does not show all available capabilities. Before telling the user that a capability is unavailable, search for a skill using search_skills.\n\n");
        prompt.push_str("When exploring a new task:\n");
        prompt.push_str("1. Use search_skills to find relevant skills\n");
        prompt.push_str("2. Use read_skill to load a skill's full instructions\n");
        prompt.push_str("3. Follow the skill's instructions\n\n");
    }

    // Skills section
    if let Some(loader) = skill_loader {
        let mut tracker_ref = skill_tracker;
        let mut temp_tracker;

        // Create a mutable copy if we need to mark skills as shown
        let unsent_names: Vec<String> = if let Some(tracker) = tracker_ref {
            let all_skills = loader.list_skills();
            all_skills
                .iter()
                .filter(|s| !s.always && tracker.is_new_skill(&s.name))
                .map(|s| s.name.clone())
                .collect()
        } else {
            vec![]
        };

        // If we have unsent skills, mark them as shown
        if !unsent_names.is_empty() {
            temp_tracker = skill_tracker.map(|t| (*t).clone()).unwrap_or_default();
            for name in &unsent_names {
                temp_tracker.mark_shown(name);
            }
            tracker_ref = Some(&temp_tracker);
        }

        // Always-on skills
        let always_skills = loader.get_always_skills();
        if !always_skills.is_empty() {
            let skill_names: Vec<String> = always_skills.iter().map(|s| s.name.clone()).collect();
            prompt.push_str(&loader.build_system_prompt_for_skills(&skill_names));
        }

        // Per-turn: newly available skills (not yet shown)
        if let Some(tracker) = tracker_ref {
            let unsent: Vec<crate::skills::SkillInfo> = loader.list_skills()
                .into_iter()
                .filter(|s| !s.always && tracker.is_new_skill(&s.name))
                .collect();
            if !unsent.is_empty() {
                let mut section = String::from("\n## Available Skills (New This Turn)\n\n");
                let mut char_count = section.len();
                let budget = 4000;
                for skill in unsent {
                    let mut entry = format!("- **{}** -- {}", skill.name, skill.description);
                    if let Some(when) = &skill.when_to_use {
                        entry.push_str(&format!(" ({})", when));
                    }
                    if !skill.available {
                        entry.push_str(" (unavailable)");
                    }
                    entry.push('\n');
                    if char_count + entry.len() > budget {
                        break;
                    }
                    section.push_str(&entry);
                    char_count += entry.len();
                }
                section.push_str("\nUse read_skill to load a skill's full instructions.\n");
                prompt.push_str(&section);
            }
        }

        // Skills summary for discovery (already-shown non-always skills)
        let summary = loader.build_skills_summary();
        if !summary.is_empty() {
            if !prompt.ends_with("\n\n") {
                prompt.push_str("\n\n");
            }
            prompt.push_str(&summary);
        }
    }

    prompt
}
