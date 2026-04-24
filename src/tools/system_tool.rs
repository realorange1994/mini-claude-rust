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
    fn windows_df(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-PSDrive -PSProvider FileSystem | Format-Table Name, @{N='Used GB';E={[math]::Round($_.Used/1GB,2)}}, @{N='Free GB';E={[math]::Round($_.Free/1GB,2)}}, @{N='Total GB';E={[math]::Round(($_.Used+$_.Free)/1GB,2)}} -AutoSize"])
            .output();
        
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
    fn windows_free(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", "$os = Get-CimInstance Win32_OperatingSystem; $total = [math]::Round($os.TotalVisibleMemorySize/1MB, 2); $free = [math]::Round($os.FreePhysicalMemory/1MB, 2); $used = [math]::Round($total - $free, 2); Write-Output \"              total        used        free      shared  buff/cache   available\"; Write-Output (\"Mem:      {0,8}GB    {1,8}GB    {2,8}GB    {3,8}    {4,8}      {5,8}GB\" -f $total, $used, $free, 0, 0, $free)"])
            .output();
        
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
    fn windows_top(&self, params: HashMap<String, Value>) -> ToolResult {
        let lines = params.get("lines").and_then(|v| v.as_i64()).unwrap_or(10);
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("Get-Process | Sort-Object CPU -Descending | Select-Object -First {} Id, ProcessName, CPU, @{{N='Memory MB';E={{[math]::Round($_.WorkingSet64/1MB,1)}}}} | Format-Table -AutoSize", lines)])
            .output();
        
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
    fn windows_uptime(&self) -> ToolResult {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", r#"(Get-Date) - (Get-CimInstance Win32_OperatingSystem).LastBootUpTime | ForEach-Object { $d = [math]::Floor($_.TotalDays); $h = [math]::Floor($_.Hours); $m = $_.Minutes; if ($d -gt 0) { Write-Output "$d days, $h hours, $m minutes" } else { Write-Output "$h hours, $m minutes" } }"#])
            .output();
        
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
        let mut args = vec!["uname".to_string()];
        let flags = params.get("flags").and_then(|v| v.as_str()).unwrap_or("-a");
        if !flags.is_empty() {
            args.extend(flags.split_whitespace().map(String::from));
        }
        
        let output = Command::new(&args[0]).args(&args[1..]).output();
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

    #[cfg(not(target_os = "windows"))]
    fn unix_df(&self, params: HashMap<String, Value>) -> ToolResult {
        let mut args = vec!["df".to_string(), "-h".to_string()];
        let flags = params.get("flags").and_then(|v| v.as_str());
        if let Some(f) = flags {
            args.extend(f.split_whitespace().map(String::from));
        }
        
        let output = Command::new(&args[0]).args(&args[1..]).output();
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

    #[cfg(not(target_os = "windows"))]
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

    #[cfg(not(target_os = "windows"))]
    fn unix_top(&self, params: HashMap<String, Value>) -> ToolResult {
        let lines = params.get("lines").and_then(|v| v.as_i64()).unwrap_or(10) as usize;
        let output = Command::new("top").args(["-b", "-n", "1"]).output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if o.status.success() {
                    let lines: Vec<&str> = stdout.lines().take(lines + 6).collect();
                    ToolResult::ok(lines.join("\n"))
                } else {
                    ToolResult::error(stdout.trim().to_string())
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
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::error(stdout.trim().to_string())
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
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::error(stdout.trim().to_string())
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
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::error(stdout.trim().to_string())
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
                if o.status.success() {
                    ToolResult::ok(stdout.trim().to_string())
                } else {
                    ToolResult::error(stdout.trim().to_string())
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unix_arch(&self) -> ToolResult {
        let output = Command::new("uname").args(["-m"]).output();
        match output {
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
