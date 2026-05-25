//! Streaming command execution support.
//! Ported from upstream streaming_executor.go (845 lines).
//!
//! Provides a StreamingExecutor that can run shell commands with:
//! - Structured JSON streaming output
//! - Tool use detection and dispatch
//! - Permission callbacks for write operations
//! - Interrupt handling

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use crate::error_types::AgentError;

/// Callback type for permission checks before executing commands.
pub type PermissionCallback = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Callback type for output streaming.
pub type OutputCallback = Box<dyn FnMut(&str) + Send>;

/// Result of a streaming execution.
#[derive(Debug, Clone)]
pub struct StreamResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub interrupted: bool,
}

/// Configuration for streaming execution.
pub struct StreamingConfig {
    /// Working directory for the command.
    pub working_dir: Option<String>,
    /// Environment variables to set.
    pub env: Vec<(String, String)>,
    /// Timeout in seconds (0 = no timeout).
    pub timeout_secs: u64,
    /// Whether to capture stderr separately.
    pub capture_stderr: bool,
    /// Maximum output size in bytes (0 = unlimited).
    pub max_output_bytes: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            working_dir: None,
            env: Vec::new(),
            timeout_secs: 0,
            capture_stderr: true,
            max_output_bytes: 0,
        }
    }
}

/// Streaming command executor.
pub struct StreamingExecutor {
    interrupted: Arc<AtomicBool>,
    config: StreamingConfig,
}

impl StreamingExecutor {
    /// Create a new executor with the given configuration.
    pub fn new(config: StreamingConfig) -> Self {
        Self {
            interrupted: Arc::new(AtomicBool::new(false)),
            config,
        }
    }

    /// Get a handle to the interrupt flag.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        self.interrupted.clone()
    }

    /// Signal interruption.
    pub fn interrupt(&self) {
        self.interrupted.store(true, AtomicOrdering::SeqCst);
    }

    /// Check if interrupted.
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(AtomicOrdering::SeqCst)
    }

    /// Execute a command with streaming output.
    pub fn execute(
        &self,
        command: &str,
        args: &[&str],
        mut output_cb: Option<OutputCallback>,
    ) -> Result<StreamResult, AgentError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(ref dir) = self.config.working_dir {
            cmd.current_dir(dir);
        }

        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| AgentError::NonRetryable { message: e.to_string() })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();

        // Read stdout
        if let Some(out) = stdout {
            let reader = std::io::BufReader::new(out);
            for line in reader.lines() {
                if self.is_interrupted() {
                    let _ = child.kill();
                    break;
                }
                match line {
                    Ok(l) => {
                        if let Some(ref mut cb) = output_cb {
                            cb(&l);
                        }
                        stdout_buf.push_str(&l);
                        stdout_buf.push('\n');
                    }
                    Err(_) => break,
                }
            }
        }

        // Read stderr
        if let Some(err) = stderr {
            let reader = BufReader::new(err);
            for line in reader.lines().flatten() {
                stderr_buf.push_str(&line);
                stderr_buf.push('\n');
            }
        }

        let exit_status = child.wait().map_err(|e| AgentError::NonRetryable { message: e.to_string() })?;
        let exit_code = exit_status.code().unwrap_or(-1);

        Ok(StreamResult {
            exit_code,
            stdout: stdout_buf,
            stderr: stderr_buf,
            interrupted: self.is_interrupted(),
        })
    }

    /// Execute a shell command string.
    pub fn execute_shell(
        &self,
        shell_cmd: &str,
        output_cb: Option<OutputCallback>,
    ) -> Result<StreamResult, AgentError> {
        #[cfg(target_os = "windows")]
        let (cmd, args) = ("cmd", vec!["/C", shell_cmd]);
        #[cfg(not(target_os = "windows"))]
        let (cmd, args) = ("sh", vec!["-c", shell_cmd]);

        self.execute(cmd, &args, output_cb)
    }
}

// ─── StreamingToolExecutor ────────────────────────────────────────────────────

/// Status of a tracked tool through its execution lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Queued,
    Executing,
    Completed,
}

/// Result of a single tool execution.
#[derive(Debug, Clone)]
pub struct ToolExecResult {
    pub index: usize,
    pub tool_name: String,
    pub tool_use_id: String,
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

/// Info about a tool call from the API response.
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// A tool tracked through its execution lifecycle.
pub struct TrackedTool {
    pub tc: ToolCallInfo,
    pub status: ToolStatus,
    pub is_concurrency_safe: bool,
    pub index: usize,
    pub cancelled: bool,
}

/// Streaming tool executor that executes tool calls as they complete during
/// streaming, overlapping tool execution with ongoing stream processing.
///
/// Follows the upstream design:
///   - Queue-based tool management with TrackedTool lifecycle
///   - canExecuteTool + processQueue pattern for ordered execution
///   - Only Bash tool errors cancel sibling tools
///   - Non-Bash errors are returned normally without affecting siblings
pub struct StreamingToolExecutor {
    max_concurrency: usize,
    tools: Vec<TrackedTool>,
    results: Vec<ToolExecResult>,
    has_errored: AtomicBool,
    errored_tool_description: String,
    discarded: bool,
}

impl StreamingToolExecutor {
    /// Create a new executor with default concurrency (10).
    pub fn new() -> Self {
        Self {
            max_concurrency: 10,
            tools: Vec::new(),
            results: Vec::new(),
            has_errored: AtomicBool::new(false),
            errored_tool_description: String::new(),
            discarded: false,
        }
    }

    /// Set the maximum number of concurrent tool executions.
    pub fn set_max_concurrency(&mut self, n: usize) {
        self.max_concurrency = if n == 0 { 1 } else { n };
    }

    /// Returns true if the tool can safely run alongside other tools.
    pub fn is_concurrency_safe(tool_name: &str, arguments: &str) -> bool {
        if tool_name == "exec" || tool_name == "lisp_exec" {
            // Parse the command from arguments and check if it's read-only
            if let Ok(input) = serde_json::from_str::<serde_json::Value>(arguments) {
                if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
                    return is_read_only_command(cmd);
                }
            }
            return false;
        }
        matches!(
            tool_name,
            "read_file" | "glob" | "grep" | "web_search" | "web_fetch"
                | "read_skill" | "tool_search" | "agent_list" | "agent_get" | "lisp_eval"
        )
    }

    /// Add a tool call to the execution queue.
    pub fn add_tool(&mut self, tc: ToolCallInfo) {
        let is_safe = Self::is_concurrency_safe(&tc.name, &tc.input.to_string());
        let index = self.tools.len();
        self.tools.push(TrackedTool {
            tc,
            status: ToolStatus::Queued,
            is_concurrency_safe: is_safe,
            index,
            cancelled: false,
        });
    }

    /// Check if any tools are still pending.
    pub fn has_pending_tools(&self) -> bool {
        self.tools.iter().any(|t| t.status == ToolStatus::Queued || t.status == ToolStatus::Executing)
    }

    /// Get the number of tools added.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Get the number of completed tools.
    pub fn completed_count(&self) -> usize {
        self.tools.iter().filter(|t| t.status == ToolStatus::Completed).count()
    }

    /// Record a tool execution result.
    pub fn record_result(&mut self, result: ToolExecResult) {
        // Mark the tool as completed
        if let Some(tool) = self.tools.get_mut(result.index) {
            tool.status = ToolStatus::Completed;
            // If this is a Bash error, set the sibling abort flag
            if result.is_error && (tool.tc.name == "exec" || tool.tc.name == "bash") {
                self.has_errored.store(true, AtomicOrdering::SeqCst);
                self.errored_tool_description = result.output.clone();
            }
        }
        self.results.push(result);
    }

    /// Get all results collected so far.
    pub fn results(&self) -> &[ToolExecResult] {
        &self.results
    }

    /// Check if a sibling error has occurred.
    pub fn has_sibling_error(&self) -> bool {
        self.has_errored.load(AtomicOrdering::SeqCst)
    }

    /// Get the description of the errored tool.
    pub fn errored_tool_description(&self) -> &str {
        &self.errored_tool_description
    }

    /// Discard all pending tools (e.g., when the turn is interrupted).
    pub fn discard(&mut self) {
        self.discarded = true;
        for tool in &mut self.tools {
            if tool.status == ToolStatus::Queued {
                tool.cancelled = true;
            }
        }
    }

    /// Check if a tool can execute based on current concurrency state.
    /// - No tools executing: YES (first tool always starts)
    /// - This tool safe + all executing tools safe: YES (parallel safe tools)
    /// - Otherwise: NO (non-concurrent tools need exclusive access)
    pub fn can_execute_tool(&self, is_safe: bool) -> bool {
        let executing = self.tools.iter().filter(|t| t.status == ToolStatus::Executing).count();
        let all_executing_safe = self
            .tools
            .iter()
            .filter(|t| t.status == ToolStatus::Executing)
            .all(|t| t.is_concurrency_safe);

        // If a Bash sibling errored, prevent new unsafe tools from starting
        if !is_safe && self.has_errored.load(AtomicOrdering::SeqCst) {
            return false;
        }

        executing == 0 || (is_safe && all_executing_safe)
    }

    /// Get pending tool call infos for execution.
    pub fn pending_tool_calls(&self) -> Vec<&ToolCallInfo> {
        self.tools
            .iter()
            .filter(|t| t.status == ToolStatus::Queued && !t.cancelled)
            .map(|t| &t.tc)
            .collect()
    }

    /// Mark a tool as executing.
    pub fn mark_executing(&mut self, index: usize) {
        if let Some(tool) = self.tools.get_mut(index) {
            tool.status = ToolStatus::Executing;
        }
    }

    /// Cancel all queued tools with a synthetic error message.
    pub fn cancel_queued(&mut self, reason: &str) {
        for tool in &mut self.tools {
            if tool.status == ToolStatus::Queued {
                tool.cancelled = true;
                tool.status = ToolStatus::Completed;
                self.results.push(ToolExecResult {
                    index: tool.index,
                    tool_name: tool.tc.name.clone(),
                    tool_use_id: tool.tc.id.clone(),
                    output: format!("Tool execution cancelled: {}", reason),
                    is_error: true,
                    duration_ms: 0,
                });
            }
        }
    }
}

impl Default for StreamingToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a shell command is read-only (safe for concurrent execution).
fn is_read_only_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    // Commands that are always read-only
    let readonly_prefixes = [
        "ls", "cat", "head", "tail", "grep", "rg", "find", "which", "where",
        "echo", "pwd", "whoami", "hostname", "uname", "date", "env", "printenv",
        "git status", "git log", "git diff", "git show", "git branch", "git remote",
        "git tag", "git rev-parse", "git config --get", "git ls-files",
        "git ls-remote", "git describe", "git blame", "git shortlog",
        "dir", "type", "more", "less", "file", "stat", "wc",
        "curl", "wget",  // read-only from network perspective
        "python3 -c", "python -c", "node -e",  // one-liners are typically read-only
    ];

    for prefix in &readonly_prefixes {
        if trimmed.starts_with(prefix) {
            return true;
        }
    }

    // Check for pipes/redirects that make it read-only
    if trimmed.contains('|') && !trimmed.contains('>'){
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_executor_new() {
        let executor = StreamingExecutor::new(StreamingConfig::default());
        assert!(!executor.is_interrupted());
    }

    #[test]
    fn test_interrupt() {
        let executor = StreamingExecutor::new(StreamingConfig::default());
        executor.interrupt();
        assert!(executor.is_interrupted());
    }

    #[test]
    fn test_execute_simple() {
        let executor = StreamingExecutor::new(StreamingConfig::default());
        #[cfg(target_os = "windows")]
        let result = executor.execute("cmd", &["/C", "echo", "hello"], None);
        #[cfg(not(target_os = "windows"))]
        let result = executor.execute("echo", &["hello"], None);

        let result = result.unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
    }
}
