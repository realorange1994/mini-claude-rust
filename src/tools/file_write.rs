//! FileWriteTool - Write content to a file

use crate::tools::{Tool, ToolResult, expand_path, is_unc_path, normalize_file_path, FileReadInfo};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

const MAX_WRITE_SIZE: usize = 10 * 1024 * 1024; // 10MB

pub struct FileWriteTool {
    files_read: Option<Arc<RwLock<HashMap<String, FileReadInfo>>>>,
}

impl FileWriteTool {
    pub fn new() -> Self {
        Self { files_read: None }
    }

    pub fn with_files_read(files_read: Arc<RwLock<HashMap<String, FileReadInfo>>>) -> Self {
        Self { files_read: Some(files_read) }
    }
}

impl Default for FileWriteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileWriteTool {
    fn clone(&self) -> Self {
        Self {
            files_read: self.files_read.clone(),
        }
    }
}

impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Writes a file to the local filesystem.\n\nUsage:\n- This tool will overwrite the existing file if there is one at the provided path.\n- If this is an existing file, you MUST use the read_file tool first to read the file's contents. This tool will fail if you did not read the file first.\n- Prefer the edit_file tool for modifying existing files — it only sends the diff. Only use this tool to create new files or for complete rewrites.\n- NEVER create documentation files (*.md) or README files unless explicitly requested by the User.\n- Only use emojis if the user explicitly requests it. Avoid writing emojis to files unless asked."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to write (must be absolute, not relative)."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let path = params.get("file_path")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("path").and_then(|v| v.as_str()));
        let path = match path {
            Some(p) => expand_path(p),
            None => return ToolResult::error("Error: path is required"),
        };

        // SECURITY: Block UNC paths before any filesystem I/O to prevent NTLM credential leaks.
        if is_unc_path(&path) {
            return ToolResult::error(format!("Error: UNC path access deferred: {}", path.display()));
        }

        // Read-before-write validation and concurrent modification detection.
        if let Some(files_read) = &self.files_read {
            let path_str = normalize_file_path(&path.to_string_lossy());
            if path.exists() {
                let fr = files_read.read().unwrap_or_else(|e| e.into_inner());
                if let Some(info) = fr.get(&path_str) {
                    // File was read — check for concurrent modification
                    if let Ok(meta) = fs::metadata(&path) {
                        if let Ok(modified) = meta.modified() {
                            if modified != info.mtime {
                                drop(fr);
                                return ToolResult::error(
                                    "Error: file has been modified since read, either by the user or a linter. Read it again before attempting to write it.".to_string()
                                );
                            }
                        }
                    }
                    // Partial-view check: if the user read only a portion (with
                    // offset/limit), they must do a fresh full read before writing.
                    let is_partial = info.read_offset != usize::MAX
                        && (info.read_offset != 1 || info.read_limit != usize::MAX);
                    if is_partial {
                        drop(fr);
                        return ToolResult::error(
                            "Error: file was only partially read. You must do a fresh full read (without offset/limit) before writing.".to_string()
                        );
                    }
                } else {
                    // File was not read first
                    drop(fr);
                    return ToolResult::error(
                        "Error: file has not been read yet. Read it first before writing to it.".to_string()
                    );
                }
            }
        }

        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Error: content is required"),
        };

        if content.len() > MAX_WRITE_SIZE {
            return ToolResult::error(format!(
                "Error: content too large ({} bytes, max {} bytes)",
                content.len(),
                MAX_WRITE_SIZE
            ));
        }

        // Create parent directories
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                return ToolResult::error(format!("Error creating directory: {}", e));
            }
        }

        // Write file
        if let Err(e) = fs::write(&path, content) {
            return ToolResult::error(format!("Error writing file: {}", e));
        }

        // Update files_read so subsequent writes are allowed without re-reading
        if let Some(files_read) = &self.files_read {
            let path_str = normalize_file_path(&path.to_string_lossy());
            let mtime = fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let read_time = SystemTime::now();
            files_read.write().unwrap_or_else(|e| e.into_inner()).insert(path_str, FileReadInfo { mtime, read_time, read_offset: usize::MAX, read_limit: usize::MAX, content: content.to_string() });
        }

        ToolResult::ok(format!("Wrote {} chars to {}", content.len(), path.display()))
    }
}

