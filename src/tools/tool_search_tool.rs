//! ToolSearchTool - Search and discover available tools
//!
//! Enables deferred tool loading: instead of putting ALL tools in the system prompt,
//! only core tools are shown. The agent can discover additional tools by searching.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use serde_json::Value;
use super::{Tool, ToolResult, Registry};

pub struct ToolSearchTool {
    /// Reference to the tool registry for searching (set via set_registry after registration)
    registry: Arc<RwLock<Option<Arc<Registry>>>>,
}

impl ToolSearchTool {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(None)),
        }
    }

    /// Set the registry reference after the full registry is populated.
    /// Call this after all other tools have been registered.
    pub fn set_registry(&self, registry: Arc<Registry>) {
        let mut guard = self.registry.write().unwrap();
        *guard = Some(registry);
    }

    fn get_registry(&self) -> Option<Arc<Registry>> {
        let guard = self.registry.read().unwrap();
        guard.clone()
    }
}

impl Default for ToolSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ToolSearchTool {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
        }
    }
}

impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn description(&self) -> &str {
        "Search and discover available tools. Use this to find tools when you're unsure which tool to use. Supports three query forms: select:name1,name2 to get specific tool definitions, keyword search for relevant tools, +prefix keyword for prefix-matched tools."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query. Three forms supported:\n1. select:name1,name2 - Get specific tool definitions by name\n2. +prefix keyword - Prefix-matched search (tools starting with keyword)\n3. keyword1 keyword2 - Scored relevance search (name=3pts, desc=1pt each)"
                }
            },
            "required": ["query"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let query = match params.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.trim(),
            None => {
                return ToolResult::error("Error: missing required parameter 'query'".to_string());
            }
        };

        let registry = match self.get_registry() {
            Some(r) => r,
            None => {
                return ToolResult::error("Error: tool registry not available".to_string());
            }
        };

        let output = if query.starts_with("select:") {
            self.handle_select(query, &registry)
        } else if query.starts_with("+") {
            self.handle_prefix(query, &registry)
        } else {
            self.handle_keyword(query, &registry)
        };

        ToolResult::ok(output)
    }
}

impl ToolSearchTool {
    /// Handle select:query - return full definitions for named tools
    fn handle_select(&self, query: &str, registry: &Registry) -> String {
        let names = query.strip_prefix("select:").unwrap_or("");
        let mut output = String::new();

        for name in names.split(',') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }

            if let Some(tool) = registry.get(name) {
                output.push_str(&format_tool_definition(&*tool));
            } else {
                output.push_str(&format!("### {}\nTool not found.\n\n", name));
            }
        }

        if output.is_empty() {
            output = "No tools found for the given names.".to_string();
        }

        output
    }

    /// Handle +prefix query - prefix-matched search
    fn handle_prefix(&self, query: &str, registry: &Registry) -> String {
        let prefix = query.strip_prefix('+').unwrap_or("").trim();

        if prefix.is_empty() {
            return "Error: prefix keyword is empty".to_string();
        }

        let mut output = String::new();
        let mut found = false;

        for tool in registry.all_tools() {
            if tool.name().to_lowercase().starts_with(&prefix.to_lowercase()) {
                output.push_str(&format_tool_definition(&*tool));
                found = true;
            }
        }

        if !found {
            output = format!("No tools found with prefix '{}'.", prefix);
        }

        output
    }

    /// Handle keyword query - scored relevance search
    fn handle_keyword(&self, query: &str, registry: &Registry) -> String {
        let keywords: Vec<&str> = query.split_whitespace().collect();

        if keywords.is_empty() {
            return "Error: no search keywords provided".to_string();
        }

        let mut scored: Vec<(i32, Arc<dyn Tool>)> = Vec::new();

        for tool in registry.all_tools() {
            let mut score = 0i32;

            for keyword in &keywords {
                let keyword_lower = keyword.to_lowercase();

                // Name match = 3 points
                if tool.name().to_lowercase().contains(&keyword_lower) {
                    score += 3;
                }

                // Description match = 1 point per keyword
                if tool.description().to_lowercase().contains(&keyword_lower) {
                    score += 1;
                }
            }

            if score > 0 {
                scored.push((score, tool));
            }
        }

        // Sort by score descending
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        // Limit to top 10
        let top_results: Vec<_> = scored.into_iter().take(10).collect();

        if top_results.is_empty() {
            return format!("No tools found matching: {}", query);
        }

        let mut output = String::new();
        for (_, tool) in top_results {
            output.push_str(&format_tool_definition(&*tool));
        }

        output
    }
}

/// Format a tool's definition for output
fn format_tool_definition(tool: &dyn Tool) -> String {
    let mut s = String::new();
    s.push_str(&format!("### {}\n", tool.name()));
    s.push_str(&format!("Description: {}\n", tool.description()));

    let schema = tool.input_schema();
    let params = extract_parameters(&schema);
    s.push_str(&format!("Parameters: {}\n\n", params));

    s
}

/// Extract parameter names from an input schema
fn extract_parameters(schema: &serde_json::Map<String, Value>) -> String {
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        let names: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
        if names.is_empty() {
            return "none".to_string();
        }
        names.join(", ")
    } else {
        "none".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_registry() -> Registry {
        let registry = Registry::new();
        registry.register(RuntimeInfoTool);
        registry.register(TestFileReadTool);
        registry.register(TestFileWriteTool);
        registry.register(WebSearchTool);
        registry
    }

    // Minimal test tools
    struct RuntimeInfoTool;
    struct TestFileReadTool;
    struct TestFileWriteTool;
    struct WebSearchTool;

    impl Tool for RuntimeInfoTool {
        fn name(&self) -> &str { "runtime_info" }
        fn description(&self) -> &str { "Show runtime and system information" }
        fn input_schema(&self) -> serde_json::Map<String, Value> {
            serde_json::json!({}).as_object().unwrap().clone()
        }
        fn check_permissions(&self, _: &HashMap<String, Value>) -> Option<ToolResult> { None }
        fn execute(&self, _: HashMap<String, Value>) -> ToolResult { ToolResult::ok("") }
    }

    impl Tool for TestFileReadTool {
        fn name(&self) -> &str { "test_file_read" }
        fn description(&self) -> &str { "Read files from the filesystem" }
        fn input_schema(&self) -> serde_json::Map<String, Value> {
            serde_json::json!({
                "properties": {
                    "path": { "type": "string" }
                }
            }).as_object().unwrap().clone()
        }
        fn check_permissions(&self, _: &HashMap<String, Value>) -> Option<ToolResult> { None }
        fn execute(&self, _: HashMap<String, Value>) -> ToolResult { ToolResult::ok("") }
    }

    impl Tool for TestFileWriteTool {
        fn name(&self) -> &str { "test_file_write" }
        fn description(&self) -> &str { "Write content to files" }
        fn input_schema(&self) -> serde_json::Map<String, Value> {
            serde_json::json!({
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                }
            }).as_object().unwrap().clone()
        }
        fn check_permissions(&self, _: &HashMap<String, Value>) -> Option<ToolResult> { None }
        fn execute(&self, _: HashMap<String, Value>) -> ToolResult { ToolResult::ok("") }
    }

    impl Tool for WebSearchTool {
        fn name(&self) -> &str { "web_search" }
        fn description(&self) -> &str { "Search the web for information" }
        fn input_schema(&self) -> serde_json::Map<String, Value> {
            serde_json::json!({
                "properties": {
                    "query": { "type": "string" }
                }
            }).as_object().unwrap().clone()
        }
        fn check_permissions(&self, _: &HashMap<String, Value>) -> Option<ToolResult> { None }
        fn execute(&self, _: HashMap<String, Value>) -> ToolResult { ToolResult::ok("") }
    }

    #[test]
    fn test_select_specific_tool() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "select:runtime_info"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(!result.output.contains("Tool not found"));
        assert!(result.output.contains("### runtime_info"));
        assert!(result.output.contains("Description: Show runtime"));
    }

    #[test]
    fn test_select_multiple_tools() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "select:runtime_info,web_search"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("### runtime_info"));
        assert!(result.output.contains("### web_search"));
    }

    #[test]
    fn test_select_not_found() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "select:nonexistent_tool"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("Tool not found"));
    }

    #[test]
    fn test_prefix_search() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "+test"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("test_file_read"));
        assert!(result.output.contains("test_file_write"));
        assert!(!result.output.contains("runtime_info"));
    }

    #[test]
    fn test_prefix_search_case_insensitive() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "+WEB"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("web_search"));
    }

    #[test]
    fn test_prefix_not_found() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "+xyz123"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("No tools found"));
    }

    #[test]
    fn test_keyword_search_by_name() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "runtime"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("runtime_info"));
    }

    #[test]
    fn test_keyword_search_by_description() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "filesystem"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("test_file_read") || result.output.contains("test_file_write"));
    }

    #[test]
    fn test_keyword_search_multiple_terms() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "file read"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        // test_file_read should score higher (name matches both "file" and "read")
        let lines: Vec<&str> = result.output.lines().collect();
        let first_tool_idx = lines.iter().position(|l| l.starts_with("### "));
        assert!(first_tool_idx.is_some());
        let first_tool = lines[first_tool_idx.unwrap()].trim_start_matches("### ");
        assert!(first_tool.contains("test_file_read"));
    }

    #[test]
    fn test_keyword_search_limit_10() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params = serde_json::json!({
            "query": "test"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        let tool_count = result.output.matches("### ").count();
        assert!(tool_count <= 10);
    }

    #[test]
    fn test_extract_parameters() {
        let schema = serde_json::json!({
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
                "offset": { "type": "number" }
            }
        }).as_object().unwrap().clone();

        let params = extract_parameters(&schema);
        assert!(params.contains("path"));
        assert!(params.contains("content"));
        assert!(params.contains("offset"));
    }

    #[test]
    fn test_extract_parameters_empty() {
        let schema = serde_json::json!({}).as_object().unwrap().clone();
        let params = extract_parameters(&schema);
        assert_eq!(params, "none");
    }

    #[test]
    fn test_missing_query_param() {
        let registry = create_test_registry();
        let tool = ToolSearchTool::new();
        tool.set_registry(Arc::new(registry));

        let params: HashMap<String, Value> = HashMap::new();
        let result = tool.execute(params);
        assert!(result.output.contains("missing required parameter"));
    }

    #[test]
    fn test_registry_not_available() {
        let tool = ToolSearchTool::new();
        // Don't set registry

        let params = serde_json::json!({
            "query": "runtime"
        }).as_object().unwrap().iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let result = tool.execute(params);
        assert!(result.output.contains("registry not available"));
    }
}
