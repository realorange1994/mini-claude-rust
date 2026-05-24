// FileEncodingTool provides file reading, writing, and editing with arbitrary text encodings.
// Supports: GBK, GB18030, Latin-1, Windows-1252, Shift-JIS, Big5, EUC-KR, etc.

use crate::tools::{
    Tool, ToolPermissionResult, ToolResult,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Encoding types supported by the tool
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    GBK,
    GB18030,
    Big5,
    ShiftJIS,
    EUCKR,
    EUCJP,
    Latin1,
    Windows1252,
}

impl Encoding {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "utf-8" | "utf8" => Some(Encoding::Utf8),
            "utf-16le" | "utf16le" => Some(Encoding::Utf16Le),
            "utf-16be" | "utf16be" => Some(Encoding::Utf16Be),
            "gbk" | "windows-936" => Some(Encoding::GBK),
            "gb18030" => Some(Encoding::GB18030),
            "big5" => Some(Encoding::Big5),
            "shift_jis" | "shift-jis" | "shiftjis" => Some(Encoding::ShiftJIS),
            "euc-kr" | "euckr" => Some(Encoding::EUCKR),
            "euc-jp" | "eucjp" => Some(Encoding::EUCJP),
            "iso-8859-1" | "latin1" | "latin-1" => Some(Encoding::Latin1),
            "windows-1252" | "cp1252" => Some(Encoding::Windows1252),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::GBK => "gbk",
            Encoding::GB18030 => "gb18030",
            Encoding::Big5 => "big5",
            Encoding::ShiftJIS => "shift_jis",
            Encoding::EUCKR => "euc-kr",
            Encoding::EUCJP => "euc-jp",
            Encoding::Latin1 => "iso-8859-1",
            Encoding::Windows1252 => "windows-1252",
        }
    }
}

/// File metadata including encoding information
#[derive(Debug, Clone)]
pub struct FileEncodingMetadata {
    pub encoding: Encoding,
    pub line_endings: String,
    pub has_bom: bool,
}

impl Default for FileEncodingMetadata {
    fn default() -> Self {
        Self {
            encoding: Encoding::Utf8,
            line_endings: "LF".to_string(),
            has_bom: false,
        }
    }
}

/// Detect encoding from file bytes using BOM and heuristics
fn detect_encoding(data: &[u8]) -> (Encoding, bool) {
    // UTF-16 LE BOM: FF FE
    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFE {
        return (Encoding::Utf16Le, true);
    }
    // UTF-16 BE BOM: FE FF
    if data.len() >= 2 && data[0] == 0xFE && data[1] == 0xFF {
        return (Encoding::Utf16Be, true);
    }
    // UTF-8 BOM: EF BB BF
    if data.len() >= 3 && data[0] == 0xEF && data[1] == 0xbb && data[2] == 0xbf {
        return (Encoding::Utf8, true);
    }

    // Heuristic UTF-16 detection (null byte pattern)
    if detect_utf16_le_without_bom(data) {
        return (Encoding::Utf16Le, false);
    }
    if detect_utf16_be_without_bom(data) {
        return (Encoding::Utf16Be, false);
    }

    // Check if valid UTF-8
    if std::str::from_utf8(data).is_ok() {
        return (Encoding::Utf8, false);
    }

    // Default to GBK for non-UTF-8 content (common for Chinese files)
    (Encoding::GBK, false)
}

fn detect_utf16_le_without_bom(data: &[u8]) -> bool {
    if data.len() < 4 || data.len() % 2 != 0 {
        return false;
    }

    let mut null_second = 0;
    let total_pairs = data.len() / 2;

    for i in 0..total_pairs {
        let lo = data[2 * i];
        let hi = data[2 * i + 1];
        if hi == 0x00 && (lo >= 0x20 && lo <= 0x7e || lo == 0x0a || lo == 0x0d || lo == 0x09) {
            null_second += 1;
        }
    }

    total_pairs > 0 && (null_second as f64 / total_pairs as f64) > 0.70
}

fn detect_utf16_be_without_bom(data: &[u8]) -> bool {
    if data.len() < 4 || data.len() % 2 != 0 {
        return false;
    }

    let mut null_first = 0;
    let total_pairs = data.len() / 2;

    for i in 0..total_pairs {
        let hi = data[2 * i];
        let lo = data[2 * i + 1];
        if hi == 0x00 && (lo >= 0x20 && lo <= 0x7e || lo == 0x0a || lo == 0x0d || lo == 0x09) {
            null_first += 1;
        }
    }

    total_pairs > 0 && (null_first as f64 / total_pairs as f64) > 0.70
}

/// Decode bytes to string using specified encoding
fn decode_content(data: &[u8], encoding: Encoding, has_bom: bool) -> Result<(String, String), String> {
    let line_endings = if data.contains(&b'\r') && data.contains(&b'\n') {
        "CRLF".to_string()
    } else {
        "LF".to_string()
    };

    // Strip BOM if present
    let data = if has_bom {
        match encoding {
            Encoding::Utf8 if data.len() >= 3 && data[0] == 0xef && data[1] == 0xbb && data[2] == 0xbf => {
                &data[3..]
            }
            Encoding::Utf16Le if data.len() >= 4 && data[0] == 0xff && data[1] == 0xfe => &data[2..],
            Encoding::Utf16Be if data.len() >= 4 && data[0] == 0xfe && data[1] == 0xff => &data[2..],
            _ => data,
        }
    } else {
        data
    };

    match encoding {
        Encoding::Utf8 => Ok((String::from_utf8_lossy(data).into_owned(), line_endings)),
        Encoding::Utf16Le => {
            let chars: Vec<u16> = data
                .chunks(2)
                .filter_map(|chunk| {
                    if chunk.len() == 2 {
                        Some(u16::from_le_bytes([chunk[0], chunk[1]]))
                    } else {
                        None
                    }
                })
                .collect();
            Ok((String::from_utf16_lossy(&chars), line_endings))
        }
        Encoding::Utf16Be => {
            let chars: Vec<u16> = data
                .chunks(2)
                .filter_map(|chunk| {
                    if chunk.len() == 2 {
                        Some(u16::from_be_bytes([chunk[0], chunk[1]]))
                    } else {
                        None
                    }
                })
                .collect();
            Ok((String::from_utf16_lossy(&chars), line_endings))
        }
        Encoding::GBK | Encoding::GB18030 => {
            // For GBK/GB18030, try to decode using lossy conversion
            Ok((decode_gbk(data), line_endings))
        }
        _ => {
            // For other encodings, use lossy conversion
            Ok((String::from_utf8_lossy(data).into_owned(), line_endings))
        }
    }
}

/// Simple GBK decoder (lossy conversion for invalid sequences)
fn decode_gbk(data: &[u8]) -> String {
    let mut result = String::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b < 0x80 {
            // ASCII
            result.push(b as char);
            i += 1;
        } else if i + 1 < data.len() {
            // GBK double-byte
            let lead = b;
            let trail = data[i + 1];
            if (lead >= 0x81 && lead <= 0xfe) && (trail >= 0x40 && trail <= 0xfe) {
                // Valid GBK range
                let code = ((lead as u16) << 8) | (trail as u16);
                if let Some(c) = char::from_u32(code as u32) {
                    result.push(c);
                } else {
                    result.push('?');
                }
                i += 2;
            } else {
                result.push(b as char);
                i += 1;
            }
        } else {
            result.push(b as char);
            i += 1;
        }
    }
    result
}

/// Encode string to bytes using specified encoding
fn encode_content(content: &str, encoding: Encoding, line_endings: &str, has_bom: bool) -> Vec<u8> {
    let mut content = content.to_string();

    // Restore line endings
    if line_endings == "CRLF" {
        content = content.replace('\n', "\r\n");
    }

    let bytes = match encoding {
        Encoding::Utf8 => content.into_bytes(),
        Encoding::Utf16Le => {
            let mut result: Vec<u8> = if has_bom { vec![0xFF, 0xFE] } else { Vec::new() };
            for c in content.encode_utf16() {
                let bytes = c.to_le_bytes();
                result.extend_from_slice(&bytes);
            }
            result
        }
        Encoding::Utf16Be => {
            let mut result: Vec<u8> = if has_bom { vec![0xFE, 0xFF] } else { Vec::new() };
            for c in content.encode_utf16() {
                let bytes = c.to_be_bytes();
                result.extend_from_slice(&bytes);
            }
            result
        }
        _ => content.into_bytes(), // For other encodings, output as-is
    };

    bytes
}

pub struct FileEncodingTool {
    files_read_handle: Option<Arc<RwLock<HashMap<String, crate::tools::FileReadInfo>>>>,
}

impl FileEncodingTool {
    pub fn new() -> Self {
        Self {
            files_read_handle: None,
        }
    }

    pub fn with_files_read(
        files_read_handle: Option<Arc<RwLock<HashMap<String, crate::tools::FileReadInfo>>>>,
    ) -> Self {
        Self { files_read_handle }
    }

    fn resolve_path(&self, path_str: &str) -> std::path::PathBuf {
        let path = std::path::Path::new(path_str);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|wd| wd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    }

    fn detect(&self, path: &Path) -> ToolResult {
        match std::fs::read(path) {
            Ok(data) => {
                let (encoding, certain) = detect_encoding(&data);
                let preview = String::from_utf8_lossy(&data[..data.len().min(200)]).to_string();
                let preview = preview.replace('\n', " ").replace('\r', "");
                ToolResult::ok(format!(
                    "Encoding: {} ({} certainty)\nPreview: {}...",
                    encoding.as_str(),
                    if certain { "high" } else { "low" },
                    preview
                ))
            }
            Err(e) => ToolResult::error(format!("Error reading file: {}", e)),
        }
    }

    fn read(&self, path: &Path, encoding_hint: Option<&str>) -> ToolResult {
        match std::fs::read(path) {
            Ok(data) => {
                let (detected_encoding, has_bom) = if let Some(hint) = encoding_hint {
                    (Encoding::from_str(hint).unwrap_or(Encoding::Utf8), false)
                } else {
                    detect_encoding(&data)
                };

                match decode_content(&data, detected_encoding, has_bom) {
                    Ok((content, line_endings)) => {
                        let normalized = content.replace("\r\n", "\n");
                        ToolResult::ok(format!(
                            "Encoding: {}\nLine endings: {}\nHas BOM: {}\n\n{}",
                            detected_encoding.as_str(),
                            line_endings,
                            has_bom,
                            normalized
                        ))
                    }
                    Err(e) => ToolResult::error(format!("Error decoding content: {}", e)),
                }
            }
            Err(e) => ToolResult::error(format!("Error reading file: {}", e)),
        }
    }

    fn write(&self, path: &Path, content: &str, encoding: Option<&str>) -> ToolResult {
        let enc = encoding
            .and_then(Encoding::from_str)
            .unwrap_or(Encoding::Utf8);

        let bytes = encode_content(content, enc, "LF", false);

        match std::fs::write(path, bytes) {
            Ok(_) => ToolResult::ok(format!("File written successfully with encoding: {}", enc.as_str())),
            Err(e) => ToolResult::error(format!("Error writing file: {}", e)),
        }
    }

    fn edit(&self, path: &Path, old_str: &str, new_str: &str, replace_all: bool, encoding: Option<&str>) -> ToolResult {
        match std::fs::read(path) {
            Ok(data) => {
                let (detected_encoding, has_bom) = if let Some(hint) = encoding {
                    (Encoding::from_str(hint).unwrap_or(Encoding::Utf8), false)
                } else {
                    detect_encoding(&data)
                };

                match decode_content(&data, detected_encoding, has_bom) {
                    Ok((content, line_endings)) => {
                        let normalized = content.replace("\r\n", "\n");
                        let new_content = if replace_all {
                            normalized.replace(old_str, new_str)
                        } else {
                            match normalized.find(old_str) {
                                Some(_) => normalized.replacen(old_str, new_str, 1),
                                None => return ToolResult::error(format!("Text '{}' not found in file", old_str)),
                            }
                        };

                        let bytes = encode_content(&new_content, detected_encoding, &line_endings, has_bom);
                        match std::fs::write(path, bytes) {
                            Ok(_) => ToolResult::ok("File edited successfully".to_string()),
                            Err(e) => ToolResult::error(format!("Error writing file: {}", e)),
                        }
                    }
                    Err(e) => ToolResult::error(format!("Error decoding content: {}", e)),
                }
            }
            Err(e) => ToolResult::error(format!("Error reading file: {}", e)),
        }
    }
}

impl Default for FileEncodingTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for FileEncodingTool {
    fn name(&self) -> &str {
        "file_encoding"
    }

    fn description(&self) -> &str {
        "Read, write, or edit files with arbitrary text encodings (GBK, GB18030, Latin-1, Shift-JIS, Big5, EUC-KR, etc.).\n\n\
        IMPORTANT: This is a fallback tool. ALWAYS prefer read_file + edit_file/multi_edit + write_file for coding and file manipulation. \
        Only use file_encoding when those tools fail due to encoding issues (garbled text, non-UTF-8 detection, unsupported charset).\n\n\
        Auto-detects encoding if not specified. Encoding detection uses BOM and heuristic analysis.\n\n\
        Usage:\n\
        - detect: Only detect the encoding, returns encoding name and a preview.\n\
        - For read/edit/multi_edit: auto-detects encoding if not specified.\n\
        - For write: defaults to UTF-8.\n\
        - edit/multi_edit: follows edit_file/multi_edit convention.\n\n\
        Common encoding names: gbk, gb18030, big5, shift_jis, euc_jp, euc_kr, iso-8859-1, windows-1252."
    }

    fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut schema = serde_json::Map::new();
        schema.insert("type".to_string(), serde_json::json!("object"));

        let mut properties = serde_json::Map::new();

        // file_path
        let mut file_path = serde_json::Map::new();
        file_path.insert("type".to_string(), serde_json::json!("string"));
        file_path.insert("description".to_string(), serde_json::json!("Absolute path to the file to operate on."));
        properties.insert("file_path".to_string(), serde_json::json!(file_path));

        // operation
        let mut operation = serde_json::Map::new();
        operation.insert("type".to_string(), serde_json::json!("string"));
        operation.insert("description".to_string(), serde_json::json!("Operation: read, write, edit, multi_edit, detect. Default: read"));
        operation.insert("enum".to_string(), serde_json::json!(["read", "write", "edit", "multi_edit", "detect"]));
        properties.insert("operation".to_string(), serde_json::json!(operation));

        // encoding
        let mut encoding = serde_json::Map::new();
        encoding.insert("type".to_string(), serde_json::json!("string"));
        encoding.insert("description".to_string(), serde_json::json!("Encoding name. Default: utf-8. Examples: gbk, gb18030, big5, shift_jis, euc_jp, euc_kr, iso-8859-1, windows-1252"));
        properties.insert("encoding".to_string(), serde_json::json!(encoding));

        // content
        let mut content = serde_json::Map::new();
        content.insert("type".to_string(), serde_json::json!("string"));
        content.insert("description".to_string(), serde_json::json!("Text content to write. REQUIRED for 'write' operation."));
        properties.insert("content".to_string(), serde_json::json!(content));

        // old_string
        let mut old_string = serde_json::Map::new();
        old_string.insert("type".to_string(), serde_json::json!("string"));
        old_string.insert("description".to_string(), serde_json::json!("Exact text to find and replace. REQUIRED for 'edit' operation."));
        properties.insert("old_string".to_string(), serde_json::json!(old_string));

        // new_string
        let mut new_string = serde_json::Map::new();
        new_string.insert("type".to_string(), serde_json::json!("string"));
        new_string.insert("description".to_string(), serde_json::json!("Replacement text. REQUIRED for 'edit' operation."));
        properties.insert("new_string".to_string(), serde_json::json!(new_string));

        // replace_all
        let mut replace_all = serde_json::Map::new();
        replace_all.insert("type".to_string(), serde_json::json!("boolean"));
        replace_all.insert("description".to_string(), serde_json::json!("Replace all occurrences of old_string (for edit operation, default: false)"));
        properties.insert("replace_all".to_string(), serde_json::json!(replace_all));

        // edits
        let mut edits = serde_json::Map::new();
        edits.insert("type".to_string(), serde_json::json!("array"));
        edits.insert("description".to_string(), serde_json::json!("List of {old_string, new_string} edit operations (for multi_edit)"));
        properties.insert("edits".to_string(), serde_json::json!(edits));

        schema.insert("properties".to_string(), serde_json::json!(properties));
        schema.insert("required".to_string(), serde_json::json!(["file_path"]));

        schema
    }

    fn check_permissions(&self, params: &HashMap<String, serde_json::Value>) -> ToolPermissionResult {
        let path = params.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return ToolPermissionResult::passthrough();
        }
        crate::tools::check_path_safety_for_auto_edit(path)
    }

    fn execute(&self, params: HashMap<String, serde_json::Value>) -> ToolResult {
        let file_path = params.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
        if file_path.is_empty() {
            return ToolResult::error("Error: file_path is required".to_string());
        }

        let path = self.resolve_path(file_path);
        let operation = params.get("operation").and_then(|v| v.as_str()).unwrap_or("read");
        let encoding = params.get("encoding").and_then(|v| v.as_str());

        match operation {
            "detect" => self.detect(&path),
            "read" => self.read(&path, encoding),
            "write" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    return ToolResult::error("Error: content is required for write operation".to_string());
                }
                self.write(&path, content, encoding)
            }
            "edit" => {
                let old_string = params.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                let new_string = params.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                if old_string.is_empty() {
                    return ToolResult::error("Error: old_string is required for edit operation".to_string());
                }
                let replace_all = params.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
                self.edit(&path, old_string, new_string, replace_all, encoding)
            }
            "multi_edit" => {
                // For multi_edit, treat as multiple sequential edits
                let edits = params.get("edits").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let mut current_content = match std::fs::read(&path) {
                    Ok(data) => {
                        let (detected_encoding, has_bom) = detect_encoding(&data);
                        match decode_content(&data, detected_encoding, has_bom) {
                            Ok((content, _)) => content.replace("\r\n", "\n"),
                            Err(e) => return ToolResult::error(format!("Error decoding content: {}", e)),
                        }
                    }
                    Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
                };

                for edit in edits {
                    let old_string = edit.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new_string = edit.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    let replace_all = edit.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);

                    if old_string.is_empty() {
                        continue;
                    }

                    if replace_all {
                        current_content = current_content.replace(old_string, new_string);
                    } else if current_content.contains(old_string) {
                        current_content = current_content.replacen(old_string, new_string, 1);
                    }
                }

                self.write(&path, &current_content, encoding)
            }
            _ => ToolResult::error(format!(
                "Error: unknown operation '{}'. Supported: read, write, edit, multi_edit, detect",
                operation
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn test_tool_name() {
        let tool = FileEncodingTool::new();
        assert_eq!(tool.name(), "file_encoding");
    }

    #[test]
    fn test_detect_encoding_utf8() {
        let data = b"Hello, world!".to_vec();
        let (encoding, has_bom) = detect_encoding(&data);
        assert_eq!(encoding, Encoding::Utf8);
        assert!(!has_bom);
    }

    #[test]
    fn test_detect_encoding_utf8_bom() {
        let data: Vec<u8> = vec![0xEF, 0xBB, 0xBF, b'H', b'i'];
        let (encoding, has_bom) = detect_encoding(&data);
        assert_eq!(encoding, Encoding::Utf8);
        assert!(has_bom);
    }

    #[test]
    fn test_detect_encoding_utf16le_bom() {
        let data: Vec<u8> = vec![0xFF, 0xFE, b'H', 0x00, b'i', 0x00];
        let (encoding, has_bom) = detect_encoding(&data);
        assert_eq!(encoding, Encoding::Utf16Le);
        assert!(has_bom);
    }

    #[test]
    fn test_detect_encoding_utf16be_bom() {
        let data: Vec<u8> = vec![0xFE, 0xFF, 0x00, b'H', 0x00, b'i'];
        let (encoding, has_bom) = detect_encoding(&data);
        assert_eq!(encoding, Encoding::Utf16Be);
        assert!(has_bom);
    }

    #[test]
    fn test_decode_utf8() {
        let data = b"Hello, world!".to_vec();
        let (content, le) = decode_content(&data, Encoding::Utf8, false).unwrap();
        assert_eq!(content, "Hello, world!");
        assert_eq!(le, "LF");
    }

    #[test]
    fn test_decode_utf16le() {
        let data: Vec<u8> = vec![b'H', 0x00, b'i', 0x00];
        let (content, _) = decode_content(&data, Encoding::Utf16Le, false).unwrap();
        assert_eq!(content, "Hi");
    }

    #[test]
    fn test_decode_utf16be() {
        let data: Vec<u8> = vec![0x00, b'H', 0x00, b'i'];
        let (content, _) = decode_content(&data, Encoding::Utf16Be, false).unwrap();
        assert_eq!(content, "Hi");
    }

    #[test]
    fn test_encode_utf16le() {
        let bytes = encode_content("Hi", Encoding::Utf16Le, "LF", false);
        assert_eq!(bytes, vec![b'H', 0x00, b'i', 0x00]);
    }

    #[test]
    fn test_encode_utf16be() {
        let bytes = encode_content("Hi", Encoding::Utf16Be, "LF", false);
        assert_eq!(bytes, vec![0x00, b'H', 0x00, b'i']);
    }

    #[test]
    fn test_encode_with_bom() {
        let bytes = encode_content("Hi", Encoding::Utf16Le, "LF", true);
        assert!(bytes.starts_with(&[0xFF, 0xFE]));
    }

    #[test]
    fn test_detect_operation() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(b"Hello encoding test").unwrap();

        let tool = FileEncodingTool::new();
        let result = tool.execute(json!({
            "file_path": file_path.to_str().unwrap(),
            "operation": "detect"
        }).as_object().unwrap().clone());
        assert!(!result.is_error, "detect failed: {}", result.output);
        assert!(result.output.contains("UTF-8"));
    }

    #[test]
    fn test_read_operation() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "Hello encoding test").unwrap();

        let tool = FileEncodingTool::new();
        let result = tool.execute(json!({
            "file_path": file_path.to_str().unwrap(),
            "operation": "read"
        }).as_object().unwrap().clone());
        assert!(!result.is_error, "read failed: {}", result.output);
        assert!(result.output.contains("Hello encoding test"));
    }

    #[test]
    fn test_write_operation() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        let tool = FileEncodingTool::new();
        let result = tool.execute(json!({
            "file_path": file_path.to_str().unwrap(),
            "operation": "write",
            "content": "Test content"
        }).as_object().unwrap().clone());
        assert!(!result.is_error, "write failed: {}", result.output);

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Test content");
    }

    #[test]
    fn test_edit_operation() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "Hello world").unwrap();

        let tool = FileEncodingTool::new();
        let result = tool.execute(json!({
            "file_path": file_path.to_str().unwrap(),
            "operation": "edit",
            "old_string": "world",
            "new_string": "Rust"
        }).as_object().unwrap().clone());
        assert!(!result.is_error, "edit failed: {}", result.output);

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello Rust");
    }
}
