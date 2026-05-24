//! Memory tools — memory_add and memory_search for session memory.

use crate::session_memory::SessionMemory;
use crate::tools::{Tool, ToolResult, ToolPermissionResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Valid categories for memory notes.
const VALID_CATEGORIES: &[&str] = &["preference", "decision", "state", "reference"];

/// MemoryAddTool — saves a note to session memory.
pub struct MemoryAddTool {
    memory: Arc<SessionMemory>,
}

impl MemoryAddTool {
    pub fn new(memory: Arc<SessionMemory>) -> Self {
        Self { memory }
    }
}

impl Clone for MemoryAddTool {
    fn clone(&self) -> Self {
        Self {
            memory: Arc::clone(&self.memory),
        }
    }
}

impl Tool for MemoryAddTool {
    fn name(&self) -> &str {
        "memory_add"
    }

    fn description(&self) -> &str {
        "Save a note to session memory for later reference. Use categories: 'preference' (user preferences), 'decision' (key decisions), 'state' (project state), 'reference' (useful references)."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "enum": VALID_CATEGORIES,
                    "description": "Category of the memory note"
                },
                "content": {
                    "type": "string",
                    "description": "The note content to remember"
                },
            },
            "required": ["category", "content"]
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> ToolPermissionResult {
        ToolPermissionResult::passthrough()
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let category = match params.get("category").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Error: category is required"),
        };
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Error: content is required"),
        };

        self.memory.add_note(category, content, "assistant");
        ToolResult::ok(format!("Saved to memory [{}]: {}", category, content))
    }


}

/// MemorySearchTool — searches session memory for relevant notes.
pub struct MemorySearchTool {
    memory: Arc<SessionMemory>,
}

impl MemorySearchTool {
    pub fn new(memory: Arc<SessionMemory>) -> Self {
        Self { memory }
    }
}

impl Clone for MemorySearchTool {
    fn clone(&self) -> Self {
        Self {
            memory: Arc::clone(&self.memory),
        }
    }
}

impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search session memory for notes matching a query. Returns relevant memory entries."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query to find relevant memory notes"
                },
            },
            "required": ["query"]
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> ToolPermissionResult {
        ToolPermissionResult::passthrough()
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let query = match params.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("Error: query is required"),
        };

        let results = self.memory.search_notes(query);
        if results.is_empty() {
            return ToolResult::ok("No matching memory notes found.");
        }

        let mut output = String::new();
        for entry in results {
            output.push_str(&format!("[{}] {}\n", entry.category, entry.content));
        }
        ToolResult::ok(output.trim_end().to_string())
    }


}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use crate::context::SessionMemory;
    use std::sync::Arc;

    fn make_memory() -> Arc<SessionMemory> {
        Arc::new(SessionMemory::new())
    }

    // ─── MemoryAddTool ────────────────────────────────────────────────────────

    #[test]
    fn test_memory_add_tool_name() {
        let mem = make_memory();
        let tool = MemoryAddTool::new(Arc::clone(&mem));
        assert_eq!(tool.name(), "memory_add");
    }

    #[test]
    fn test_memory_add_tool_schema() {
        let mem = make_memory();
        let tool = MemoryAddTool::new(Arc::clone(&mem));
        let schema = tool.input_schema();
        let props = schema.get("properties").unwrap().as_object().unwrap();
        assert!(props.contains_key("category"), "expected 'category' in schema");
        assert!(props.contains_key("content"), "expected 'content' in schema");
    }

    #[test]
    fn test_memory_add_execute_valid() {
        let mem = make_memory();
        let tool = MemoryAddTool::new(Arc::clone(&mem));
        let result = tool.execute(&serde_json::json!({
            "category": "preference",
            "content": "user likes dark mode"
        }));
        assert!(!result.is_error, "unexpected error: {}", result.output);
    }

    #[test]
    fn test_memory_add_missing_category() {
        let mem = make_memory();
        let tool = MemoryAddTool::new(mem);
        let result = tool.execute(&serde_json::json!({"content": "test"}));
        assert!(result.is_error, "missing category should return error");
    }

    #[test]
    fn test_memory_add_missing_content() {
        let mem = make_memory();
        let tool = MemoryAddTool::new(mem);
        let result = tool.execute(&serde_json::json!({"category": "state"}));
        assert!(result.is_error, "missing content should return error");
    }

    #[test]
    fn test_memory_add_invalid_category() {
        let mem = make_memory();
        let tool = MemoryAddTool::new(mem);
        let result = tool.execute(&serde_json::json!({
            "category": "bug",
            "content": "test"
        }));
        assert!(result.is_error, "invalid category should return error");
    }

    // ─── MemorySearchTool ────────────────────────────────────────────────────

    #[test]
    fn test_memory_search_tool_name() {
        let mem = make_memory();
        let tool = MemorySearchTool::new(Arc::clone(&mem));
        assert_eq!(tool.name(), "memory_search");
    }

    #[test]
    fn test_memory_search_tool_schema() {
        let mem = make_memory();
        let tool = MemorySearchTool::new(Arc::clone(&mem));
        let schema = tool.input_schema();
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|r| r.as_str() == Some("query")), "expected 'query' in required");
    }

    #[test]
    fn test_memory_search_no_results() {
        let mem = make_memory();
        let tool = MemorySearchTool::new(mem);
        let result = tool.execute(&serde_json::json!({"query": "nothing"}));
        assert!(!result.is_error, "unexpected error: {}", result.output);
    }

    #[test]
    fn test_memory_search_missing_query() {
        let mem = make_memory();
        let tool = MemorySearchTool::new(mem);
        let result = tool.execute(&serde_json::json!({}));
        assert!(result.is_error, "missing query should return error");
    }

    #[test]
    fn test_memory_search_with_results() {
        let mem = make_memory();
        // First add something
        let add_tool = MemoryAddTool::new(Arc::clone(&mem));
        add_tool.execute(&serde_json::json!({
            "category": "state",
            "content": "working on feature"
        }));

        let search_tool = MemorySearchTool::new(Arc::clone(&mem));
        let result = search_tool.execute(&serde_json::json!({"query": "feature"}));
        assert!(!result.is_error, "unexpected error: {}", result.output);
    }
}
