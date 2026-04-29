//! ExecTool - Shell command execution with security guards

use crate::tools::{Tool, ToolResult};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use std::sync::OnceLock;

pub struct ExecTool;

impl ExecTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExecTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ExecTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command. On Windows, use PowerShell syntax (`;` to separate commands, not `&&`). Use `curl.exe` instead of `curl` on Windows (curl is alias to Invoke-WebRequest). Use for running scripts, installing packages, git operations, and any shell task. Commands run in the current working directory."
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

        // Check for .git directory destruction
        let git_harmful = [
            r"rm\s+-rf.*\.git",
            r"rm\s+-r.*\.git",
            r"rmdir.*\.git",
            r"del.*\.git",
            r"rmrf.*\.git",
        ];
        for pattern in &git_harmful {
            if Regex::new(pattern).unwrap().is_match(&lower) {
                return Some(ToolResult::error("Command would destroy .git directory"));
            }
        }

        // Check for home directory destruction
        let home_harmful = [
            r"rm\s+-rf\s*~",
            r"rm\s+-rf\s+/home",
            r"rm\s+-rf\s+/",
            r"rm\s+-rf\s+C:\\Users",
            r"del\s+/[fq]\s+\w+\\.*",
        ];
        for pattern in &home_harmful {
            if Regex::new(pattern).unwrap().is_match(&lower) {
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

        // Determine shell: powershell → bash → cmd on Windows (matching Go)
        let (shell, flag) = if cfg!(target_os = "windows") {
            if std::process::Command::new("powershell").output().is_ok() {
                ("powershell", "-Command")
            } else if std::process::Command::new("bash").output().is_ok() {
                ("bash", "-c")
            } else {
                ("cmd", "/C")
            }
        } else {
            ("bash", "-c")
        };

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
