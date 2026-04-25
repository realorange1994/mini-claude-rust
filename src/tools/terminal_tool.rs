//! TerminalTool - Terminal session management (tmux/screen)

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;

pub struct TerminalTool;

impl TerminalTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TerminalTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for TerminalTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for TerminalTool {
    fn name(&self) -> &str {
        "terminal"
    }

    fn description(&self) -> &str {
        "Terminal session management via tmux or screen. Supports list, new, detach, attach, send, kill, and rename operations. Unix/Linux only."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "manager": {
                    "type": "string",
                    "description": "Terminal manager: tmux (default) or screen",
                    "enum": ["tmux", "screen"]
                },
                "operation": {
                    "type": "string",
                    "description": "Operation: list, new, detach, attach, send, kill, rename",
                    "enum": ["list", "new", "detach", "attach", "send", "kill", "rename"]
                },
                "session": {
                    "type": "string",
                    "description": "Session name (for attach, send, kill, rename)"
                },
                "command": {
                    "type": "string",
                    "description": "Command to send to session (for send operation)"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for new session"
                },
                "new_name": {
                    "type": "string",
                    "description": "New session name (for rename operation)"
                }
            },
            "required": ["operation"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        #[cfg(target_os = "windows")]
        {
            return Some(ToolResult::error(
                "Error: terminal tool is not supported on Windows. It requires tmux or screen which are Unix/Linux tools.",
            ));
        }
        #[cfg(not(target_os = "windows"))]
        None
    }

    fn execute(&self, #[allow(unused_variables)] params: HashMap<String, Value>) -> ToolResult {
        #[cfg(target_os = "windows")]
        {
            return ToolResult::error(
                "Error: terminal tool is not supported on Windows. It requires tmux or screen which are Unix/Linux tools.",
            );
        }

        #[cfg(not(target_os = "windows"))]
        {
            let manager = params
                .get("manager")
                .and_then(|v| v.as_str())
                .unwrap_or("tmux");

            let operation = match params.get("operation").and_then(|v| v.as_str()) {
                Some(op) => op,
                None => return ToolResult::error("Error: operation is required"),
            };

            match self.run_terminal_command(manager, operation, &params) {
                Ok(output) => {
                    if output.is_empty() {
                        ToolResult::ok("(no output)".to_string())
                    } else {
                        ToolResult::ok(output)
                    }
                }
                Err(e) => ToolResult::error(e),
            }
        }
    }
}

impl TerminalTool {
    #[cfg(not(target_os = "windows"))]
    fn run_terminal_command(&self, manager: &str, operation: &str, params: &HashMap<String, Value>) -> Result<String, String> {
        let session = params.get("session").and_then(|v| v.as_str());
        let cwd = params.get("cwd").and_then(|v| v.as_str());
        let new_name = params.get("new_name").and_then(|v| v.as_str());
        let command = params.get("command").and_then(|v| v.as_str());

        let mut cmd = match operation {
            "list" => {
                if manager == "tmux" {
                    Command::new("tmux").args(["list-sessions"])
                } else {
                    Command::new("screen").args(["-ls"])
                }
            }
            "new" => {
                let session_name = session.unwrap_or("main");
                if manager == "tmux" {
                    let mut c = Command::new("tmux");
                    c.args(["new-session", "-s", session_name]);
                    if let Some(cwd) = cwd {
                        c.arg("-c").arg(cwd);
                    }
                    c
                } else {
                    let mut c = Command::new("screen");
                    c.arg("-S").arg(session_name);
                    if let Some(cwd) = cwd {
                        c.arg("-c").arg(cwd);
                    }
                    c
                }
            }
            "attach" => {
                let session = session.ok_or("session name is required for attach")?;
                if manager == "tmux" {
                    Command::new("tmux").args(["attach-session", "-t", session])
                } else {
                    Command::new("screen").args(["-r", session])
                }
            }
            "detach" => {
                if manager == "tmux" {
                    Command::new("tmux").args(["detach-client"])
                } else {
                    Command::new("screen").args(["-d"])
                }
            }
            "send" => {
                let session = session.ok_or("session name is required for send")?;
                let command = command.ok_or("command is required for send")?;
                if manager == "tmux" {
                    Command::new("tmux").args(["send-keys", "-t", session, command, "Enter"])
                } else {
                    Command::new("screen").args(["-S", session, "-X", "stuff", &format!("{}\n", command)])
                }
            }
            "kill" => {
                let session = session.ok_or("session name is required for kill")?;
                if manager == "tmux" {
                    Command::new("tmux").args(["kill-session", "-t", session])
                } else {
                    Command::new("screen").args(["-S", session, "-X", "quit"])
                }
            }
            "rename" => {
                let session = session.ok_or("session name is required for rename")?;
                let new_name = new_name.ok_or("new_name is required for rename")?;
                if manager == "tmux" {
                    Command::new("tmux").args(["rename-session", "-t", session, new_name])
                } else {
                    Command::new("screen").args(["-S", session, "-X", "sessionname", new_name])
                }
            }
            _ => return Err(format!("unknown operation: {}", operation)),
        };

        let output = cmd
            .output()
            .map_err(|e| format!("Error: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&stderr);
        }

        if !output.status.success() {
            return Err(result);
        }

        Ok(result)
    }
}
