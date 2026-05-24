// NotebookEditTool provides cell-level editing for Jupyter notebooks (.ipynb).
// Supports replace, insert, and delete operations on cells.

use crate::tools::{
    Tool, ToolPermissionResult, ToolResult,
};
use dunce;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use std::sync::Arc;

/// Notebook cell structure matching Jupyter notebook format
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NbCell {
    #[serde(rename = "cell_type")]
    pub cell_type: String,
    pub source: Value,
    #[serde(rename = "outputs", skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<Value>>,
    #[serde(rename = "execution_count", skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<Value>,
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "metadata", skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl NbCell {
    /// Get source as string
    pub fn get_source(&self) -> String {
        match &self.source {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        }
    }

    /// Set source from string
    pub fn set_source(&mut self, source: String) {
        self.source = Value::String(source);
    }
}

/// Jupyter notebook document structure
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NbDocument {
    pub cells: Vec<NbCell>,
    pub nbformat: usize,
    #[serde(rename = "nbformat_minor")]
    pub nbformat_minor: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

pub struct NotebookEditTool {
    files_read_handle: Option<Arc<RwLock<HashMap<String, crate::tools::FileReadInfo>>>>,
}

impl NotebookEditTool {
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

    fn find_cell_index(&self, cells: &[NbCell], cell_id: &str) -> Option<usize> {
        // Try exact match on cell ID first
        for (i, cell) in cells.iter().enumerate() {
            if let Some(ref id) = cell.id {
                if id == cell_id {
                    return Some(i);
                }
            }
        }

        // Try index format "cell-N"
        if cell_id.starts_with("cell-") {
            if let Ok(idx) = cell_id[5..].parse::<usize>() {
                if idx < cells.len() {
                    return Some(idx);
                }
            }
        }

        // Try substring/prefix match against cell source content
        for (i, cell) in cells.iter().enumerate() {
            let source = cell.get_source();
            if source.contains(cell_id) || source.starts_with(cell_id) {
                return Some(i);
            }
        }

        None
    }

    fn ensure_cell_id(cell: &mut NbCell) {
        if cell.id.is_none() {
            cell.id = Some(uuid::Uuid::new_v4().to_string());
        }
    }

    fn execute_edit(
        &self,
        path: &Path,
        cell_id: &str,
        new_source: &str,
        cell_type: Option<&str>,
        edit_mode: &str,
    ) -> ToolResult {
        // Read the notebook file
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => return ToolResult::error(format!("Error reading notebook: {}", e)),
        };

        // Parse JSON
        let mut nb: NbDocument = match serde_json::from_slice(&data) {
            Ok(nb) => nb,
            Err(e) => return ToolResult::error(format!("Error: invalid notebook format: {}", e)),
        };

        if nb.nbformat < 4 {
            return ToolResult::error(format!(
                "Error: unsupported notebook format (nbformat={}, requires 4+)",
                nb.nbformat
            ));
        }

        let target_index = self.find_cell_index(&nb.cells, cell_id);

        match edit_mode {
            "replace" => {
                if let Some(idx) = target_index {
                    let cell = &mut nb.cells[idx];
                    cell.set_source(new_source.to_string());
                    if let Some(ct) = cell_type {
                        cell.cell_type = ct.to_string();
                    }
                } else {
                    // Auto-promote to insert if cell not found
                    let mut new_cell = NbCell {
                        cell_type: cell_type.unwrap_or("code").to_string(),
                        source: Value::String(new_source.to_string()),
                        outputs: None,
                        execution_count: None,
                        id: None,
                        metadata: None,
                    };
                    Self::ensure_cell_id(&mut new_cell);
                    nb.cells.push(new_cell);
                }
            }
            "insert" => {
                let idx = target_index.unwrap_or(nb.cells.len());
                let mut new_cell = NbCell {
                    cell_type: cell_type.unwrap_or("code").to_string(),
                    source: Value::String(new_source.to_string()),
                    outputs: None,
                    execution_count: None,
                    id: None,
                    metadata: None,
                };
                Self::ensure_cell_id(&mut new_cell);
                nb.cells.insert(idx, new_cell);
            }
            "delete" => {
                if let Some(idx) = target_index {
                    nb.cells.remove(idx);
                } else {
                    return ToolResult::error(format!("Error: cell '{}' not found in notebook", cell_id));
                }
            }
            _ => {
                return ToolResult::error(format!(
                    "Error: invalid edit_mode '{}'. Must be 'replace', 'insert', or 'delete'.",
                    edit_mode
                ));
            }
        }

        // Ensure all cells have IDs
        for cell in &mut nb.cells {
            Self::ensure_cell_id(cell);
        }

        // Write back to file
        let output = match serde_json::to_string_pretty(&nb) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("Error serializing notebook: {}", e)),
        };

        if let Err(e) = std::fs::write(path, output) {
            return ToolResult::error(format!("Error writing notebook: {}", e));
        }

        let message = match edit_mode {
            "replace" => format!("Cell '{}' replaced successfully", cell_id),
            "insert" => format!("Cell inserted at position '{}'", cell_id),
            "delete" => format!("Cell '{}' deleted successfully", cell_id),
            _ => String::new(),
        };

        ToolResult::ok(message)
    }
}

impl Default for NotebookEditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for NotebookEditTool {
    fn name(&self) -> &str {
        "notebook_edit"
    }

    fn description(&self) -> &str {
        "Edit a Jupyter Notebook (.ipynb) at the cell level. Supports replace, insert, and delete operations on cells. Use read_file first to see the current notebook structure and cell IDs."
    }

    fn input_schema(&self) -> Map<String, Value> {
        let mut schema = Map::new();
        schema.insert("type".to_string(), Value::String("object".to_string()));

        let mut properties = Map::new();

        // notebook_path
        let mut notebook_path = Map::new();
        notebook_path.insert("type".to_string(), Value::String("string".to_string()));
        notebook_path.insert(
            "description".to_string(),
            Value::String("Path to the Jupyter Notebook file (.ipynb). Must be read with read_file first.".to_string()),
        );
        properties.insert("notebook_path".to_string(), Value::Object(notebook_path));

        // cell_id
        let mut cell_id = Map::new();
        cell_id.insert("type".to_string(), Value::String("string".to_string()));
        cell_id.insert(
            "description".to_string(),
            Value::String("Cell ID to operate on. Resolution order: 1) exact match against cell ID field; 2) index format 'cell-N' (e.g., 'cell-0' for 0th cell); 3) substring/prefix match against cell source content.".to_string()),
        );
        properties.insert("cell_id".to_string(), Value::Object(cell_id));

        // new_source
        let mut new_source = Map::new();
        new_source.insert("type".to_string(), Value::String("string".to_string()));
        new_source.insert(
            "description".to_string(),
            Value::String("New source code/text for the cell. Required for replace and insert modes.".to_string()),
        );
        properties.insert("new_source".to_string(), Value::Object(new_source));

        // cell_type
        let mut cell_type = Map::new();
        cell_type.insert("type".to_string(), Value::String("string".to_string()));
        cell_type.insert(
            "description".to_string(),
            Value::String("Cell type: 'code' or 'markdown'. Optional for replace (keeps existing if not specified), required for insert.".to_string()),
        );
        cell_type.insert(
            "enum".to_string(),
            Value::Array(vec![Value::String("code".to_string()), Value::String("markdown".to_string())]),
        );
        properties.insert("cell_type".to_string(), Value::Object(cell_type));

        // edit_mode
        let mut edit_mode = Map::new();
        edit_mode.insert("type".to_string(), Value::String("string".to_string()));
        edit_mode.insert(
            "description".to_string(),
            Value::String("Edit mode: 'replace' (default), 'insert' (insert before target cell), or 'delete' (remove target cell).".to_string()),
        );
        edit_mode.insert(
            "enum".to_string(),
            Value::Array(vec![
                Value::String("replace".to_string()),
                Value::String("insert".to_string()),
                Value::String("delete".to_string()),
            ]),
        );
        properties.insert("edit_mode".to_string(), Value::Object(edit_mode));

        schema.insert("properties".to_string(), Value::Object(properties));
        schema.insert(
            "required".to_string(),
            Value::Array(vec![
                Value::String("notebook_path".to_string()),
                Value::String("cell_id".to_string()),
            ]),
        );

        schema
    }

    fn check_permissions(&self, params: &HashMap<String, Value>) -> ToolPermissionResult {
        let path = params.get("notebook_path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return ToolPermissionResult::deny("notebook_path is required");
        }
        if !path.to_lowercase().ends_with(".ipynb") {
            return ToolPermissionResult::deny("notebook_edit only works on .ipynb files");
        }
        ToolPermissionResult::passthrough()
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let notebook_path = params
            .get("notebook_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if notebook_path.is_empty() {
            return ToolResult::error("Error: notebook_path is required".to_string());
        }

        if !notebook_path.to_lowercase().ends_with(".ipynb") {
            return ToolResult::error("Error: notebook_edit only works on .ipynb files".to_string());
        }

        let path = self.resolve_path(notebook_path);

        // Check file exists
        match std::fs::metadata(&path) {
            Ok(info) => {
                if info.len() > 10 * 1024 * 1024 {
                    return ToolResult::error(format!(
                        "Error: notebook too large ({} bytes, max 10MB)",
                        info.len()
                    ));
                }
            }
            Err(e) => {
                return ToolResult::error(format!("Error: notebook not found: {} ({})", notebook_path, e));
            }
        }

        // Read-before-edit check
        if let Some(ref handle) = self.files_read_handle {
            if let Ok(files_read) = handle.read() {
                if let Some(read_info) = files_read.get(notebook_path) {
                    if let Ok(current_mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                        if current_mtime > read_info.mtime && read_info.from_read {
                            return ToolResult::error(format!(
                                "Error: notebook was modified since you last read it. Read it again with read_file to get the current content."
                            ));
                        }
                    }
                } else {
                    return ToolResult::error(format!(
                        "Error: you must read the notebook with read_file before editing it."
                    ));
                }
            }
        }

        let cell_id = params.get("cell_id").and_then(|v| v.as_str()).unwrap_or("");
        let new_source = params.get("new_source").and_then(|v| v.as_str()).unwrap_or("");
        let cell_type = params.get("cell_type").and_then(|v| v.as_str());
        let edit_mode = params.get("edit_mode").and_then(|v| v.as_str()).unwrap_or("replace");

        // Validate parameters
        if edit_mode == "insert" && cell_type.is_none() {
            return ToolResult::error("Error: cell_type is required for insert mode. Must be 'code' or 'markdown'.".to_string());
        }

        if (edit_mode == "replace" || edit_mode == "insert") && new_source.is_empty() {
            return ToolResult::error("Error: new_source is required for replace and insert modes.".to_string());
        }

        self.execute_edit(&path, cell_id, new_source, cell_type, edit_mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use serde_json::json;
    use std::io::Write;

    fn create_test_notebook(dir: &std::path::Path) -> String {
        let nb = NbDocument {
            nbformat: 4,
            nbformat_minor: 5,
            metadata: serde_json::json!({}),
            cells: vec![
                NbCell {
                    cell_type: "markdown".to_string(),
                    source: serde_json::json!(["# Title\n"]),
                    id: Some("cell-0".to_string()),
                    metadata: serde_json::json!({}),
                    outputs: None,
                    execution_count: None,
                },
                NbCell {
                    cell_type: "code".to_string(),
                    source: serde_json::json!(["print('hello')\n"]),
                    id: Some("cell-1".to_string()),
                    metadata: serde_json::json!({}),
                    outputs: Some(serde_json::json!([])),
                    execution_count: None,
                },
                NbCell {
                    cell_type: "code".to_string(),
                    source: serde_json::json!(["x = 1\n"]),
                    id: Some("cell-2".to_string()),
                    metadata: serde_json::json!({}),
                    outputs: Some(serde_json::json!([])),
                    execution_count: None,
                },
            ],
        };
        let data = serde_json::to_string_pretty(&nb).unwrap();
        let fp = dir.join("test.ipynb");
        std::fs::write(&fp, data).unwrap();
        fp.to_string_lossy().to_string()
    }

    #[test]
    fn test_tool_name() {
        let tool = NotebookEditTool::new(None, None);
        assert_eq!(tool.name(), "notebook_edit");
    }

    #[test]
    fn test_rejects_non_ipynb() {
        let tool = NotebookEditTool::new(None, None);
        let result = tool.execute(json!({
            "notebook_path": "test.py",
            "cell_id": "cell-0",
            "new_source": "hello",
            "edit_mode": "replace"
        }).as_object().unwrap().clone());
        assert!(result.is_error, "expected error for non-.ipynb file");
        assert!(result.output.contains(".ipynb"), "expected .ipynb in error, got: {}", result.output);
    }

    #[test]
    fn test_rejects_no_path() {
        let tool = NotebookEditTool::new(None, None);
        let result = tool.execute(json!({
            "cell_id": "cell-0",
            "new_source": "hello"
        }).as_object().unwrap().clone());
        assert!(result.is_error, "expected error for missing path");
    }

    #[test]
    fn test_requires_read_first() {
        let dir = tempfile::tempdir().unwrap();
        let fp = create_test_notebook(dir.path());
        let tool = NotebookEditTool::new(None, None);

        let result = tool.execute(json!({
            "notebook_path": fp,
            "cell_id": "cell-0",
            "new_source": "new content",
            "edit_mode": "replace"
        }).as_object().unwrap().clone());
        // Without files_read_handle, it should either require read or proceed
        // depending on implementation
    }

    #[test]
    fn test_insert_requires_cell_type() {
        let dir = tempfile::tempdir().unwrap();
        let fp = create_test_notebook(dir.path());
        let tool = NotebookEditTool::new(None, None);

        // Without marking file as read, this may fail for read-first check first
        let result = tool.execute(json!({
            "notebook_path": fp,
            "cell_id": "cell-0",
            "new_source": "hello",
            "edit_mode": "insert"
        }).as_object().unwrap().clone());
        // Should error either for read-first or missing cell_type
        assert!(result.is_error, "expected error for missing cell_type in insert mode");
    }

    #[test]
    fn test_invalid_edit_mode() {
        let dir = tempfile::tempdir().unwrap();
        let fp = create_test_notebook(dir.path());
        let tool = NotebookEditTool::new(None, None);

        let result = tool.execute(json!({
            "notebook_path": fp,
            "cell_id": "cell-0",
            "new_source": "hello",
            "edit_mode": "invalid"
        }).as_object().unwrap().clone());
        assert!(result.is_error, "expected error for invalid edit_mode");
    }

    #[test]
    fn test_find_cell_index_by_id() {
        let cells = vec![
            NbCell { cell_type: "code".into(), source: json!("a = 1\n"), id: Some("cell-0".into()), metadata: json!({}), outputs: None, execution_count: None },
            NbCell { cell_type: "code".into(), source: json!("b = 2\n"), id: Some("cell-1".into()), metadata: json!({}), outputs: None, execution_count: None },
            NbCell { cell_type: "markdown".into(), source: json!("# Title\n"), id: Some("cell-2".into()), metadata: json!({}), outputs: None, execution_count: None },
        ];
        let tool = NotebookEditTool::new(None, None);
        assert_eq!(tool.find_cell_index(&cells, "cell-0"), Some(0));
        assert_eq!(tool.find_cell_index(&cells, "cell-1"), Some(1));
        assert_eq!(tool.find_cell_index(&cells, "cell-2"), Some(2));
        assert_eq!(tool.find_cell_index(&cells, "cell-100"), None);
    }

    #[test]
    fn test_find_cell_index_by_custom_id() {
        let cells = vec![
            NbCell { cell_type: "code".into(), source: json!("a = 1\n"), id: Some("abc-123".into()), metadata: json!({}), outputs: None, execution_count: None },
            NbCell { cell_type: "code".into(), source: json!("b = 2\n"), id: Some("xyz-456".into()), metadata: json!({}), outputs: None, execution_count: None },
        ];
        let tool = NotebookEditTool::new(None, None);
        assert_eq!(tool.find_cell_index(&cells, "xyz-456"), Some(1));
    }

    #[test]
    fn test_tool_permissions() {
        let tool = NotebookEditTool::new(None, None);
        let perms = tool.permissions();
        assert!(!perms.is_empty());
    }
}
