//! MultiEditTool - Apply multiple search/replace edits atomically

use crate::tools::{Tool, ToolResult, expand_path, is_unc_path, normalize_file_path, restore_crlf, FileReadInfo};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

pub struct MultiEditTool {
    files_read: Option<Arc<RwLock<HashMap<String, FileReadInfo>>>>,
}

impl MultiEditTool {
    pub fn new() -> Self {
        Self { files_read: None }
    }

    pub fn with_files_read(files_read: Arc<RwLock<HashMap<String, FileReadInfo>>>) -> Self {
        Self { files_read: Some(files_read) }
    }
}

impl Default for MultiEditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for MultiEditTool {
    fn clone(&self) -> Self {
        Self {
            files_read: self.files_read.clone(),
        }
    }
}

impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit"
    }

    fn description(&self) -> &str {
        "Apply multiple search/replace edits to a file atomically. If any edit fails, all are rolled back. You must read the file first with read_file before editing. Accepts a list of {old_string, new_string} pairs."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to edit."
                },
                "edits": {
                    "type": "array",
                    "description": "List of {old_string, new_string} edit operations.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {
                                "type": "string",
                                "description": "Exact text to find."
                            },
                            "new_string": {
                                "type": "string",
                                "description": "Text to replace it with."
                            },
                            "replace_all": {
                                "type": "boolean",
                                "description": "Replace all occurrences of this old_string (default: false)."
                            }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["file_path", "edits"]
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

        // Read-before-write validation and concurrent modification detection.
        if let Some(files_read) = &self.files_read {
            let path_str = normalize_file_path(&path.to_string_lossy());
            if path.exists() {
                let fr = files_read.read().unwrap();
                if let Some(info) = fr.get(&path_str) {
                    if let Ok(meta) = fs::metadata(&path) {
                        if let Ok(modified) = meta.modified() {
                            if modified != info.mtime {
                                drop(fr);
                                return ToolResult::error(
                                    "Error: file has been modified since read, either by the user or a linter. Read it again before attempting to edit.".to_string()
                                );
                            }
                        }
                    }
                } else {
                    drop(fr);
                    return ToolResult::error(
                        "Error: you must read the file with read_file before editing it.".to_string()
                    );
                }
            }
        }

        // 1 GiB guard: stat first to avoid loading huge files into memory
        const MAX_EDIT_SIZE: u64 = 1 << 30;
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > MAX_EDIT_SIZE {
                return ToolResult::error(format!(
                    "Error: file too large ({} bytes, max {} bytes). Use offset/limit to read portions.",
                    meta.len(),
                    MAX_EDIT_SIZE
                ));
            }
        }

        let edits_raw = match params.get("edits") {
            Some(v) => v,
            None => return ToolResult::error("Error: edits is required"),
        };

        let edits_array = match edits_raw.as_array() {
            Some(arr) => arr,
            None => return ToolResult::error("Error: edits must be an array"),
        };

        if edits_array.is_empty() {
            return ToolResult::error("Error: edits must not be empty");
        }

        #[derive(Clone)]
        struct Edit {
            old: String,
            new: String,
            replace_all: bool,
        }

        let mut edits = Vec::new();
        for (i, e) in edits_array.iter().enumerate() {
            let m = match e.as_object() {
                Some(m) => m,
                None => return ToolResult::error(format!("Error: edit {} must be an object", i + 1)),
            };

            let old_str = match m.get("old_string").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s,
                _ => return ToolResult::error(format!("Error: edit {}: old_string must not be empty", i + 1)),
            };

            let new_str = m.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            let replace_all = m.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
            edits.push(Edit {
                old: old_str.to_string(),
                new: new_str.to_string(),
                replace_all,
            });
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("Error: file not found: {}", path.display()))
            }
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        // Normalize CRLF
        let mut content = content.replace("\r\n", "\n");
        let has_crlf = content.contains('\r');

        for edit in &mut edits {
            edit.old = edit.old.replace("\r\n", "\n");
            edit.new = edit.new.replace("\r\n", "\n");
        }

        // Normalize curly quotes (matching official)
        for edit in &mut edits {
            edit.old = normalize_quotes(&edit.old);
            edit.new = normalize_quotes(&edit.new);
        }

        // Track applied new strings for overlapping edit detection
        let mut applied_new_strings: Vec<String> = Vec::new();

        // Dry run: validate all edits and detect overlapping
        let mut test_content = content.clone();
        for (i, edit) in edits.iter().enumerate() {
            let old_trimmed = edit.old.trim_end_matches('\n');

            // Overlapping edit detection: old_string must not be a substring of any previously applied new_string
            for prev_new in &applied_new_strings {
                if !old_trimmed.is_empty() && prev_new.contains(old_trimmed) {
                    return ToolResult::error(format!(
                        "Error: edit {} failed: old_string is a substring of a new_string from a previous edit",
                        i + 1
                    ));
                }
            }

            // Find the edit location
            let idx = find_edit_location(&test_content, &edit.old);
            let mut final_old = edit.old.clone();
            let mut final_new = edit.new.clone();

            if idx < 0 {
                // Try desanitized version
                let desanitized_old = desanitize(&edit.old);
                let desanitized_new = desanitize(&edit.new);
                let desanitized_idx = find_edit_location(&test_content, &desanitized_old);
                if desanitized_idx >= 0 {
                    final_old = desanitized_old;
                    final_new = desanitized_new;
                }
            }

            if idx < 0 && find_edit_location(&test_content, &final_old) < 0 {
                return ToolResult::error(format!(
                    "Error: edit {} failed: old_text not found: {:?}",
                    i + 1,
                    truncate(&edit.old, 80)
                ));
            }

            // Apply in test content
            if edit.replace_all {
                test_content = test_content.replace(&final_old, &final_new);
            } else {
                test_content = test_content.replacen(&final_old, &final_new, 1);
            }
            applied_new_strings.push(edit.new.clone());
        }

        // Apply atomically
        if has_crlf {
            content = restore_crlf(&test_content);
        } else {
            content = test_content;
        }

        if let Err(e) = fs::write(&path, &content) {
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
            files_read.write().unwrap().insert(path_str, FileReadInfo { mtime, read_time, read_offset: usize::MAX, read_limit: usize::MAX });
        }

        ToolResult::ok(format!("Applied {} edits to {}", edits.len(), path.display()))
    }
}

/// Finds old_string in content, first trying exact match, then with trailing newlines stripped.
fn find_edit_location(content: &str, old: &str) -> isize {
    if let Some(idx) = content.find(old) {
        return idx as isize;
    }
    // Try with trailing newlines stripped (matching official)
    let trimmed = old.trim_end_matches('\n');
    if trimmed != old {
        if let Some(idx) = content.find(trimmed) {
            return idx as isize;
        }
    }
    -1
}

/// Desanitized token mappings: sanitized -> original API format.
const DESANITIZATIONS: &[(&str, &str)] = &[
    ("<fnr>", "<function_results>"),
    ("<n>", "<name>"),
    ("</n>", "</name>"),
    ("<o>", "<output>"),
    ("</o>", "</output>"),
    ("<e>", "<error>"),
    ("</e>", "</error>"),
    ("<s>", "<system>"),
    ("</s>", "</system>"),
    ("<r>", "<result>"),
    ("</r>", "</result>"),
    ("< META_START >", "<META_START>"),
    ("< META_END >", "<META_END>"),
    ("< EOT >", "<EOT>"),
    ("< META >", "<META>"),
    ("< SOS >", "<SOS>"),
    ("\n\nH:", "\n\nHuman:"),
    ("\n\nA:", "\n\nAssistant:"),
];

/// Applies all known desanitization reversals to a string.
fn desanitize(s: &str) -> String {
    let mut result = s.to_string();
    for (from, to) in DESANITIZATIONS {
        result = result.replace(from, to);
    }
    result
}

/// Converts curly/smart quotes to straight ASCII quotes.
fn normalize_quotes(s: &str) -> String {
    s.replace('\u{201C}', "\"")
        .replace('\u{201D}', "\"")
        .replace('\u{2018}', "'")
        .replace('\u{2019}', "'")
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}
