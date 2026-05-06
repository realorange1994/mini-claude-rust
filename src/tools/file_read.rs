//! FileReadTool - Read file contents with optional line range

use crate::tools::{Tool, ToolResult, expand_path, is_unc_path, normalize_file_path, FileReadInfo};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

const MAX_FILE_SIZE: u64 = 256 * 1024; // 256 KB, matching Claude Code official

/// Prefix of the "file unchanged" dedup stub. Re-exports the shared constant from the tools module.
const FILE_UNCHANGED_STUB: &str = super::FILE_UNCHANGED_STUB;

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
        "Reads a file from the local filesystem. You can access any file directly by using this tool.\n\nUsage:\n- The file_path parameter must be an absolute path, not a relative path\n- Small/medium files are read entirely. Files larger than 256KB require offset+limit to read in portions\n- You can optionally specify a line offset and limit to read specific portions of any file\n- Results are returned using cat -n format, with line numbers starting at 1\n- This tool can read Jupyter notebooks (.ipynb files) and returns all cells with their outputs\n- You must read a file before editing it with edit_file or write_file\nNEVER use exec with cat, head, or tail — always use this tool instead."
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

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let path = match params.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => expand_path(p),
            None => return ToolResult::error("Error: file_path is required"),
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

        // Magic bytes detection: detect binary formats by file header signatures.
        // Catches renamed binaries (e.g., malware disguised as .txt).
        if metadata.len() >= 4 {
            if let Ok(mut file) = std::fs::File::open(&path) {
                use std::io::Read;
                let mut header = [0u8; 512];
                if let Ok(n) = file.read(&mut header) {
                    if n >= 4 && is_binary_magic(&header[..n]) {
                        return ToolResult::error("Error: binary file detected (magic bytes mismatch)".to_string());
                    }
                }
            }
        }

        // Parse offset/limit early so we can skip the size check for partial reads.
        // If the user specified offset and/or limit, they're reading a portion — allow it
        // even for large files (matching upstream behavior).
        let has_explicit_offset = params.contains_key("offset");
        let has_explicit_limit = params.contains_key("limit");
        let is_partial_request = has_explicit_offset && has_explicit_limit;

        let offset = params
            .get("offset")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok())))
            .map(|v| if v < 1 { 1 } else { v as usize })
            .unwrap_or(1);

        // Only enforce file size limit for full-file reads.
        // Partial reads (with offset/limit) are allowed for large files.
        if !is_partial_request && metadata.len() > MAX_FILE_SIZE {
            return ToolResult::error("Error: file too large (>256 KB). Use offset and limit parameters to read specific portions.".to_string());
        }

        // Read the file content with automatic encoding detection
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        let content = if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
            // UTF-16 LE BOM — decode to UTF-8
            // Skip BOM (2 bytes), then decode pairs. If odd byte count after BOM,
            // ignore the trailing byte (incomplete UTF-16 code unit).
            let data = &bytes[2..];
            let even_len = data.len() & !1; // round down to even
            let u16s: Vec<u16> = data[..even_len]
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect();
            String::from_utf16_lossy(&u16s)
        } else {
            // UTF-8 (or ASCII)
            let s = String::from_utf8_lossy(&bytes).into_owned();
            // Strip UTF-8 BOM (matching official Claude Code behavior)
            s.strip_prefix('\u{FEFF}').unwrap_or(&s).to_string()
        };
        let content = content.replace("\r\n", "\n");
        let mut lines: Vec<&str> = content.lines().collect();

        // Remove trailing empty element
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }

        let total = lines.len();

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
                // Only dedup entries from a prior Read (edit/write entries reflect post-edit
                // mtime, so deduping against them would wrongly point the model at the pre-edit Read content).
                let is_from_read = info.from_read;
                if is_from_read {
                    let range_match = info.read_offset == offset && info.read_limit == limit;
                    if range_match {
                        if let Ok(meta) = fs::metadata(&path) {
                            if let Ok(modified) = meta.modified() {
                                if modified == info.mtime {
                                    // File unchanged — return stub
                                    return ToolResult::ok(format!(
                                        "{} The content from the earlier read_file tool_result in this conversation is still current — refer to that instead of re-reading.",
                                        FILE_UNCHANGED_STUB
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
                // For full-file reads (offset=1, limit covers entire file), store sentinel values
                // so edit/write tools recognize it as a full read. Partial reads keep actual values.
                let is_full_read = offset == 1 && limit >= total;
                let (stored_offset, stored_limit) = if is_full_read {
                    (usize::MAX, usize::MAX)
                } else {
                    (offset, limit)
                };
                let stored_content = if is_full_read { content.clone() } else { String::new() };
                files_read.write().unwrap_or_else(|e| e.into_inner()).insert(path_str, FileReadInfo {
                    mtime,
                    read_time,
                    read_offset: stored_offset,
                    read_limit: stored_limit,
                    content: stored_content,
                    is_partial: !is_full_read,
                    from_read: true,
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

/// Checks file header magic bytes to detect binary files regardless of extension.
/// Catches renamed/misleading files (e.g., malware disguised as .txt).
fn is_binary_magic(header: &[u8]) -> bool {
    if header.is_empty() {
        return false;
    }

    // 2-byte signatures
    if header.len() >= 2 {
        // PE/EXE/DLL: 4d 5a ("MZ")
        if header[0] == b'M' && header[1] == b'Z' { return true; }
        // GZIP: 1f 8b
        if header[0] == 0x1f && header[1] == 0x8b { return true; }
        // BZIP2: 42 5a ("BZ")
        if header[0] == b'B' && header[1] == b'Z' { return true; }
        // MP3 without ID3: ff fb or ff f3 or ff f2
        if header[0] == 0xff && matches!(header[1], 0xfb | 0xf3 | 0xf2) { return true; }
    }

    // 3-byte signatures
    if header.len() >= 3 {
        // MP3 ID3v2: 49 44 33 ("ID3")
        if header[0] == b'I' && header[1] == b'D' && header[2] == b'3' { return true; }
        // JPEG: ff d8 ff
        if header[0] == 0xff && header[1] == 0xd8 && header[2] == 0xff { return true; }
    }

    // Need at least 4 bytes for most signatures
    if header.len() < 4 { return false; }

    // ELF executable: 7f 45 4c 46
    if header[0] == 0x7f && header[1] == b'E' && header[2] == b'L' && header[3] == b'F' { return true; }

    // PDF: 25 50 44 46 ("%PDF")
    if header[0] == b'%' && header[1] == b'P' && header[2] == b'D' && header[3] == b'F' { return true; }

    // PNG: 89 50 4e 47 0d 0a 1a 0a
    if header[0] == 0x89 && header[1] == b'P' && header[2] == b'N' && header[3] == b'G' { return true; }

    // GIF: 47 49 46 38 ("GIF8")
    if header[0] == b'G' && header[1] == b'I' && header[2] == b'F' && header[3] == b'8' { return true; }

    // ZIP/JAR/DOCX/XLSX/PPTX/ODT/APK: 50 4b 03 04 or 50 4b 05 06 or 50 4b 07 08
    if header[0] == b'P' && header[1] == b'K' {
        if (header[2] == 0x03 && header[3] == 0x04) ||
            (header[2] == 0x05 && header[3] == 0x06) ||
            (header[2] == 0x07 && header[3] == 0x08) {
            return true;
        }
    }

    if header.len() < 6 { return false; }

    // XZ: fd 37 7a 58 5a 00
    if header[0] == 0xfd && header[1] == b'7' && header[2] == b'z' && header[3] == b'X' && header[4] == b'Z' && header[5] == 0x00 { return true; }

    // 7Z: 37 7a bc af 27 1c
    if header[0] == b'7' && header[1] == b'z' && header[2] == 0xbc && header[3] == 0xaf && header[4] == 0x27 && header[5] == 0x1c { return true; }

    if header.len() < 8 { return false; }

    // WebP: 52 49 46 46 ... 57 45 42 50 ("RIFF....WEBP")
    if header[0] == b'R' && header[1] == b'I' && header[2] == b'F' && header[3] == b'F' &&
        header[8] == b'W' && header[9] == b'E' && header[10] == b'B' && header[11] == b'P' { return true; }

    // WAV: 52 49 46 46 ... 57 41 56 45 ("RIFF....WAVE")
    if header[0] == b'R' && header[1] == b'I' && header[2] == b'F' && header[3] == b'F' &&
        header[8] == b'W' && header[9] == b'A' && header[10] == b'V' && header[11] == b'E' { return true; }

    // MP4/M4A/QuickTime: 00 00 00 XX 66 74 79 70
    if header[4] == b'f' && header[5] == b't' && header[6] == b'y' && header[7] == b'p' { return true; }

    // Java .class: ca fe ba be
    if header[0] == 0xca && header[1] == 0xfe && header[2] == 0xba && header[3] == 0xbe { return true; }

    // Wasm: 00 61 73 6d ("\0asm")
    if header[0] == 0x00 && header[1] == b'a' && header[2] == b's' && header[3] == b'm' { return true; }

    // Python .pyc: 0d 0d 0d 0a
    if header[0] == 0x0d && header[1] == 0x0d && header[2] == 0x0d && header[3] == 0x0a { return true; }

    // Lua bytecode: 1b 4c 75 61 ("\033Lua")
    if header[0] == 0x1b && header[1] == b'L' && header[2] == b'u' && header[3] == b'a' { return true; }

    false
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

