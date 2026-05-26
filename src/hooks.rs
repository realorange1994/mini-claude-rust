//! Hook Manager module for compact lifecycle events
//!
//! Provides pre-compact and post-compact hook registration and execution.
//! Hooks are called synchronously with a timeout context.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Trigger type for compaction events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTrigger {
    Manual,
    Auto,
    SmCompact,
}

impl std::fmt::Display for HookTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookTrigger::Manual => write!(f, "manual"),
            HookTrigger::Auto => write!(f, "auto"),
            HookTrigger::SmCompact => write!(f, "sm_compact"),
        }
    }
}

/// Input passed to pre-compact hooks
#[derive(Debug, Clone)]
pub struct PreCompactInput {
    pub trigger: HookTrigger,
    /// Instructions already queued for the summarizer; hooks can append to this
    pub custom_instructions: String,
}

/// Output from pre-compact hooks
#[derive(Debug, Clone, Default)]
pub struct PreCompactOutput {
    /// Additional instructions for the compaction prompt
    pub custom_instructions: String,
    /// Message to display to the user (logged, not injected into prompt)
    pub user_message: String,
}

/// Input passed to post-compact hooks
#[derive(Debug, Clone)]
pub struct PostCompactInput {
    pub trigger: HookTrigger,
    /// The summary that replaced the compacted conversation
    pub compact_summary: String,
    /// Files that were re-injected post-compaction
    pub recovered_files: Vec<String>,
}

/// Output from post-compact hooks
#[derive(Debug, Clone, Default)]
pub struct PostCompactOutput {
    /// Message to display to the user
    pub user_message: String,
    /// Content to inject as an attachment (added to prompt context)
    pub attachment: String,
}

/// Pre-compact hook handler signature
pub type PreCompactHandler =
    Arc<dyn Fn(PreCompactInput) -> PreCompactOutput + Send + Sync>;

/// Post-compact hook handler signature
pub type PostCompactHandler =
    Arc<dyn Fn(PostCompactInput) -> PostCompactOutput + Send + Sync>;

/// A registered hook entry
struct HookEntry {
    name: String,
    handler: Arc<dyn Fn(PreCompactInput) -> PreCompactOutput + Send + Sync>,
    timeout: Duration,
}

/// Thread-safe hook manager for compact lifecycle events
pub struct HookManager {
    pre_compact_hooks: Arc<Mutex<Vec<HookEntry>>>,
    post_compact_prelude: Arc<Mutex<Vec<(String, PostCompactHandler)>>>,
    post_compact_epilogue: Arc<Mutex<Vec<(String, PostCompactHandler)>>>,
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

impl HookManager {
    /// Create a new HookManager
    pub fn new() -> Self {
        Self {
            pre_compact_hooks: Arc::new(Mutex::new(Vec::new())),
            post_compact_prelude: Arc::new(Mutex::new(Vec::new())),
            post_compact_epilogue: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a pre-compact hook with the given timeout.
    /// The handler is called synchronously before compaction runs.
    pub fn register_pre_compact<H>(&self, name: &str, handler: H, timeout: Duration)
    where
        H: Fn(PreCompactInput) -> PreCompactOutput + Send + Sync + 'static,
    {
        let entry = HookEntry {
            name: name.to_string(),
            handler: Arc::new(handler),
            timeout,
        };
        let guard = self.pre_compact_hooks.blocking_lock();
        guard.push(entry);
    }

    /// Register a post-compact prelude hook.
    /// Prelude hooks run AFTER the summary is generated but BEFORE it is injected.
    /// They receive the raw summary and can modify it or add attachments.
    pub fn register_post_compact_prelude<H>(&self, name: &str, handler: H)
    where
        H: Fn(PostCompactInput) -> PostCompactOutput + Send + Sync + 'static,
    {
        let mut guard = self.post_compact_prelude.blocking_lock();
        guard.push((name.to_string(), Arc::new(handler)));
    }

    /// Register a post-compact epilogue hook.
    /// Epilogue hooks run AFTER the summary is injected into context.
    /// They are mainly for side effects (notifications, cleanup, etc.).
    pub fn register_post_compact_epilogue<H>(&self, name: &str, handler: H)
    where
        H: Fn(PostCompactInput) -> PostCompactOutput + Send + Sync + 'static,
    {
        let mut guard = self.post_compact_epilogue.blocking_lock();
        guard.push((name.to_string(), Arc::new(handler)));
    }

    /// Execute all pre-compact hooks sequentially.
    /// Outputs are merged: CustomInstructions concatenated, UserMessage appended.
    pub async fn execute_pre_compact_hooks(
        &self,
        input: PreCompactInput,
    ) -> (PreCompactOutput, Option<String>) {
        let guard = self.pre_compact_hooks.lock().await;
        if guard.is_empty() {
            return (PreCompactOutput::default(), None);
        }

        let mut result = PreCompactOutput::default();
        let mut first_err: Option<String> = None;

        for entry in guard.iter() {
            let timeout = if entry.timeout.is_zero() {
                Duration::from_secs(5)
            } else {
                entry.timeout
            };

            // Create a closure that captures the input and handler
            let input_clone = input.clone();
            let handler = Arc::clone(&entry.handler);

            // Run with timeout
            let out = match tokio::time::timeout(timeout, async {
                handler(input_clone)
            })
            .await
            {
                Ok(out) => out,
                Err(_) => {
                    let err_msg = format!("[hook:{}] timed out after {:?}", entry.name, timeout);
                    if first_err.is_none() {
                        first_err = Some(err_msg.clone());
                    } else {
                        first_err = first_err.as_ref().map(|e| format!("{}\n{}", e, err_msg));
                    }
                    result.user_message.push_str(&format!(
                        "\nPreCompact [hook:{}] timed out after {:?}",
                        entry.name, timeout
                    ));
                    continue;
                }
            };

            // Merge CustomInstructions
            if !out.custom_instructions.is_empty() {
                if result.custom_instructions.is_empty() {
                    result.custom_instructions = out.custom_instructions;
                } else {
                    result.custom_instructions.push_str("\n\n");
                    result.custom_instructions.push_str(&out.custom_instructions);
                }
            }

            // Append UserMessage
            if !out.user_message.is_empty() {
                if result.user_message.is_empty() {
                    result.user_message = format!("PreCompact [hook:{}] completed: {}", entry.name, out.user_message);
                } else {
                    result.user_message.push_str(&format!(
                        "\nPreCompact [hook:{}] completed: {}",
                        entry.name, out.user_message
                    ));
                }
            }
        }

        (result, first_err)
    }

    /// Execute post-compact prelude hooks (run BEFORE summary injection).
    /// These hooks can modify the summary or add attachments.
    pub async fn execute_post_compact_prelude_hooks(
        &self,
        input: PostCompactInput,
    ) -> PostCompactOutput {
        let guard = self.post_compact_prelude.lock().await;
        if guard.is_empty() {
            return PostCompactOutput::default();
        }

        let mut result = PostCompactOutput::default();

        for (name, handler) in guard.iter() {
            let timeout = Duration::from_secs(5); // Default 5s for post-compact
            let input_clone = input.clone();
            let handler = Arc::clone(handler);

            let out = match tokio::time::timeout(timeout, async {
                handler(input_clone)
            })
            .await
            {
                Ok(out) => out,
                Err(_) => {
                    eprintln!("[hook:{}] post-compact prelude timed out", name);
                    continue;
                }
            };

            // Only take the attachment from prelude hooks
            if !out.attachment.is_empty() {
                if result.attachment.is_empty() {
                    result.attachment = out.attachment;
                } else {
                    result.attachment.push_str("\n\n");
                    result.attachment.push_str(&out.attachment);
                }
            }
        }

        result
    }

    /// Execute post-compact epilogue hooks (run AFTER summary injection).
    /// These are mainly for side effects and notifications.
    pub async fn execute_post_compact_epilogue_hooks(
        &self,
        input: PostCompactInput,
    ) -> (PostCompactOutput, Option<String>) {
        let guard = self.post_compact_epilogue.lock().await;
        if guard.is_empty() {
            return (PostCompactOutput::default(), None);
        }

        let mut result = PostCompactOutput::default();
        let mut first_err: Option<String> = None;

        for (name, handler) in guard.iter() {
            let timeout = Duration::from_secs(5);
            let input_clone = input.clone();
            let handler = Arc::clone(handler);

            let out = match tokio::time::timeout(timeout, async {
                handler(input_clone)
            })
            .await
            {
                Ok(out) => out,
                Err(_) => {
                    let err_msg = format!("[hook:{}] timed out after 5s", name);
                    if first_err.is_none() {
                        first_err = Some(err_msg.clone());
                    }
                    result.user_message.push_str(&format!(
                        "\nPostCompact [hook:{}] timed out",
                        name
                    ));
                    continue;
                }
            };

            // Append UserMessage
            if !out.user_message.is_empty() {
                if result.user_message.is_empty() {
                    result.user_message = format!("PostCompact [hook:{}] completed: {}", name, out.user_message);
                } else {
                    result.user_message.push_str(&format!(
                        "\nPostCompact [hook:{}] completed: {}",
                        name, out.user_message
                    ));
                }
            }
        }

        (result, first_err)
    }

    /// Execute all post-compact hooks (prelude then epilogue).
    /// Returns merged output from all hooks.
    pub async fn execute_post_compact_hooks(
        &self,
        input: PostCompactInput,
    ) -> PostCompactOutput {
        let prelude_out = self.execute_post_compact_prelude_hooks(input.clone()).await;

        let (epilogue_out, _) = self.execute_post_compact_epilogue_hooks(input).await;

        // Merge outputs
        let mut result = prelude_out;
        if !epilogue_out.user_message.is_empty() {
            if result.user_message.is_empty() {
                result.user_message = epilogue_out.user_message;
            } else {
                result.user_message.push_str(&epilogue_out.user_message);
            }
        }

        result
    }

    /// Returns the number of registered hooks
    pub async fn hook_count(&self) -> usize {
        let pre = self.pre_compact_hooks.lock().await.len();
        let prelude = self.post_compact_prelude.lock().await.len();
        let epilogue = self.post_compact_epilogue.lock().await.len();
        pre + prelude + epilogue
    }
}

// ---------------------------------------------------------------------------
// Shell Hook Infrastructure
// ---------------------------------------------------------------------------

/// Events that can trigger hooks, matching Go's HookEvent constants.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PreUserMessage,
    PostUserMessage,
    PreAssistantMessage,
    PostAssistantMessage,
    PreApiCall,
    PostApiCall,
    OnError,
    OnAbort,
    OnNotification,
    OnSubagent,
    OnFork,
    OnResume,
    Stop,
}

/// JSON-serializable input sent to a shell hook via stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellHookInput {
    pub session_id: String,
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub tool_result: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Parsed output from a shell hook's stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellHookOutput {
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, rename = "suppressOutput")]
    pub suppress_output: bool,
    #[serde(default, rename = "updatedInput")]
    pub updated_input: Option<serde_json::Value>,
    #[serde(default, rename = "hookSpecificOutput")]
    pub hook_specific_output: Option<serde_json::Value>,
}

impl Default for ShellHookOutput {
    fn default() -> Self {
        Self {
            decision: "approve".into(),
            reason: None,
            suppress_output: false,
            updated_input: None,
            hook_specific_output: None,
        }
    }
}

/// A shell command hook loaded from settings.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCommand {
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub r#async: bool,
    #[serde(default)]
    pub r#type: Option<String>,
}

/// HookConfig maps event names to lists of HookCommand.
pub type HookConfig = HashMap<String, Vec<HookCommand>>;

/// Result of executing a shell hook.
#[derive(Debug, Clone)]
pub struct HookShellResult {
    pub decision: String,
    pub reason: Option<String>,
    pub permission_decision: Option<String>,
    pub permission_decision_reason: Option<String>,
    pub updated_input: Option<serde_json::Value>,
    pub suppress_output: bool,
    pub stop_reason: Option<String>,
    pub system_message: Option<String>,
    pub raw_stdout: String,
    pub raw_stderr: String,
    pub exit_code: i32,
}

impl HookShellResult {
    /// Returns true if this hook result indicates the tool should be blocked.
    pub fn should_block(&self) -> bool {
        self.decision == "block" || self.permission_decision.as_deref() == Some("deny")
    }

    /// Returns true if the hook wants the user to be prompted.
    pub fn should_ask(&self) -> bool {
        self.permission_decision.as_deref() == Some("ask")
    }

    /// Returns the reason why the hook blocked the tool.
    pub fn block_reason(&self) -> String {
        self.permission_decision_reason
            .clone()
            .or(self.reason.clone())
            .unwrap_or_else(|| "Blocked by hook".to_string())
    }

    /// Parse hook stdout for JSON output.
    pub fn parse_stdout(&mut self) {
        let stdout = self.raw_stdout.trim();
        if stdout.is_empty() || !stdout.starts_with('{') {
            return;
        }

        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(stdout) else {
            return;
        };

        if let Some(v) = parsed.get("continue").and_then(|v| v.as_bool()) {
            if !v {
                self.decision = "block".to_string();
            }
        }
        if let Some(v) = parsed.get("suppressOutput").and_then(|v| v.as_bool()) {
            self.suppress_output = v;
        }
        if let Some(v) = parsed.get("decision").and_then(|v| v.as_str()) {
            self.decision = v.to_string();
        }
        if let Some(v) = parsed.get("reason").and_then(|v| v.as_str()) {
            self.reason = Some(v.to_string());
        }
        if let Some(v) = parsed.get("stopReason").and_then(|v| v.as_str()) {
            self.stop_reason = Some(v.to_string());
        }
        if let Some(v) = parsed.get("systemMessage").and_then(|v| v.as_str()) {
            self.system_message = Some(v.to_string());
        }

        // Extract hookSpecificOutput for PreToolUse hooks
        if let Some(spec) = parsed.get("hookSpecificOutput").and_then(|v| v.as_object()) {
            if let Some(v) = spec.get("permissionDecision").and_then(|v| v.as_str()) {
                self.permission_decision = Some(v.to_string());
            }
            if let Some(v) = spec.get("permissionDecisionReason").and_then(|v| v.as_str()) {
                self.permission_decision_reason = Some(v.to_string());
            }
            if let Some(v) = spec.get("updatedInput") {
                self.updated_input = Some(v.clone());
            }
            if self.system_message.is_none() {
                if let Some(v) = spec.get("additionalContext").and_then(|v| v.as_str()) {
                    self.system_message = Some(v.to_string());
                }
            }
        }
    }
}

/// HookBlockError signals that a PreToolUse hook blocked tool execution.
#[derive(Debug)]
pub struct HookBlockError {
    pub tool_name: String,
    pub command: String,
    pub reason: String,
}

impl std::fmt::Display for HookBlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hook blocked {}: {} (command: {})", self.tool_name, self.reason, self.command)
    }
}

impl std::error::Error for HookBlockError {}

/// Check if a hook's matcher pattern matches the given query string.
/// Supports exact match and glob patterns (* and ?).
pub fn match_hook(hook: &HookCommand, query: &str) -> bool {
    let Some(pattern) = &hook.matcher else {
        return true; // no matcher = match all
    };
    if pattern == query {
        return true;
    }
    hook_glob_match(pattern, query)
}

/// Perform simple glob pattern matching for hook matchers.
/// Supports * (any characters) and ? (single character).
fn hook_glob_match(pattern: &str, text: &str) -> bool {
    let p_bytes = pattern.as_bytes();
    let t_bytes = text.as_bytes();
    let p_len = p_bytes.len();
    let t_len = t_bytes.len();

    // dp[i][j] = can pattern[..i] match text[..j]?
    let mut dp = vec![vec![false; t_len + 1]; p_len + 1];
    dp[0][0] = true;

    for i in 1..=p_len {
        if p_bytes[i - 1] == b'*' {
            dp[i][0] = dp[i - 1][0];
        }
        for j in 1..=t_len {
            if p_bytes[i - 1] == b'*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if p_bytes[i - 1] == b'?' || p_bytes[i - 1] == t_bytes[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[p_len][t_len]
}

/// Build environment variables for a hook command.
fn build_hook_env(extra: Option<&HashMap<String, String>>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();

    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.display().to_string();
        env.push(("CLAUDE_PROJECT_DIR".to_string(), cwd_str.clone()));
        env.push(("CLAUDE_CWD".to_string(), cwd_str));
    }

    if let Some(extra) = extra {
        for (k, v) in extra {
            env.push((k.clone(), v.clone()));
        }
    }

    env
}

/// Execute a shell command hook with JSON input via stdin.
/// Spawns a shell process, passes serialized input as JSON, parses stdout.
pub async fn execute_shell_hook(
    hook: &HookCommand,
    event: &str,
    json_input: &str,
    extra_env: Option<&HashMap<String, String>>,
) -> Result<HookShellResult, String> {
    let timeout = Duration::from_secs(hook.timeout.unwrap_or(600)); // default 10 min

    // Determine shell and build command
    let shell_type = hook.shell.as_deref().unwrap_or("bash");
    let (prog, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        if shell_type == "powershell" {
            ("powershell", vec!["-NoProfile", "-NonInteractive", "-Command", &hook.command])
        } else {
            // On Windows, prefer Git Bash if available
            if let Some(bash) = detect_git_bash_for_hook() {
                (bash, vec!["-c", &hook.command])
            } else {
                ("cmd", vec!["/c", &hook.command])
            }
        }
    } else {
        ("sh", vec!["-c", &hook.command])
    };

    let mut cmd = tokio::process::Command::new(prog);
    for arg in &args {
        cmd.arg(arg);
    }
    cmd.envs(build_hook_env(extra_env));
    if let Ok(cwd) = std::env::current_dir() {
        cmd.current_dir(cwd);
    }

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to start hook command: {e}"))?;

    // Write JSON input to stdin
    if let Some(mut stdin) = child.stdin.take() {
        let input_bytes = format!("{json_input}\n").into_bytes();
        let _ = stdin.write_all(&input_bytes).await;
    }

    // Read stdout and stderr with timeout
    let stdout_future = async {
        let mut buf = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            use tokio::io::AsyncReadExt;
            let _ = stdout.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).to_string()
    };

    let stderr_future = async {
        let mut buf = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            use tokio::io::AsyncReadExt;
            let _ = stderr.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).to_string()
    };

    let (raw_stdout, raw_stderr) = tokio::join!(stdout_future, stderr_future);

    let exit_code = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(e)) => return Err(format!("Hook command failed: {e}")),
        Err(_) => return Err(format!("Hook timed out after {:?}", timeout)),
    };

    let mut result = HookShellResult {
        decision: String::new(),
        reason: None,
        permission_decision: None,
        permission_decision_reason: None,
        updated_input: None,
        suppress_output: false,
        stop_reason: None,
        system_message: None,
        raw_stdout,
        raw_stderr,
        exit_code,
    };

    result.parse_stdout();

    Ok(result)
}

/// Find Git Bash executable on Windows.
fn detect_git_bash_for_hook() -> Option<String> {
    // Check CLAUDE_CODE_GIT_BASH_PATH env var
    if let Ok(path) = std::env::var("CLAUDE_CODE_GIT_BASH_PATH") {
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }

    // Check common locations
    let candidates = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    // Try PATH
    if let Ok(p) = which::which("bash") {
        return p.to_str().map(|s| s.to_string());
    }

    None
}

/// Load hooks from a settings.json file.
pub fn load_hooks_from_settings(file_path: &str) -> Result<HookConfig, String> {
    let data = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read settings file: {e}"))?;

    let raw: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse settings JSON: {e}"))?;

    let Some(hooks_raw) = raw.get("hooks") else {
        return Ok(HookConfig::new());
    };

    let Some(hooks_map) = hooks_raw.as_object() else {
        return Err("hooks must be an object".to_string());
    };

    let mut result = HookConfig::new();
    for (event_name, hook_list) in hooks_map {
        let Some(hooks) = hook_list.as_array() else {
            continue;
        };
        for h in hooks {
            let hook_json = serde_json::to_string(h).unwrap_or_default();
            let Ok(hook) = serde_json::from_str::<HookCommand>(&hook_json) else {
                continue;
            };
            let event_key = capitalize_first(event_name);
            result.entry(event_key).or_default().push(hook);
        }
    }

    Ok(result)
}

/// Load hooks from all config sources (project + home).
pub fn load_all_hooks(project_dir: &str) -> HookConfig {
    let mut result = HookConfig::new();

    // Project-level settings
    let project_path = std::path::Path::new(project_dir)
        .join(".claude")
        .join("settings.json");
    if let Ok(hooks) = load_hooks_from_settings(project_path.to_str().unwrap_or("")) {
        for (event, cmds) in hooks {
            result.entry(event).or_default().extend(cmds);
        }
    }

    // Home directory settings
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let home_path = std::path::Path::new(&home)
            .join(".claude")
            .join("settings.json");
        if let Ok(hooks) = load_hooks_from_settings(home_path.to_str().unwrap_or("")) {
            for (event, cmds) in hooks {
                result.entry(event).or_default().extend(cmds);
            }
        }
    }

    result
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hook_manager_empty() {
        let manager = HookManager::new();
        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_none());
        assert!(result.custom_instructions.is_empty());
        assert!(result.user_message.is_empty());
    }

    #[tokio::test]
    async fn test_pre_compact_hook_basic() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "test_hook",
            |input| PreCompactOutput {
                custom_instructions: format!("additional for trigger {:?}", input.trigger),
                user_message: "hook ran successfully".to_string(),
            },
            Duration::from_secs(5),
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: "original instructions".to_string(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_none());
        assert!(result.custom_instructions.contains("additional"));
        assert!(result.user_message.contains("hook ran successfully"));
    }

    #[tokio::test]
    async fn test_pre_compact_hook_merge_instructions() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "hook1",
            |_| PreCompactOutput {
                custom_instructions: "instruction 1".to_string(),
                user_message: String::new(),
            },
            Duration::from_secs(5),
        );

        manager.register_pre_compact(
            "hook2",
            |_| PreCompactOutput {
                custom_instructions: "instruction 2".to_string(),
                user_message: String::new(),
            },
            Duration::from_secs(5),
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Manual,
            custom_instructions: String::new(),
        };

        let (result, _) = manager.execute_pre_compact_hooks(input).await;
        assert!(result.custom_instructions.contains("instruction 1"));
        assert!(result.custom_instructions.contains("instruction 2"));
    }

    #[tokio::test]
    async fn test_post_compact_prelude_hook() {
        let manager = HookManager::new();

        manager.register_post_compact_prelude(
            "test_prelude",
            |input| PostCompactOutput {
                user_message: String::new(),
                attachment: format!("attachment from summary: {}",
                    &input.compact_summary.chars().take(50).collect::<String>()),
            },
        );

        let input = PostCompactInput {
            trigger: HookTrigger::Auto,
            compact_summary: "This is the full summary text".to_string(),
            recovered_files: vec![],
        };

        let result = manager.execute_post_compact_prelude_hooks(input).await;
        assert!(result.attachment.contains("attachment from summary"));
    }

    #[tokio::test]
    async fn test_post_compact_epilogue_hook() {
        let manager = HookManager::new();

        manager.register_post_compact_epilogue(
            "test_epilogue",
            |input| PostCompactOutput {
                user_message: format!("Notified about {} recovered files",
                    input.recovered_files.len()),
                attachment: String::new(),
            },
        );

        let input = PostCompactInput {
            trigger: HookTrigger::SmCompact,
            compact_summary: "Summary".to_string(),
            recovered_files: vec!["file1.rs".to_string(), "file2.rs".to_string()],
        };

        let (result, _) = manager.execute_post_compact_epilogue_hooks(input).await;
        assert!(result.user_message.contains("Notified about 2 recovered files"));
    }

    #[tokio::test]
    async fn test_hook_timeout() {
        let manager = HookManager::new();

        // Register a hook that takes too long
        manager.register_pre_compact(
            "slow_hook",
            |_| async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                PreCompactOutput::default()
            },
            Duration::from_millis(50), // Very short timeout
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_some()); // Should have timed out
        assert!(result.user_message.contains("timed out"));
    }

    #[tokio::test]
    async fn test_multiple_hooks_first_error_tracked() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "failing_hook",
            |_| PreCompactOutput {
                custom_instructions: String::new(),
                user_message: "first".to_string(),
            },
            Duration::from_secs(5),
        );

        manager.register_pre_compact(
            "slow_hook",
            |_| async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                PreCompactOutput::default()
            },
            Duration::from_millis(50),
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_some());
        assert!(result.user_message.contains("first"));
        assert!(result.user_message.contains("timed out"));
    }

    #[tokio::test]
    async fn test_default_timeout() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "no_timeout_specified",
            |input| PreCompactOutput {
                custom_instructions: format!("got input with trigger {:?}", input.trigger),
                user_message: String::new(),
            },
            Duration::ZERO, // Should use default 5s
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Manual,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_none());
        assert!(result.custom_instructions.contains("Manual"));
    }
}
