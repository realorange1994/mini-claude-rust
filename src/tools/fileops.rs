//! FileOpsTool - File and directory operations

use crate::tools::{Tool, ToolResult, expand_path};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub struct FileOpsTool;

impl FileOpsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileOpsTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileOpsTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for FileOpsTool {
    fn name(&self) -> &str {
        "fileops"
    }

    fn description(&self) -> &str {
        "File and directory operations. Supports mkdir, rm, rmrf (recursive remove), mv, cp, cpdir (recursive copy), chmod, and ln (symbolic/hard links)."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "Operation: mkdir, rm, rmrf, mv, cp, cpdir, chmod, ln",
                    "enum": ["mkdir", "rm", "rmrf", "mv", "cp", "cpdir", "chmod", "ln"]
                },
                "path": {
                    "type": "string",
                    "description": "Path for the operation."
                },
                "destination": {
                    "type": "string",
                    "description": "Destination path (for mv, cp, ln)."
                },
                "mode": {
                    "type": "string",
                    "description": "Permission mode (for mkdir/chmod, e.g. 755, 644)."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Create parent directories (for mkdir)."
                },
                "force": {
                    "type": "boolean",
                    "description": "Force operation (for ln)."
                },
                "symbolic": {
                    "type": "boolean",
                    "description": "Create symbolic link instead of hard link (for ln, default: true)."
                }
            },
            "required": ["operation", "path"]
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

        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => expand_path(p),
            None => return ToolResult::error("Error: path is required"),
        };

        match operation {
            "mkdir" => self.op_mkdir(&path, params),
            "rm" => self.op_remove(&path),
            "rmrf" => self.op_remove_all(&path),
            "mv" => self.op_move(&path, params),
            "cp" => self.op_copy(&path, params),
            "cpdir" => self.op_copy_dir(&path, params),
            "chmod" => self.op_chmod(&path, params),
            "ln" => self.op_link(&path, params),
            _ => ToolResult::error(format!("Error: unknown operation: {}", operation)),
        }
    }
}

impl FileOpsTool {
    #[allow(unused_variables)]
    fn op_mkdir(&self, path: &Path, params: HashMap<String, Value>) -> ToolResult {
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .and_then(|m| parse_mode(m))
            .unwrap_or(0o755);

        if let Err(e) = fs::create_dir_all(path) {
            return ToolResult::error(format!("Error creating directory: {}", e));
        }

        // Try to set mode (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = PermissionsExt::from_mode(mode);
            let _ = fs::set_permissions(path, permissions);
        }

        ToolResult::ok(format!("Created directory: {}", path.display()))
    }

    fn op_remove(&self, path: &Path) -> ToolResult {
        if let Err(e) = fs::remove_file(path) {
            return ToolResult::error(format!("Error removing: {}", e));
        }
        ToolResult::ok(format!("Removed: {}", path.display()))
    }

    fn op_remove_all(&self, path: &Path) -> ToolResult {
        let path_str = path.display().to_string();

        // Protect system directories
        let normalized = path_str.replace('\\', "/");
        if normalized == "/"
            || normalized == "."
            || normalized == "./"
            || normalized == ".\\"
            || normalized.ends_with("/.git")
            || normalized.ends_with("\\.git")
            || normalized.ends_with("/~")
            || normalized.ends_with("\\~")
        {
            return ToolResult::error("Cannot remove protected path (root, .git, or home directory)");
        }

        if let Err(e) = fs::remove_dir_all(path) {
            return ToolResult::error(format!("Error removing recursively: {}", e));
        }
        ToolResult::ok(format!("Removed recursively: {}", path.display()))
    }

    fn op_move(&self, src: &Path, params: HashMap<String, Value>) -> ToolResult {
        let dest = match params.get("destination").and_then(|v| v.as_str()) {
            Some(d) => expand_path(d),
            None => return ToolResult::error("Error: destination is required for mv"),
        };

        if let Err(e) = fs::rename(src, &dest) {
            // Cross-device: fall back to copy + remove
            if let Err(e2) = copy_path(src, &dest) {
                return ToolResult::error(format!("Error moving: {} (copy failed: {})", e, e2));
            }
            let _ = fs::remove_dir_all(src);
        }

        ToolResult::ok(format!("Moved {} to {}", src.display(), dest.display()))
    }

    fn op_copy(&self, src: &Path, params: HashMap<String, Value>) -> ToolResult {
        let dest = match params.get("destination").and_then(|v| v.as_str()) {
            Some(d) => expand_path(d),
            None => return ToolResult::error("Error: destination is required for cp"),
        };

        if let Err(e) = copy_file(src, &dest) {
            return ToolResult::error(format!("Error copying: {}", e));
        }

        ToolResult::ok(format!("Copied {} to {}", src.display(), dest.display()))
    }

    fn op_copy_dir(&self, src: &Path, params: HashMap<String, Value>) -> ToolResult {
        let dest = match params.get("destination").and_then(|v| v.as_str()) {
            Some(d) => expand_path(d),
            None => return ToolResult::error("Error: destination is required for cpdir"),
        };

        if let Err(e) = copy_path(src, &dest) {
            return ToolResult::error(format!("Error copying directory: {}", e));
        }

        ToolResult::ok(format!(
            "Copied directory {} to {}",
            src.display(),
            dest.display()
        ))
    }

    #[allow(unused_variables)]
    fn op_chmod(&self, path: &Path, params: HashMap<String, Value>) -> ToolResult {
        let mode_str = params.get("mode").and_then(|v| v.as_str());
        let mode = match mode_str.and_then(parse_mode) {
            Some(m) => m,
            None => {
                return ToolResult::error(format!("Error: invalid mode: {:?}", mode_str));
            }
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = fs::set_permissions(path, PermissionsExt::from_mode(mode)) {
                return ToolResult::error(format!("Error changing mode: {}", e));
            }

            ToolResult::ok(format!(
                "Changed mode of {} to {}",
                path.display(),
                mode_str.unwrap_or("0644")
            ))
        }

        #[cfg(windows)]
        {
            let _ = mode;
            let _ = mode_str;
            let _ = path;
            ToolResult::error("Error: chmod is not supported on Windows")
        }
    }

    fn op_link(&self, path: &Path, params: HashMap<String, Value>) -> ToolResult {
        let dest = match params.get("destination").and_then(|v| v.as_str()) {
            Some(d) => expand_path(d),
            None => return ToolResult::error("Error: destination is required for ln"),
        };

        let symbolic = params
            .get("symbolic")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let link_type = if symbolic { "symbolic link" } else { "hard link" };

        // Remove existing destination if force (matching Go)
        if force {
            let _ = fs::remove_file(&dest);
            let _ = fs::remove_dir_all(&dest);
        }

        #[cfg(unix)]
        {
            let result = if symbolic {
                std::os::unix::fs::symlink(path, &dest)
            } else {
                fs::hard_link(path, &dest)
            };

            if let Err(e) = result {
                return ToolResult::error(format!("Error creating {}: {}", link_type, e));
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs as windows_fs;
            if symbolic {
                if let Err(e) = windows_fs::symlink_file(path, &dest) {
                    return ToolResult::error(format!(
                        "Error creating symlink: {} (may require administrator privileges on Windows)",
                        e
                    ));
                }
            } else {
                if let Err(e) = fs::hard_link(path, &dest) {
                    return ToolResult::error(format!("Error creating hard link: {}", e));
                }
            }
        }

        ToolResult::ok(format!(
            "Created {} {} -> {}",
            link_type,
            dest.display(),
            path.display()
        ))
    }
}


fn parse_mode(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Ok(mode) = s.parse::<u32>() {
        return Some(mode);
    }
    // Try octal parsing
    u32::from_str_radix(s, 8).ok()
}

fn copy_file(src: &Path, dest: &Path) -> std::io::Result<()> {
    let data = fs::read(src)?;
    fs::write(dest, data)
}

fn copy_path(src: &Path, dest: &Path) -> std::io::Result<()> {
    let info = fs::metadata(src)?;
    if info.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dest_path = dest.join(entry.file_name());
            copy_path(&src_path, &dest_path)?;
        }
    } else {
        copy_file(src, dest)?;
    }
    Ok(())
}
