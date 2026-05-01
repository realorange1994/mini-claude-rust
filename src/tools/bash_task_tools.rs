//! Bash task tools -- task_stop and task_output for background bash task management.
//!
//! These tools use callback functions to stay decoupled from the TaskStore,
//! allowing it to be owned by the agent loop while tools are registered in the registry.

use crate::tools::{Tool, ToolResult};
use crate::task_store::SharedTaskStore;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

// ─── TaskStopTool ───────────────────────────────────────────────────────────

/// Callback for stopping/killing a background task by ID.
pub type TaskStopFunc = Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>;

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

    fn input_schema(&self) -> Map<String, Value> {
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
pub type TaskOutputFunc =
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

    fn input_schema(&self) -> Map<String, Value> {
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
pub fn make_task_stop_func(task_store: SharedTaskStore) -> TaskStopFunc {
    Arc::new(move |task_id: String| task_store.kill_task(&task_id))
}

/// Build an output callback from a TaskStore.
/// Returns (output, error_text).
pub fn make_task_output_func(task_store: SharedTaskStore) -> TaskOutputFunc {
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
    task_store: SharedTaskStore,
    notification_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Arc<dyn Fn(String, String) -> (String, String, String) + Send + Sync> {
    Arc::new(move |command: String, working_dir: String| {
        spawn_background_bash(&task_store, &notification_tx, command, working_dir)
    })
}

// ─── Background bash spawning ──────────────────────────────────────────────

/// Spawn a background bash command and register it in the TaskStore.
/// Returns (task_id, output_file, error_text).
fn spawn_background_bash(
    task_store: &SharedTaskStore,
    notification_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    command: String,
    working_dir: String,
) -> (String, String, String) {
    use crate::task_store::bash_bg_tasks_dir;

    // Determine shell
    let (shell, flag) = detect_shell();

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

/// Detect the shell and flag to use.
fn detect_shell() -> (&'static str, &'static str) {
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
    task_store: &SharedTaskStore,
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
