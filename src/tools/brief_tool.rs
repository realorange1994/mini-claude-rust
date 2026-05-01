//! BriefTool - Provides communication principles to the agent

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;

pub struct BriefTool;

impl BriefTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BriefTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for BriefTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for BriefTool {
    fn name(&self) -> &str {
        "brief"
    }

    fn description(&self) -> &str {
        "Provides communication guidance. Call this tool when you need to review best practices for clear, concise communication with the user."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task or context for which communication guidance is needed"
                }
            },
            "required": ["task"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        // Validate task parameter exists and is non-empty
        let task = match params.get("task") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
            Some(Value::String(_)) => {
                return ToolResult::error("Error: parameter \"task\" must be a non-empty string");
            }
            Some(_) => {
                return ToolResult::error("Error: parameter \"task\" must be a string");
            }
            None => {
                return ToolResult::error("Error: missing required parameter: \"task\"");
            }
        };

        let principles = format!(
            "Communication principles for: {}\n\
            \n\
            - Be concise and direct — lead with the answer, not the reasoning\n\
            - Skip filler words, preamble, and unnecessary transitions\n\
            - Don't restate what the user said — just do it\n\
            - When explaining, include only what's necessary for the user to understand\n\
            - If you can say it in one sentence, don't use three\n\
            - Focus text output on: decisions needing user input, high-level status updates, errors/blockers",
            task
        );

        ToolResult::ok(principles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{validate_params, Tool};

    #[test]
    fn brief_tool_name() {
        let tool = BriefTool::new();
        assert_eq!(tool.name(), "brief");
    }

    #[test]
    fn brief_tool_description_not_empty() {
        let tool = BriefTool::new();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn brief_tool_input_schema_has_task_required() {
        let tool = BriefTool::new();
        let schema = tool.input_schema();
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&serde_json::json!("task")));
    }

    #[test]
    fn brief_tool_execute_valid_task() {
        let tool = BriefTool::new();
        let mut params = HashMap::new();
        params.insert("task".into(), serde_json::json!("writing code"));
        let result = tool.execute(params);
        assert!(!result.is_error);
        assert!(result.output.contains("Communication principles"));
        assert!(result.output.contains("Be concise and direct"));
        assert!(result.output.contains("Skip filler words"));
        assert!(result.output.contains("Don't restate"));
        assert!(result.output.contains("include only what's necessary"));
        assert!(result.output.contains("one sentence"));
        assert!(result.output.contains("decisions needing user input"));
    }

    #[test]
    fn brief_tool_execute_missing_task() {
        let tool = BriefTool::new();
        let params: HashMap<String, Value> = HashMap::new();
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("task"));
    }

    #[test]
    fn brief_tool_execute_empty_task() {
        let tool = BriefTool::new();
        let mut params = HashMap::new();
        params.insert("task".into(), serde_json::json!(""));
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("non-empty"));
    }

    #[test]
    fn brief_tool_execute_whitespace_task() {
        let tool = BriefTool::new();
        let mut params = HashMap::new();
        params.insert("task".into(), serde_json::json!("   "));
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("non-empty"));
    }

    #[test]
    fn brief_tool_execute_non_string_task() {
        let tool = BriefTool::new();
        let mut params = HashMap::new();
        params.insert("task".into(), serde_json::json!(42));
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("must be a string"));
    }

    #[test]
    fn brief_tool_check_permissions_returns_none() {
        let tool = BriefTool::new();
        let params: HashMap<String, Value> = HashMap::new();
        assert!(tool.check_permissions(&params).is_none());
    }

    #[test]
    fn brief_tool_validate_params_missing_task() {
        let tool = BriefTool::new();
        let params: HashMap<String, Value> = HashMap::new();
        let result = validate_params(&tool, &params);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.is_error);
        assert!(r.output.contains("task"));
    }

    #[test]
    fn brief_tool_validate_params_with_task() {
        let tool = BriefTool::new();
        let mut params = HashMap::new();
        params.insert("task".into(), serde_json::json!("testing"));
        let result = validate_params(&tool, &params);
        assert!(result.is_none());
    }

    #[test]
    fn brief_tool_clone() {
        let tool = BriefTool::new();
        let cloned = tool.clone();
        assert_eq!(cloned.name(), "brief");
    }

    #[test]
    fn brief_tool_default() {
        let tool = BriefTool::default();
        assert_eq!(tool.name(), "brief");
    }
}
