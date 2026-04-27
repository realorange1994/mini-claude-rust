//! SystemTool - System information and monitoring

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;

pub struct SystemTool;

impl SystemTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SystemTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for SystemTool {
    fn name(&self) -> &str {
        "system"
    }

    fn description(&self) -> &str {
        "Get system information. Supports uname, df (disk), free (memory), top (processes), uptime, who, w, hostname, and arch. On Windows, uses PowerShell cmdlets."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "System operation: uname, df, free, top, uptime, who, w, hostname, arch",
                    "enum": ["uname", "df", "free", "top", "uptime", "who", "w", "hostname", "arch"]
                },
                "flags": {
                    "type": "string",
                    "description": "Additional flags for the command (Unix only)"
                },
                "lines": {
                    "type": "integer",
                    "description": "Number of lines to show for top (default: 10)"
                }
            },
            "required": ["operation"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let operation = match params.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return ToolResult::error("Error: operation is required"),
        };

        #[cfg(target_os = "windows")]
        {
            match operation {
                "uname" => self.windows_uname(),
                "df" => self.windows_df(),
                "free" => self.windows_free(),
                "top" => self.windows_top(params),
                "uptime" => self.windows_uptime(),
                "who" => self.windows_who(),
                "w" => self.windows_w(),
                "hostname" => self.windows_hostname(),
                "arch" => self.windows_arch(),
                _ => ToolResult::error(format!("Error: unknown operation: {}", operation)),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            match operation {
                "uname" => self.unix_uname(params),
                "df" => self.unix_df(params),
                "free" => self.unix_free(),
                "top" => self.unix_top(params),
                "uptime" => self.unix_uptime(),
                "who" => self.unix_who(),
                "w" => self.unix_w(),
                "hostname" => self.unix_hostname(),
                "arch" => self.unix_arch(),
                _ => ToolResult::error(format!("Error: unknown operation: {}", operation)),
            }
        }
    }
}

impl SystemTool {
    #[cfg(target_os = "windows")]
    fn windows_uname(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $h = $env:COMPUTERNAME; $os = (Get-CimInstance Win32_OperatingSystem).Caption; $v = (Get-CimInstance Win32_OperatingSystem).Version; Write-Output \"Windows $h $os $v\""])
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
    fn windows_df(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-PSDrive -PSProvider FileSystem | Format-Table Name, @{N='Used GB';E={[math]::Round($_.Used/1GB,2)}}, @{N='Free GB';E={[math]::Round($_.Free/1GB,2)}}, @{N='Total GB';E={[math]::Round(($_.Used+$_.Free)/1GB,2)}} -AutoSize"])
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
    fn windows_free(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", "$os = Get-CimInstance Win32_OperatingSystem; $total = [math]::Round($os.TotalVisibleMemorySize/1MB, 2); $free = [math]::Round($os.FreePhysicalMemory/1MB, 2); $used = [math]::Round($total - $free, 2); Write-Output \"              total        used        free      shared  buff/cache   available\"; Write-Output (\"Mem:      {0,8}GB    {1,8}GB    {2,8}GB    {3,8}    {4,8}      {5,8}GB\" -f $total, $used, $free, 0, 0, $free)"])
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
    fn windows_top(&self, params: HashMap<String, Value>) -> ToolResult {
        let lines = params.get("lines").and_then(|v| v.as_i64()).unwrap_or(10);
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("Get-Process | Sort-Object CPU -Descending | Select-Object -First {} Id, ProcessName, CPU, @{{N='Memory MB';E={{[math]::Round($_.WorkingSet64/1MB,1)}}}} | Format-Table -AutoSize", lines)])
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
    fn windows_uptime(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", r#"(Get-Date) - (Get-CimInstance Win32_OperatingSystem).LastBootUpTime | ForEach-Object { $d = [math]::Floor($_.TotalDays); $h = [math]::Floor($_.Hours); $m = $_.Minutes; if ($d -gt 0) { Write-Output "$d days, $h hours, $m minutes" } else { Write-Output "$h hours, $m minutes" } }"#])
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
    fn windows_who(&self) -> ToolResult {
        let output = Command::new("whoami").output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                ToolResult::ok(stdout.trim().to_string())
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_w(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-Process | Where-Object { $_.MainWindowTitle -ne '' } | Select-Object -First 10 Id, ProcessName, CPU, @{N='Window';E={$_.MainWindowTitle}} | Format-Table -AutoSize"])
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
    fn windows_hostname(&self) -> ToolResult {
        let output = Command::new("hostname").output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::error(stdout.trim().to_string())
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_arch(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_Processor).Architecture | ForEach-Object { switch($_){0{'x86'}4{'x64'}5{'ARM'}9{'x64'}12{'ARM64'}default{$_} } }"])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if o.status.success() && !stdout.trim().is_empty() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::ok(std::env::consts::ARCH.to_string())
                }
            }
            Err(_) => ToolResult::ok(std::env::consts::ARCH.to_string()),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_uname(&self, params: HashMap<String, Value>) -> ToolResult {
        let flags = params.get("flags").and_then(|v| v.as_str()).unwrap_or("-a");
        let mut cmd_args = vec!["uname"];
        if !flags.is_empty() {
            cmd_args.extend(flags.split_whitespace());
        }

        let output = Command::new(&cmd_args[0]).args(&cmd_args[1..]).output();
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
    fn unix_df(&self, params: HashMap<String, Value>) -> ToolResult {
        let mut cmd_args = vec!["df", "-h"];
        if let Some(f) = params.get("flags").and_then(|v| v.as_str()) {
            cmd_args.extend(f.split_whitespace());
        }

        let output = Command::new(&cmd_args[0]).args(&cmd_args[1..]).output();
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

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    fn unix_free(&self) -> ToolResult {
        let output = Command::new("free").args(["-h"]).output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::error(stdout.trim().to_string())
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(target_os = "macos")]
    fn unix_free(&self) -> ToolResult {
        // macOS doesn't have `free` — use vm_stat + sysctl
        let page_size = Command::new("sysctl")
            .args(["-n", "hw.pagesize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i64>().ok())
            .unwrap_or(4096);

        let total_mem = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i64>().ok())
            .unwrap_or(0);

        let output = Command::new("vm_stat").output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let mut free_pages: i64 = 0;
                let mut active_pages: i64 = 0;
                let mut inactive_pages: i64 = 0;
                let mut speculative_pages: i64 = 0;
                let mut wired_pages: i64 = 0;
                let mut compressed_pages: i64 = 0;

                for line in stdout.lines() {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 3 {
                        let value = parts[2].trim_end_matches('.');
                        if let Ok(v) = value.parse::<i64>() {
                            if line.contains("Pages free:") {
                                free_pages = v;
                            } else if line.contains("Pages active:") {
                                active_pages = v;
                            } else if line.contains("Pages inactive:") {
                                inactive_pages = v;
                            } else if line.contains("Pages speculative:") {
                                speculative_pages = v;
                            } else if line.contains("Pages wired down:") {
                                wired_pages = v;
                            } else if line.contains("Pages stored in compressor:") {
                                compressed_pages = v;
                            }
                        }
                    }
                }

                let format_bytes = |b: i64| -> String {
                    if b < 1024 {
                        format!("{}B", b)
                    } else if b < 1024 * 1024 {
                        format!("{:.1}KB", b as f64 / 1024.0)
                    } else if b < 1024 * 1024 * 1024 {
                        format!("{:.1}MB", b as f64 / (1024.0 * 1024.0))
                    } else {
                        format!("{:.1}GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
                    }
                };

                let free_mem = (free_pages + speculative_pages) * page_size;
                let used_mem = (active_pages + wired_pages + compressed_pages) * page_size;
                let cache_mem = inactive_pages * page_size;
                let available_mem = free_mem + cache_mem;

                let result = format!(
                    "              total        used        free      shared  buff/cache   available\nMem:      {:>8}    {:>8}    {:>8}    {:>8}    {:>8}    {:>8}",
                    format_bytes(total_mem),
                    format_bytes(used_mem),
                    format_bytes(free_mem),
                    "0",
                    format_bytes(cache_mem),
                    format_bytes(available_mem)
                );
                ToolResult::ok(result)
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
    fn unix_uptime(&self) -> ToolResult {
        let output = Command::new("uptime").output();
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
    fn unix_who(&self) -> ToolResult {
        let output = Command::new("who").output();
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
    fn unix_w(&self) -> ToolResult {
        let output = Command::new("w").output();
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
    fn unix_hostname(&self) -> ToolResult {
        let output = Command::new("hostname").output();
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
    fn unix_arch(&self) -> ToolResult {
        // Try `arch` first (Linux), fall back to `uname -m` (macOS)
        let output = Command::new("arch").output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if o.status.success() && !stdout.trim().is_empty() {
                    return ToolResult::ok(stdout.trim().to_string());
                }
            }
            Err(_) => {}
        }
        // Fallback to uname -m
        let output2 = Command::new("uname").args(["-m"]).output();
        match output2 {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::ok(std::env::consts::ARCH.to_string())
                }
            }
            Err(_) => ToolResult::ok(std::env::consts::ARCH.to_string()),
        }
    }
}
