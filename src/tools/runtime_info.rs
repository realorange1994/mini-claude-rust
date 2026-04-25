//! RuntimeInfoTool - Go runtime and system diagnostics

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;

pub struct RuntimeInfoTool;

impl RuntimeInfoTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuntimeInfoTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for RuntimeInfoTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for RuntimeInfoTool {
    fn name(&self) -> &str {
        "runtime_info"
    }

    fn description(&self) -> &str {
        "Show Go runtime and system information: version, OS, architecture, CPU count, working directory, and memory usage."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, _params: HashMap<String, Value>) -> ToolResult {
        let mut output = String::new();

        output.push_str(&format!("Rust Version: {}\n", rust_version()));
        output.push_str(&format!("OS: {}\n", std::env::consts::OS));
        output.push_str(&format!("Architecture: {}\n", std::env::consts::ARCH));
        output.push_str(&format!("NumCPU: {}\n", num_cpus::get()));
        output.push_str(&format!("NumThreads: {:?}\n", std::thread::available_parallelism()));

        if let Ok(cwd) = std::env::current_dir() {
            output.push_str(&format!("Working Directory: {}\n", cwd.display()));
        }

        // Memory info (platform-dependent)
        #[cfg(target_os = "linux")]
        {
            if let Ok(mem_info) = std::fs::read_to_string("/proc/meminfo") {
                for line in mem_info.lines().take(3) {
                    output.push_str(&format!("{}\n", line));
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            use std::process::Command;
            if let Ok(output_ps) = Command::new("powershell")
                .args(["-NoProfile", "-Command", "Get-CimInstance Win32_OperatingSystem | Select-Object TotalVisibleMemorySize, FreePhysicalMemory | Format-Table -AutoSize"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output_ps.stdout);
                output.push_str(&stdout);
            }
        }

        ToolResult::ok(output.trim().to_string())
    }
}

fn rust_version() -> String {
    if let Ok(output) = std::process::Command::new("rustc")
        .arg("--version")
        .output()
    {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !version.is_empty() {
            return version;
        }
    }
    "unknown".to_string()
}
