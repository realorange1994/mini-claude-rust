//! FileReadTool - Read file contents with optional line range

use crate::tools::{Tool, ToolResult, expand_path, is_unc_path, normalize_file_path, FileReadInfo};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

const MAX_FILE_SIZE: u64 = 256 * 1024; // 256 KB, matching Claude Code official

pub struct FileReadTool {
    files_read: Option<Arc<RwLock<HashMap<String, FileReadInfo>>>>,
}

impl FileReadTool {
    pub fn new() -> Self {
        Self { files_read: None }
    }

    pub fn with_files_read(files_read: Arc<RwLock<HashMap<String, FileReadInfo>>>) -> Self {
        Self { files_read: Some(files_read) }
    }
}

impl Default for FileReadTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileReadTool {
    fn clone(&self) -> Self {
        Self {
            files_read: self.files_read.clone(),
        }
    }
}

impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Reads a file from the local filesystem. You can access any file directly by using this tool.\n\nUsage:\n- The file_path parameter must be an absolute path, not a relative path\n- By default, it reads up to 2000 lines starting from the beginning of the file\n- You can optionally specify a line offset and limit (especially handy for long files), but it's recommended to read the whole file by not providing these parameters\n- Results are returned using cat -n format, with line numbers starting at 1\n- This tool can read Jupyter notebooks (.ipynb files) and returns all cells with their outputs\n- You must read a file before editing it with edit_file or write_file"
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to read."
                },
                "offset": {
                    "type": "integer",
                    "description": "The line number to start reading from. Only provide if the file is too large to read at once."
                },
                "limit": {
                    "type": "integer",
                    "description": "The number of lines to read. Only provide if the file is too large to read at once."
                }
            },
            "required": ["file_path"]
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
            return ToolResult::error(format!(
                "Error: UNC path access deferred: {}",
                path.display()
            ));
        }

        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("Error: file not found: {}", path.display()))
            }
            Err(e) => return ToolResult::error(format!("Error: {}", e)),
        };

        if metadata.is_dir() {
            return ToolResult::error(format!("Error: not a file: {}", path.display()));
        }

        // Block device files that would block indefinitely or produce infinite output
        if is_device_file(&path.to_string_lossy()) {
            return ToolResult::error(format!("Error: cannot read device file: {}", path.display()));
        }

        // Reject binary file extensions (matching official Claude Code behavior)
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if is_binary_extension(ext) {
                return ToolResult::error(format!(
                    "Error: binary file not supported: {}",
                    ext
                ));
            }
        }

        if metadata.len() > MAX_FILE_SIZE {
            return ToolResult::error("Error: file too large (>256 KB). Use offset and limit parameters to read specific portions.".to_string());
        }

        // Read the file content
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        // Strip UTF-8 BOM (matching official Claude Code behavior)
        let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
        let content = content.replace("\r\n", "\n");
        let mut lines: Vec<&str> = content.lines().collect();

        // Remove trailing empty element
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }

        let total = lines.len();

        let offset = params
            .get("offset")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok())))
            .map(|v| if v < 1 { 1 } else { v as usize })
            .unwrap_or(1);

        // Official: limit=0 or missing means read entire file
        let limit = params
            .get("limit")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok())))
            .map(|v| if v <= 0 { total } else { v as usize })
            .unwrap_or(total); // default: read entire file (matching Claude Code official)

        // Dedup: if we've already read this exact range and the file hasn't
        // changed on disk, return a stub instead of re-sending the full content.
        // The earlier Read tool_result is still in context — two full copies
        // waste cache_creation tokens on every subsequent turn.
        if let Some(files_read) = &self.files_read {
            let path_str = normalize_file_path(&path.to_string_lossy());
            let fr = files_read.read().unwrap_or_else(|e| e.into_inner());
            if let Some(info) = fr.get(&path_str) {
                // Only dedup entries from a prior Read (offset is always set by Read).
                // Edit/Write store offset=usize::MAX — their entry reflects post-edit
                // mtime, so deduping against it would wrongly point the model at the
                // pre-edit Read content.
                let is_from_read = info.read_offset != usize::MAX;
                if is_from_read {
                    let range_match = info.read_offset == offset && info.read_limit == limit;
                    if range_match {
                        if let Ok(meta) = fs::metadata(&path) {
                            if let Ok(modified) = meta.modified() {
                                if modified == info.mtime {
                                    // File unchanged — return stub
                                    return ToolResult::ok(format!(
                                        "File unchanged since last read. The content from the earlier read_file tool_result in this conversation is still current — refer to that instead of re-reading."
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        if total == 0 {
            return ToolResult::ok(
                "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>".to_string()
            );
        }
        if offset > total {
            return ToolResult::ok(format!(
                "<system-reminder>Warning: the file exists but is shorter than the provided offset ({}). The file has {} lines.</system-reminder>",
                offset, total
            ));
        }

        let start = offset.saturating_sub(1);
        let end = (start + limit).min(total);
        let selected = &lines[start..end];

        let mut result = String::new();
        for (i, line) in selected.iter().enumerate() {
            result.push_str(&format!("{}\t{}\n", offset + i, line));
        }

        // Add pagination hint
        if end < total {
            result.push_str(&format!(
                "\n\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
                offset,
                end,
                total,
                end + 1
            ));
        } else {
            result.push_str(&format!("\n\n(End of file - {} lines total)", total));
        }

        // Mark file as read so subsequent edit/write operations don't require re-reading
        // Store full content for content-based staleness fallback (matching upstream).
        // Only store content for full-file reads (when limit covers the rest of the file).
        if let Some(files_read) = &self.files_read {
            let path_str = normalize_file_path(&path.to_string_lossy());
            let mtime = fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let read_time = SystemTime::now();
            // Store content only for full reads (offset=1, limit covering rest of file)
            let stored_content = if limit >= total { content.clone() } else { String::new() };
            files_read.write().unwrap_or_else(|e| e.into_inner()).insert(path_str, FileReadInfo {
                mtime,
                read_time,
                read_offset: offset,
                read_limit: limit,
                content: stored_content,
            });
        }

        ToolResult::ok(result.trim_end().to_string())
    }
}

/// Checks if a file extension is a binary format that should be rejected.
/// Official Claude Code proactively rejects binary extensions to avoid reading garbage content.
fn is_binary_extension(ext: &str) -> bool {
    let ext = ext.to_lowercase();
    matches!(
        ext.as_str(),
        // Executables
        "exe" | "dll" | "so" | "dylib" | "com"
        // Archives
        | "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "tgz" | "zst" | "lz4"
        | "cab" | "iso" | "img" | "dmg"
        // Images (without image processing support)
        | "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tiff" | "ico" | "webp" | "svgz"
        | "avif" | "apng"
        // Audio/Video
        | "mp3" | "mp4" | "wav" | "ogg" | "avi" | "mov" | "mkv" | "flac" | "flv"
        | "wmv" | "webm" | "aac" | "wma" | "m4a"
        // Data/compiled
        | "pyc" | "pyo" | "o" | "obj" | "a" | "lib" | "class" | "jar" | "war"
        | "dat" | "bin" | "db" | "sqlite"
        | "pdf" | "docx" | "xlsx" | "pptx"
        | "woff" | "woff2" | "eot" | "ttf"
    )
}

/// Checks if a path is a special device file that should be blocked from reading.
/// These files would block indefinitely (/dev/zero, /dev/stdin) or produce infinite output.
/// Matches official Claude Code behavior.
fn is_device_file(path: &str) -> bool {
    // Normalize to forward slashes and lowercase for comparison
    let normalized = path.replace('\\', "/").to_lowercase();

    // Check for Unix device files
    let device_paths = [
        "/dev/zero", "/dev/random", "/dev/urandom", "/dev/full",
        "/dev/stdin", "/dev/tty", "/dev/console",
        "/dev/stdout", "/dev/stderr",
        "/dev/fd/0", "/dev/fd/1", "/dev/fd/2",
    ];
    for dp in &device_paths {
        if normalized == *dp || normalized.ends_with(dp) {
            return true;
        }
    }

    // Check for /proc/self/fd/ and /proc/<pid>/fd/ patterns
    if normalized.contains("/proc/") && normalized.contains("/fd/") {
        return true;
    }

    false
}

