//! ProcessTool - Process management and monitoring

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;

pub struct ProcessTool;

impl ProcessTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProcessTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ProcessTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "Process management and monitoring. Supports list (ps), kill, pkill, pgrep, top, and pstree operations. On Windows, uses PowerShell cmdlets."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "Operation: list, kill, pkill, pgrep, top, pstree",
                    "enum": ["list", "kill", "pkill", "pgrep", "top", "pstree"]
                },
                "pid": {
                    "type": "integer",
                    "description": "Process ID (for kill)."
                },
                "pattern": {
                    "type": "string",
                    "description": "Process name pattern (for pkill, pgrep)."
                },
                "signal": {
                    "type": "string",
                    "description": "Signal to send (e.g., SIGTERM, SIGKILL, 9). Unix only (default: SIGTERM)."
                },
                "user": {
                    "type": "string",
                    "description": "Filter by user (for list, pgrep)."
                },
                "lines": {
                    "type": "integer",
                    "description": "Number of lines to show for top (default: 10)."
                }
            },
            "required": ["operation"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::Subprocess]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Classifier
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let operation = match params.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return ToolResult::error("Error: operation is required"),
        };

        #[cfg(target_os = "windows")]
        {
            match operation {
                "list" => self.windows_list(params),
                "kill" => self.windows_kill(params),
                "pkill" => self.windows_pkill(params),
                "pgrep" => self.windows_pgrep(params),
                "top" => self.windows_top(params),
                "pstree" => self.windows_pstree(),
                _ => ToolResult::error(format!("Error: unknown operation: {}", operation)),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            match operation {
                "list" => self.unix_list(params),
                "kill" => self.unix_kill(params),
                "pkill" => self.unix_pkill(params),
                "pgrep" => self.unix_pgrep(params),
                "top" => self.unix_top(params),
                "pstree" => self.unix_pstree(),
                _ => ToolResult::error(format!("Error: unknown operation: {}", operation)),
            }
        }
    }
}

impl ProcessTool {
    #[cfg(target_os = "windows")]
    fn windows_list(&self, params: HashMap<String, Value>) -> ToolResult {
        let user = params.get("user").and_then(|v| v.as_str());
        let output = if let Some(user) = user {
            Command::new("powershell")
                .args(["-NoProfile", "-Command", &format!("Get-Process -IncludeUserName | Where-Object {{$_.UserName -like '*{}*'}} | Format-Table -AutoSize", sanitize_ps_input(user))])
                .output()
        } else {
            Command::new("powershell")
                .args(["-NoProfile", "-Command", "Get-Process | Format-Table -AutoSize"])
                .output()
        };

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_kill(&self, params: HashMap<String, Value>) -> ToolResult {
        let pid = match params.get("pid").and_then(|v| v.as_i64()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pid is required for kill"),
        };

        if pid <= 0 {
            return ToolResult::error("Error: pid must be a non-zero integer");
        }

        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("Stop-Process -Id {} -Force", pid)])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    ToolResult::ok(format!("Killed process {}", pid))
                } else {
                    let out = if !stderr.trim().is_empty() {
                        stderr.trim().to_string()
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_pkill(&self, params: HashMap<String, Value>) -> ToolResult {
        let pattern = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pattern is required for pkill"),
        };

        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("Get-Process -Name '*{}*' -ErrorAction SilentlyContinue | Stop-Process -Force", sanitize_ps_input(pattern))])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                let out = stdout.trim().to_string();
                if out.is_empty() {
                    ToolResult::ok(format!("No processes matching '{}' found", pattern))
                } else if o.status.success() {
                    ToolResult::ok(out)
                } else {
                    let err_out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", out, stderr.trim())
                    } else {
                        out
                    };
                    ToolResult::error(err_out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_pgrep(&self, params: HashMap<String, Value>) -> ToolResult {
        let pattern = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pattern is required for pgrep"),
        };

        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("Get-Process -Name '*{}*' -ErrorAction SilentlyContinue | Format-Table Id, ProcessName, CPU, WorkingSet -AutoSize", sanitize_ps_input(pattern))])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if stdout.trim().is_empty() {
                    ToolResult::ok(format!("No processes matching '{}' found", pattern))
                } else if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_top(&self, params: HashMap<String, Value>) -> ToolResult {
        let lines = params.get("lines").and_then(|v| v.as_i64()).unwrap_or(10);
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("Get-Process | Sort-Object CPU -Descending | Select-Object -First {} Id, ProcessName, CPU, WorkingSet64 | Format-Table -AutoSize", lines)])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_pstree(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", r#"Get-Process | Select-Object -First 30 Id, ProcessName, @{N='Parent';E={(Get-CimInstance Win32_Process -Filter "ProcessId=$($_.Id)").ParentProcessId}} | Format-Table -AutoSize"#])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_list(&self, params: HashMap<String, Value>) -> ToolResult {
        let output = if let Some(user) = params.get("user").and_then(|v| v.as_str()) {
            Command::new("ps").args(["-u", user]).output()
        } else {
            Command::new("ps").args(["aux"]).output()
        };

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_kill(&self, params: HashMap<String, Value>) -> ToolResult {
        let pid = match params.get("pid").and_then(|v| v.as_i64()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pid is required for kill"),
        };

        if pid <= 0 {
            return ToolResult::error("Error: pid must be a non-zero integer");
        }

        let signal = params.get("signal").and_then(|v| v.as_str()).unwrap_or("SIGTERM");
        let sig = if signal.parse::<i32>().is_ok() {
            format!("-{}", signal)
        } else {
            format!("-{}", signal.trim_start_matches("SIG"))
        };

        let output = Command::new("kill").args([&sig, &pid.to_string()]).output();

        match output {
            Ok(o) => {
                if o.status.success() {
                    ToolResult::ok(format!("Killed process {}", pid))
                } else {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    ToolResult::error(stderr.trim().to_string())
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_pkill(&self, params: HashMap<String, Value>) -> ToolResult {
        let pattern = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pattern is required for pkill"),
        };

        let signal = params.get("signal").and_then(|v| v.as_str()).unwrap_or("SIGTERM");
        let sig = if signal.parse::<i32>().is_ok() {
            format!("-{}", signal)
        } else {
            format!("-{}", signal.trim_start_matches("SIG"))
        };

        let output = Command::new("pkill").args([&sig, pattern]).output();

        match output {
            Ok(o) => {
                if o.status.success() {
                    ToolResult::ok(format!("Sent {} to processes matching '{}'", signal, pattern))
                } else {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    ToolResult::error(stderr.trim().to_string())
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_pgrep(&self, params: HashMap<String, Value>) -> ToolResult {
        let pattern = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pattern is required for pgrep"),
        };

        let output = Command::new("pgrep").args(["-a", pattern]).output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() && !stdout.trim().is_empty() {
                    ToolResult::ok(stdout.trim().to_string())
                } else if o.status.success() {
                    ToolResult::ok(format!("No processes matching '{}' found", pattern))
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        format!("No processes matching '{}' found", pattern)
                    };
                    ToolResult::ok(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_top(&self, params: HashMap<String, Value>) -> ToolResult {
        let lines = params.get("lines").and_then(|v| v.as_i64()).unwrap_or(10) as usize;
        let output = Command::new("top").args(["-b", "-n", "1"]).output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    let top_lines: Vec<&str> = stdout.lines().take(lines + 6).collect();
                    ToolResult::ok(top_lines.join("\n"))
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_pstree(&self) -> ToolResult {
        let output = Command::new("pstree").output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    let out = if !stderr.trim().is_empty() {
                        format!("{}\n{}", stdout.trim(), stderr.trim())
                    } else {
                        stdout.trim().to_string()
                    };
                    ToolResult::error(out)
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }
}

fn sanitize_ps_input(s: &str) -> String {
    // Strip PowerShell metacharacters to prevent command injection.
    // Matches Go's sanitizePSInput behavior.
    s.chars()
        .filter(|c| {
            !matches!(
                c,
                '\'' | '"' | '`' | '$' | ';' | '&' | '|' | '(' | ')' | '{' | '}' | '<' | '>' | '\n' | '\r'
            )
        })
        .collect()
}
