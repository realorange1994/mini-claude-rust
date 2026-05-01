//! ExecTool - Shell command execution with security guards
//!
//! This module also contains the background bash task tools (TaskStopTool,
//! TaskOutputTool) and the background bash spawning engine that was previously
//! in bash_task_tools.rs.

use crate::tools::{Tool, ToolResult};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::OnceLock;

// ─── Background task callback type ────────────────────────────────────────────

/// Background task callback: (command, working_dir) -> (task_id, output_file, error_text)
pub type BashBgTaskCallback =
    Arc<dyn Fn(String, String) -> (String, String, String) + Send + Sync>;

// ─── ExecTool ────────────────────────────────────────────────────────────────

/// ExecTool executes shell commands with security guards and background support.
pub struct ExecTool {
    /// When set, enables run_in_background support. The callback spawns a background
    /// bash task and returns (task_id, output_file, error_text).
    pub background_callback: Option<BashBgTaskCallback>,
}

impl ExecTool {
    pub fn new() -> Self {
        Self {
            background_callback: None,
        }
    }

    /// Create with a background task callback.
    pub fn with_background_callback(callback: BashBgTaskCallback) -> Self {
        Self {
            background_callback: Some(callback),
        }
    }
}

impl Default for ExecTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ExecTool {
    fn clone(&self) -> Self {
        Self {
            background_callback: self.background_callback.clone(),
        }
    }
}

impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command. On Windows, use PowerShell syntax (`;` to separate commands, not `&&`). Use `curl.exe` instead of `curl` on Windows (curl is alias to Invoke-WebRequest). Use for running scripts, installing packages, git operations, and any shell task. Commands run in the current working directory. Supports running commands in the background with run_in_background=true."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for the command (default: current directory)."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 120, max 600)."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Set to true to run this command in the background. Returns immediately with a task ID. Use task_output to check results later."
                }
            },
            "required": ["command"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, params: &HashMap<String, Value>) -> Option<ToolResult> {
        let command = params.get("command")?.as_str()?.trim();
        let lower = command.to_lowercase();

        // Check for dangerous patterns (cached regexes)
        static DANGEROUS: OnceLock<Vec<Regex>> = OnceLock::new();
        let dangerous = DANGEROUS.get_or_init(|| {
            [
                r"\brm\s+-[rf]{1,2}\b",
                r"\bdel\s+/[fq]\b",
                r"\brmdir\s+/s\b",
                r"format\b",
                r"\b(mkfs|diskpart)\b",
                r"\bdd\s+.*\bof=",
                r">\s*/dev/sd",
                r"\b(shutdown|reboot|poweroff)\b",
                r":\(\)\s*\{.*\};\s*:",
                r"&\S*&\S*&",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });

        for re in dangerous {
            if re.is_match(&lower) {
                return Some(ToolResult::error(format!("Dangerous command pattern detected: {}", re.as_str())));
            }
        }

        // Check for .git directory destruction (cached regexes)
        static GIT_HARMFUL: OnceLock<Vec<Regex>> = OnceLock::new();
        let git_harmful = GIT_HARMFUL.get_or_init(|| {
            [
                r"rm\s+-rf.*\.git",
                r"rm\s+-r.*\.git",
                r"rmdir.*\.git",
                r"del.*\.git",
                r"rmrf.*\.git",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });
        for re in git_harmful {
            if re.is_match(&lower) {
                return Some(ToolResult::error("Command would destroy .git directory"));
            }
        }

        // Check for home directory destruction (cached regexes)
        static HOME_HARMFUL: OnceLock<Vec<Regex>> = OnceLock::new();
        let home_harmful = HOME_HARMFUL.get_or_init(|| {
            [
                r"rm\s+-rf\s*~",
                r"rm\s+-rf\s+/home",
                r"rm\s+-rf\s+/",
                r"rm\s+-rf\s+C:\\Users",
                r"del\s+/[fq]\s+\w+\\.*",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });
        for re in home_harmful {
            if re.is_match(&lower) {
                return Some(ToolResult::error("Command would destroy home directory or system root"));
            }
        }

        // Check for internal URLs (cached regexes)
        static URL_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
        let url_patterns = URL_PATTERNS.get_or_init(|| {
            [
                r"https?://(localhost|127\.0\.0\.1|0\.0\.0\.0|192\.168\.\d+\.\d+|10\.\d+\.\d+\.\d+|172\.(1[6-9]|2\d|3[01])\.\d+\.\d+)[:/]",
                r"https?://[0-9]+(?:\.[0-9]+){3}:\d+",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });

        for re in url_patterns {
            if re.is_match(&lower) {
                return Some(ToolResult::error("Internal/private URL detected"));
            }
        }

        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        // Check for background execution request
        let run_in_background = params
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if run_in_background {
            return self.exec_in_background(&params);
        }

        self.exec_foreground(params)
    }
}

// ─── Foreground execution ───────────────────────────────────────────────────

impl ExecTool {
    fn exec_foreground(&self, params: HashMap<String, Value>) -> ToolResult {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.trim(),
            None => return ToolResult::error("Error: empty command"),
        };

        if command.is_empty() {
            return ToolResult::error("Error: empty command");
        }

        let timeout_secs = params
            .get("timeout")
            .and_then(|v| v.as_i64())
            .unwrap_or(120)
            .clamp(1, 600) as u64;

        let working_dir = params
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        // Determine shell: powershell -> bash -> cmd on Windows (matching Go)
        // Cached with OnceLock to avoid spawning a process every call
        static SHELL_CACHE: OnceLock<(&'static str, &'static str)> = OnceLock::new();
        let (shell, flag) = SHELL_CACHE.get_or_init(|| {
            if cfg!(target_os = "windows") {
                if std::process::Command::new("powershell").output().is_ok() {
                    ("powershell", "-Command")
                } else if std::process::Command::new("bash").output().is_ok() {
                    ("bash", "-c")
                } else {
                    ("cmd", "/C")
                }
            } else {
                ("bash", "-c")
            }
        });

        let output_result = Command::new(shell)
            .arg(flag)
            .arg(command)
            .current_dir(&working_dir)
            .stdin(std::process::Stdio::null())  // Isolate from REPL stdin
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let mut child = match output_result {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Error: {}", e)),
        };

        // Apply timeout using wait_with_timeout pattern
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let start = std::time::Instant::now();
        let timed_out = loop {
            match child.try_wait() {
                Ok(Some(_)) => break false,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        break true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break false,
            }
        };

        if timed_out {
            return ToolResult::error(format!(
                "Error: command timed out after {}s: {}",
                timeout_secs, command
            ));
        }

        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => return ToolResult::error(format!("Error: {}", e)),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("[stderr]\n");
            result.push_str(&stderr);
        }

        // Add exit code
        result.push_str(&format!("\nExit code: {}", exit_code));

        // Truncate if too large
        const MAX_OUTPUT: usize = 50000;
        if result.len() > MAX_OUTPUT {
            let half = MAX_OUTPUT / 2;
            let mut first_end = half;
            while first_end > 0 && !result.is_char_boundary(first_end) { first_end -= 1; }
            let mid_start = result.len() - half;
            let mut mid_end = mid_start;
            while mid_end < result.len() && !result.is_char_boundary(mid_end) { mid_end += 1; }
            let truncated = result.len() - (first_end + (result.len() - mid_end));
            result = format!(
                "{}\n\n... ({} chars truncated) ...\n\n{}",
                &result[..first_end],
                truncated,
                &result[mid_end..]
            );
        }

        if result.is_empty() {
            result = "(no output)".to_string();
        }

        ToolResult {
            output: result,
            is_error: !output.status.success(),
            ..Default::default()
        }
    }
}

// ─── Background execution ───────────────────────────────────────────────────

impl ExecTool {
    /// Execute a command in the background. Delegates to the background callback
    /// if set, otherwise falls back to foreground execution.
    fn exec_in_background(&self, params: &HashMap<String, Value>) -> ToolResult {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.trim().to_string(),
            None => return ToolResult::error("Error: empty command"),
        };

        if command.is_empty() {
            return ToolResult::error("Error: empty command");
        }

        // If no callback is configured, fall back to foreground execution
        if self.background_callback.is_none() {
            return self.exec_foreground(params.clone());
        }

        // Determine working directory
        let working_dir = params
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());

        let callback = self.background_callback.as_ref().unwrap();
        let (task_id, output_file, err_text) = callback(command.clone(), working_dir.clone());

        if !err_text.is_empty() {
            return ToolResult::error(err_text);
        }

        ToolResult::ok(format!(
            "Background task started.\nTask ID: {}\nOutput file: {}\nUse the task_output tool to check results when ready.",
            task_id, output_file
        ))
    }
}

// ─── TaskStopTool ───────────────────────────────────────────────────────────

/// Callback for stopping/killing a background task by ID.
type TaskStopFunc = Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>;

pub struct TaskStopTool {
    stop_func: TaskStopFunc,
}

impl TaskStopTool {
    pub fn new(stop_func: TaskStopFunc) -> Self {
        Self { stop_func }
    }
}

impl Clone for TaskStopTool {
    fn clone(&self) -> Self {
        Self {
            stop_func: Arc::clone(&self.stop_func),
        }
    }
}

impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "task_stop"
    }

    fn description(&self) -> &str {
        "Stop a running background bash task by its ID. Use this to terminate long-running or stuck processes."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task to stop (e.g., 'b3f2a1c4')"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if task_id.is_empty() {
            return ToolResult::error("task_id is required");
        }

        match (self.stop_func)(task_id.to_string()) {
            Ok(()) => ToolResult::ok(format!("Task {} stopped successfully", task_id)),
            Err(e) => ToolResult::error(e),
        }
    }
}

// ─── TaskOutputTool ─────────────────────────────────────────────────────────

/// Callback for reading background task output.
/// (task_id, block, timeout_secs) -> (output, error_text)
type TaskOutputFunc =
    Arc<dyn Fn(String, bool, u64) -> (String, String) + Send + Sync>;

/// task_output reads the output file of a background bash task.
pub struct TaskOutputTool {
    output_func: TaskOutputFunc,
}

impl TaskOutputTool {
    pub fn new(output_func: TaskOutputFunc) -> Self {
        Self { output_func }
    }
}

impl Clone for TaskOutputTool {
    fn clone(&self) -> Self {
        Self {
            output_func: Arc::clone(&self.output_func),
        }
    }
}

impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "task_output"
    }

    fn description(&self) -> &str {
        "Read the output of a background bash task. Returns the full output file content with a status header."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task (e.g., 'b3f2a1c4')"
                },
                "block": {
                    "type": "boolean",
                    "description": "If true, wait for the task to complete before returning (default: false)"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Maximum time to wait when block=true, in seconds (default: 60, max: 600)"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if task_id.is_empty() {
            return ToolResult::error("task_id is required");
        }

        let block = params
            .get("block")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let timeout_secs = params
            .get("timeout")
            .and_then(|v| v.as_i64())
            .unwrap_or(60)
            .clamp(1, 600) as u64;

        let (output, err_text) =
            (self.output_func)(task_id.to_string(), block, timeout_secs);

        if !err_text.is_empty() {
            return ToolResult::error(err_text);
        }

        ToolResult::ok(output)
    }
}

// ─── Helper functions for building callbacks ────────────────────────────────

/// Build a stop callback from a TaskStore.
pub fn make_task_stop_func(task_store: crate::task_store::SharedTaskStore) -> TaskStopFunc {
    Arc::new(move |task_id: String| task_store.kill_task(&task_id))
}

/// Build an output callback from a TaskStore.
/// Returns (output, error_text).
pub fn make_task_output_func(task_store: crate::task_store::SharedTaskStore) -> TaskOutputFunc {
    Arc::new(move |task_id: String, block: bool, timeout_secs: u64| {
        let task_arc = task_store.get_task(&task_id);

        let task_arc = match task_arc {
            Some(t) => t,
            None => {
                return (
                    String::new(),
                    format!("Background task {} not found", task_id),
                );
            }
        };

        // If block is true, wait for task to finish
        if block {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
            loop {
                let is_terminal = {
                    let task = task_arc.lock().unwrap();
                    task.is_terminal()
                };
                if is_terminal {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    let task = task_arc.lock().unwrap();
                    return (
                        format!(
                            "Task {} ({}) -- timeout after {}s (still running, try increasing timeout or check task_output again later)",
                            task_id,
                            task.status,
                            timeout_secs
                        ),
                        String::new(),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }

        // Read output file
        let (output_file, status) = {
            let task = task_arc.lock().unwrap();
            match task.output_file {
                Some(ref path) => (path.clone(), task.status),
                None => {
                    return (
                        String::new(),
                        format!("Task {} has no output file", task_id),
                    );
                }
            }
        };

        let content = match std::fs::read_to_string(&output_file) {
            Ok(c) => c,
            Err(e) => {
                return (
                    String::new(),
                    format!("Error reading output file: {}", e),
                );
            }
        };

        // Truncate if too large
        const MAX_OUTPUT: usize = 50000;
        let output = if content.len() > MAX_OUTPUT {
            let half = MAX_OUTPUT / 2;
            let mid_start = content.len() - half;
            format!(
                "{}\n\n... ({} chars truncated) ...\n\n{}",
                &content[..half],
                content.len() - MAX_OUTPUT,
                &content[mid_start..]
            )
        } else {
            content
        };

        (
            format!("Task {} ({}) -- output:\n{}", task_id, status, output),
            String::new(),
        )
    })
}

/// Build a background callback from a TaskStore.
/// This is called by ExecTool when run_in_background=true.
/// Returns (task_id, output_file, error_text).
pub fn make_bash_bg_callback(
    task_store: crate::task_store::SharedTaskStore,
    notification_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> BashBgTaskCallback {
    Arc::new(move |command: String, working_dir: String| {
        spawn_background_bash(&task_store, &notification_tx, command, working_dir)
    })
}

// ─── Background bash spawning ──────────────────────────────────────────────

/// Spawn a background bash command and register it in the TaskStore.
/// Returns (task_id, output_file, error_text).
fn spawn_background_bash(
    task_store: &crate::task_store::SharedTaskStore,
    notification_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    command: String,
    working_dir: String,
) -> (String, String, String) {
    use crate::task_store::bash_bg_tasks_dir;

    // Determine shell
    let (shell, flag) = detect_shell_inline();

    // Create output directory
    let output_dir = bash_bg_tasks_dir();
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        return (
            String::new(),
            String::new(),
            format!("Error: failed to create task output directory: {}", e),
        );
    }

    // Generate task ID and output file path
    let task_id = {
        let id = uuid::Uuid::new_v4().to_string();
        let hex: String = id.chars().filter(|c| *c != '-').take(8).collect();
        format!("b{}", hex)
    };
    let output_file = output_dir.join(format!("{}.output", task_id));

    // Create/truncate the output file with header
    if let Err(e) = write_output_header(&output_file, &task_id, &command, &working_dir) {
        return (
            String::new(),
            String::new(),
            format!("Error: failed to create output file: {}", e),
        );
    }

    // Register task in the TaskStore
    let output_file_str = output_file.to_string_lossy().to_string();
    task_store.register_bash_bg_task(command.clone(), output_file_str.clone());

    // Spawn a dedicated background thread to run the process
    let task_store_clone = Arc::clone(task_store);
    let notification_tx_clone = notification_tx.clone();
    let output_file_clone = output_file_str.clone();
    let task_id_clone = task_id.clone();
    let command_clone = command.clone();
    let working_dir_clone = working_dir.clone();
    let shell_owned = shell.to_string();
    let flag_owned = flag.to_string();

    std::thread::Builder::new()
        .name(format!("bg-task-{}", task_id))
        .spawn(move || {
            run_background_bash(
                &task_store_clone,
                &notification_tx_clone,
                &task_id_clone,
                &output_file_clone,
                &shell_owned,
                &flag_owned,
                &command_clone,
                &working_dir_clone,
            );
        })
        .expect("failed to spawn background task thread");

    (task_id, output_file_str, String::new())
}

/// Detect shell inline (no caching -- for spawned threads).
fn detect_shell_inline() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        if std::process::Command::new("powershell")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            ("powershell", "-Command")
        } else if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            ("bash", "-c")
        } else {
            ("cmd", "/C")
        }
    } else {
        ("bash", "-c")
    }
}

/// Write the header to the output file.
fn write_output_header(
    path: &std::path::Path,
    task_id: &str,
    command: &str,
    working_dir: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "--- Background Task: {} ---", task_id)?;
    writeln!(f, "Command: {}", command)?;
    writeln!(f, "Working Dir: {}", working_dir)?;
    writeln!(
        f,
        "Started: {}",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
    )?;
    writeln!(f, "--- Output ---\n")?;
    Ok(())
}

/// Run the background bash command in a dedicated thread.
/// Uses std::process::Command and writes output to file.
fn run_background_bash(
    task_store: &crate::task_store::SharedTaskStore,
    notification_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    task_id: &str,
    output_file: &str,
    shell: &str,
    flag: &str,
    command: &str,
    working_dir: &str,
) {
    let start = std::time::Instant::now();

    // Spawn the child process
    let spawn_result = std::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .current_dir(working_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    let mut child = match spawn_result {
        Ok(c) => c,
        Err(e) => {
            let err_text = format!("Error: failed to start command: {}", e);
            let _ = append_to_output_file(output_file, &format!("{}\n", err_text));
            let _ = task_store.fail_task(task_id, &err_text);
            let notification =
                make_notification(task_id, "failed", output_file, command, &err_text);
            let _ = notification_tx.send(notification);
            return;
        }
    };

    // Store the PID in the TaskStore BEFORE calling wait
    // This is the critical fix from the Go version -- Process must be set before Wait()
    let pid = child.id();
    task_store.set_pid(task_id, pid);

    // Wait for the process to complete
    let output_result = child.wait_with_output();
    let elapsed = start.elapsed();

    match output_result {
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let stderr_str = String::from_utf8_lossy(&output.stderr);

            // Write stdout to output file
            if !stdout_str.is_empty() {
                let _ = append_to_output_file(output_file, &stdout_str);
            }
            // Write stderr to output file
            if !stderr_str.is_empty() {
                if !stdout_str.is_empty() {
                    let _ = append_to_output_file(output_file, "\n--- stderr ---\n");
                }
                let _ = append_to_output_file(output_file, &stderr_str);
            }

            // Write footer
            let footer = format!(
                "\n--- Task Complete ---\nExit code: {}\nDuration: {:.2}s\nStatus: {}\n",
                exit_code,
                elapsed.as_secs_f64(),
                if exit_code == 0 {
                    "completed"
                } else {
                    "failed"
                }
            );
            let _ = append_to_output_file(output_file, &footer);

            // Guard: if task was already killed, don't overwrite status
            let already_killed = task_store.is_terminal(task_id);

            if !already_killed {
                if exit_code == 0 {
                    task_store.complete_task(task_id, "Command completed (exit code 0)");
                } else {
                    task_store.fail_task(
                        task_id,
                        &format!("Command failed with exit code {}", exit_code),
                    );
                }
            }

            // Send notification
            let (status_str, summary) = if already_killed {
                ("killed", "Command was stopped".to_string())
            } else if exit_code == 0 {
                ("completed", "Command completed successfully".to_string())
            } else {
                let summary = format!("Command failed (exit code {})", exit_code);
                ("failed", summary)
            };

            let notification =
                make_notification(task_id, status_str, output_file, command, &summary);
            let _ = notification_tx.send(notification);
        }
        Err(e) => {
            let err_text = format!("Error: failed to wait for command: {}", e);
            let _ = append_to_output_file(output_file, &format!("{}\n", err_text));

            // Guard: if already killed, don't overwrite
            if !task_store.is_terminal(task_id) {
                let _ = task_store.fail_task(task_id, &err_text);
            }

            let notification =
                make_notification(task_id, "failed", output_file, command, &err_text);
            let _ = notification_tx.send(notification);
        }
    }
}

/// Append text to the output file.
fn append_to_output_file(path: &str, text: &str) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut f = OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

/// Build an XML task notification string.
fn make_notification(
    task_id: &str,
    status: &str,
    output_file: &str,
    command: &str,
    summary: &str,
) -> String {
    let command_escaped = escape_xml(command);
    let summary_escaped = escape_xml(summary);
    format!(
        r#"<task-notification>
<task_id>{}</task_id>
<task_type>bash_background</task_type>
<status>{}</status>
<output_file>{}</output_file>
<command>{}</command>
<summary>{}</summary>
</task-notification>"#,
        task_id, status, output_file, command_escaped, summary_escaped
    )
}

/// Escape special characters for XML.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
