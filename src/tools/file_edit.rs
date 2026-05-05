//! FileEditTool - Edit a file by replaced exact strings

use crate::tools::{Tool, ToolResult, expand_path, is_unc_path, normalize_file_path, restore_crlf, FileReadInfo};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

pub struct FileEditTool {
    files_read: Option<Arc<RwLock<HashMap<String, FileReadInfo>>>>,
}

impl FileEditTool {
    pub fn new() -> Self {
        Self { files_read: None }
    }

    pub fn with_files_read(files_read: Arc<RwLock<HashMap<String, FileReadInfo>>>) -> Self {
        Self { files_read: Some(files_read) }
    }
}

impl Default for FileEditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileEditTool {
    fn clone(&self) -> Self {
        Self {
            files_read: self.files_read.clone(),
        }
    }
}

impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Performs exact string replacements in files.

Usage:
- You must use read_file at least once in the conversation before editing. This tool will error if you attempt an edit without reading the file.
- When editing text from read_file output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix.
- ALWAYS prefer editing existing files in the codebase. NEVER write new files unless explicitly required.
- Only use emojis if the user explicitly requests it. Avoid adding emojis to files unless asked.
- The edit will FAIL if old_string is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use replace_all to change every instance of old_string.
- Use replace_all for replacing and renaming strings across the file. This parameter is useful if you want to rename a variable for instance."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find. Use empty string to create a new file."
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from old_string)."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false)."
                }
            },
            "required": ["file_path", "old_string", "new_string"]
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
                    // Partial-view check: if the user read only a portion (with
                    // offset/limit), they must do a fresh full read before editing.
                    // This prevents the model from editing based on incomplete content.
                    let is_partial = info.read_offset != usize::MAX
                        && (info.read_offset != 1 || info.read_limit != usize::MAX);
                    if is_partial {
                        drop(fr);
                        return ToolResult::error(
                            "Error: file was only partially read. You must do a fresh full read (without offset/limit) before editing.".to_string()
                        );
                    }
                } else {
                    drop(fr);
                    return ToolResult::error(
                        "Error: file has not been read yet. Read it first before editing it.".to_string()
                    );
                }
            }
        }

        let old_str = params
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let new_str = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Check for identical old/new strings (matching official behavior)
        if old_str == new_str && !old_str.is_empty() {
            return ToolResult::error("Error: old_string and new_string must be different".to_string());
        }

        if old_str.is_empty() {
            // Official: allows creating a new file when old_string is empty
            if path.exists() {
                // Allow writing to an existing empty file (matching upstream behavior)
                if let Ok(existing) = fs::read(&path) {
                    let trimmed = String::from_utf8_lossy(&existing);
                    if trimmed.trim().is_empty() {
                        // Truly empty file — allow overwrite
                    } else {
                        return ToolResult::error(
                            "Error: cannot create new file - file already exists with content".to_string(),
                        );
                    }
                } else {
                    return ToolResult::error(
                        "Error: cannot create new file - file already exists with content".to_string(),
                    );
                }
            }
            if let Some(parent) = path.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return ToolResult::error(format!("Error: {}", e));
                }
            }
            if let Err(e) = fs::write(&path, &new_str) {
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
                files_read.write().unwrap_or_else(|e| e.into_inner()).insert(path_str, FileReadInfo { mtime, read_time, read_offset: usize::MAX, read_limit: usize::MAX });
            }
            return ToolResult::ok(format!("Successfully created {}", path.display()));
        }

        // 1 GiB guard: prevent OOM from loading huge files into memory
        const MAX_EDIT_SIZE: u64 = 1 << 30; // 1 GiB
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > MAX_EDIT_SIZE {
                return ToolResult::error(format!(
                    "Error: file too large ({} bytes, max {} bytes). Use offset/limit to read portions.",
                    meta.len(),
                    MAX_EDIT_SIZE
                ));
            }
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("Error: file not found: {}", path.display()))
            }
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        let has_crlf = content.contains("\r\n");

        // Strip trailing whitespace from new_string (except .md/.mdx) matching official
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let new_str = if ext != "md" && ext != "mdx" {
            strip_trailing_whitespace(new_str)
        } else {
            new_str.to_string()
        };

        // Normalize curly quotes to straight quotes for matching (matching official Claude Code).
        // LLMs often output curly quotes but files use straight quotes.
        let content_norm = normalize_quotes(&content);
        let old_str_norm = normalize_quotes(&old_str);
        let new_str_norm = normalize_quotes(&new_str);

        // Normalize CRLF for matching
        let (content_norm, old_str_norm, new_str_norm) = if has_crlf {
            (
                content_norm.replace("\r\n", "\n"),
                old_str_norm.replace("\r\n", "\n"),
                new_str_norm.replace("\r\n", "\n"),
            )
        } else {
            (content_norm, old_str_norm, new_str_norm)
        };

        let count = content_norm.matches(&old_str_norm).count();
        let (mut count, mut old_str_norm, mut new_str_norm) = if count == 0 {
            // Try desanitized version (matching official: reverse sanitized tokens)
            let desanitized_old = desanitize(&old_str_norm);
            if desanitized_old != old_str_norm {
                let c = content_norm.matches(&desanitized_old).count();
                if c > 0 {
                    (c, desanitized_old, desanitize(&new_str_norm))
                } else {
                    (0, old_str_norm, new_str_norm)
                }
            } else {
                (0, old_str_norm, new_str_norm)
            }
        } else {
            (count, old_str_norm, new_str_norm)
        };
        if count == 0 {
            return ToolResult::error(format!(
                "Error: old_text not found in {}. Verify the file content.",
                path.display()
            ));
        }
        if count > 1 && !replace_all {
            return ToolResult::error(format!(
                "Warning: old_text appears {} times. Provide more context or set replace_all=true.",
                count
            ));
        }

        // When deleting a line (newStr is empty), also strip a trailing \n
        // that follows the oldString in the file (matching upstream).
        // E.g. deleting "  let x = 1;" from "  let x = 1;\n" should remove the orphan \n too.
        let result = if new_str_norm.is_empty() && !old_str_norm.ends_with('\n') {
            let old_with_lf = format!("{}\n", old_str_norm);
            if replace_all {
                content_norm.replace(&old_with_lf, &new_str_norm)
            } else if let Some(idx) = content_norm.find(&old_with_lf) {
                let mut r = content_norm[..idx].to_string();
                r.push_str(&new_str_norm);
                r.push_str(&content_norm[idx + old_with_lf.len()..]);
                r
            } else {
                if replace_all {
                    content_norm.replace(&old_str_norm, &new_str_norm)
                } else {
                    content_norm.replacen(&old_str_norm, &new_str_norm, 1)
                }
            }
        } else if replace_all {
            content_norm.replace(&old_str_norm, &new_str_norm)
        } else {
            content_norm.replacen(&old_str_norm, &new_str_norm, 1)
        };

        // Preserve original quote style
        let result = preserve_quote_style(&result, &old_str, &new_str, &old_str_norm);

        // Restore CRLF
        let result = if has_crlf {
            restore_crlf(&result)
        } else {
            result
        };

        if let Err(e) = fs::write(&path, &result) {
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
            files_read.write().unwrap_or_else(|e| e.into_inner()).insert(path_str, FileReadInfo { mtime, read_time, read_offset: usize::MAX, read_limit: usize::MAX });
        }

        ToolResult::ok(format!("Successfully edited {}", path.display()))
    }
}

/// Converts curly/smart quotes to straight ASCII quotes.
fn normalize_quotes(s: &str) -> String {
    s.replace('\u{201C}', "\"")  // left double curly quote
     .replace('\u{201D}', "\"")  // right double curly quote
     .replace('\u{2018}', "'")   // left single curly quote
     .replace('\u{2019}', "'")   // right single curly quote
}

/// Preserves original quote style in the result.
/// Matching upstream's logic:
/// 1. If old_str === old_str_norm (no normalization), return new_str as-is.
/// 2. If normalization happened, check if the ACTUAL matched text in the file
///    had curly quotes. If so, apply the same curly quote style to new_str.
fn preserve_quote_style(result: &str, old_str: &str, new_str: &str, old_str_norm: &str) -> String {
    // If no normalization was needed, return as-is
    if old_str == old_str_norm {
        return result.to_string();
    }

    // Find the actual matched text in the result (before replacement)
    // The result is after replacement, so find old_str_norm position and extract
    let actual_matched = if let Some(idx) = result.find(old_str_norm) {
        &result[idx..idx + old_str_norm.len()]
    } else {
        return new_str.to_string();
    };

    // Check if the actual matched text has curly quotes
    let has_curly_double = actual_matched.contains('\u{201C}') || actual_matched.contains('\u{201D}');
    let has_curly_single = actual_matched.contains('\u{2018}') || actual_matched.contains('\u{2019}');

    let mut out = new_str.to_string();
    if has_curly_double {
        out = curly_to_straight_double(&out);
        out = straight_to_curly_double(&out);
    }
    if has_curly_single {
        out = curly_to_straight_single(&out);
        out = straight_to_curly_single(&out);
    }
    out
}

/// Converts curly double quotes to straight double quotes.
fn curly_to_straight_double(s: &str) -> String {
    s.replace('\u{201C}', "\"").replace('\u{201D}', "\"")
}

/// Converts curly single quotes to straight single quotes.
fn curly_to_straight_single(s: &str) -> String {
    s.replace('\u{2018}', "'").replace('\u{2019}', "'")
}

/// Converts straight double quotes to curly double quotes,
/// using context (preceding character) to distinguish opening vs closing.
fn straight_to_curly_double(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '"' {
            let prev = if i > 0 { chars[i - 1] } else { '\0' };
            if i == 0 || is_opening_quote_context(prev) {
                result.push('\u{201C}'); // opening double curly quote
            } else {
                result.push('\u{201D}'); // closing double curly quote
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Returns true if the preceding character indicates an opening curly quote.
/// Matches upstream's isOpeningContext exactly.
fn is_opening_quote_context(prev: char) -> bool {
    matches!(prev,
        '(' | '[' | '{' | ' ' | '\t' | '\n' | '\r' |
        '\u{2014}' | // em dash
        '\u{2013}'   // en dash
    )
}

/// Converts straight single quotes to curly single quotes,
/// using context to distinguish opening (apostrophe) vs closing.
fn straight_to_curly_single(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '\'' {
            // Check for contraction: letter-'letter pattern (don't, can't, it's, etc.)
            if i > 0 && i < chars.len() - 1 {
                let prev = chars[i - 1];
                let next = chars[i + 1];
                if prev.is_ascii_alphabetic() && next.is_ascii_alphabetic() {
                    result.push('\u{2019}'); // right single curly (apostrophe)
                    continue;
                }
            }
            let prev = if i > 0 { chars[i - 1] } else { '\0' };
            if i == 0 || is_opening_quote_context(prev) {
                result.push('\u{2018}'); // left single curly quote
            } else {
                result.push('\u{2019}'); // right single curly quote
            }
        } else {
            result.push(c);
        }
    }
    result
}


/// Strips trailing whitespace from each line.
fn strip_trailing_whitespace(s: &str) -> String {
    // Use split('\n') which keeps trailing empty strings (unlike .lines() which drops them).
    // "hello\n" → ["hello", ""] → "hello\n" ✓
    // "hello"   → ["hello"]     → "hello"   ✓
    s.split('\n')
        .map(|line| line.trim_end_matches(|c| c == ' ' || c == '\t'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Desanitized token mappings.
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
