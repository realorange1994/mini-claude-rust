//! FileEditTool - Edit a file by replaced exact strings

use crate::tools::{Tool, ToolResult, expand_path, restore_crlf};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;

pub struct FileEditTool;

impl FileEditTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileEditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileEditTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string with a new string. \
         You MUST use read_file to read the file at least once before editing. \
         ALWAYS prefer edit_file for modifying existing files — it only sends the diff. \
         The edit will FAIL if old_string is not unique in the file. Provide enough context to uniquely match. \
         Use replace_all to change every instance of old_string."
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
                return ToolResult::error(
                    "Error: cannot create new file - file already exists with content".to_string(),
                );
            }
            if let Some(parent) = path.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return ToolResult::error(format!("Error: {}", e));
                }
            }
            if let Err(e) = fs::write(&path, &new_str) {
                return ToolResult::error(format!("Error writing file: {}", e));
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

        let result = if replace_all {
            content_norm.replace(&old_str_norm, &new_str_norm)
        } else {
            content_norm.replacen(&old_str_norm, &new_str_norm, 1)
        };

        // Preserve original quote style — pass ORIGINAL (pre-normalized) content
        // so curly quotes can be detected in the actual file content
        let result = preserve_quote_style(&result, &content, &old_str, &new_str);

        // Restore CRLF
        let result = if has_crlf {
            restore_crlf(&result)
        } else {
            result
        };

        if let Err(e) = fs::write(&path, &result) {
            return ToolResult::error(format!("Error writing file: {}", e));
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
/// If the file actually contains curly quotes, the replacement also uses curly quotes.
fn preserve_quote_style(result: &str, content_orig: &str, old_str: &str, new_str: &str) -> String {
    // Check if the file actually contains curly quotes
    let has_curly_double = content_orig.contains('\u{201C}') || content_orig.contains('\u{201D}');
    let has_curly_single = content_orig.contains('\u{2018}') || content_orig.contains('\u{2019}');
    if !has_curly_double && !has_curly_single {
        return result.to_string();
    }

    // Detect quote style used in old_str (the matched text in the file)
    let old_has_curly_double = old_str.contains('\u{201C}') || old_str.contains('\u{201D}');
    let old_has_curly_single = old_str.contains('\u{2018}') || old_str.contains('\u{2019}');

    let mut out = result.to_string();
    if old_has_curly_double && has_curly_double {
        out = curly_to_straight_double(&out);
        out = straight_to_curly_double(&out);
    }
    if old_has_curly_single && has_curly_single {
        out = curly_to_straight_single(&out);
        out = straight_to_curly_single(&out);
    }
    out
}

/// Converts curly double quotes to straight double quotes.
fn curly_to_straight_double(s: &str) -> String {
    s.replace('\u{201C}', "\"").replace('\u{201D}', "\"")
}

/// Converts straight double quotes to curly double quotes.
fn straight_to_curly_double(s: &str) -> String {
    s.replace('"', "\u{201C}")
}

/// Converts curly single quotes to straight single quotes.
fn curly_to_straight_single(s: &str) -> String {
    s.replace('\u{2018}', "'").replace('\u{2019}', "'")
}

/// Converts straight single quotes to curly single quotes.
fn straight_to_curly_single(s: &str) -> String {
    s.replace('\'', "\u{2019}")
}

/// Strips trailing whitespace from each line.
fn strip_trailing_whitespace(s: &str) -> String {
    s.lines()
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
