//! AskUserQuestionTool - allows the model to ask the user questions with multiple-choice options.

use crate::tools::{Tool, ToolResult, ToolPermissionResult};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

pub struct AskUserQuestionTool;

impl AskUserQuestionTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AskUserQuestionTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for AskUserQuestionTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn description(&self) -> &str {
        "Prompts the user with a multiple-choice question. Use this when you need to \
        clarify something before proceeding. The question is presented with 2-4 options, \
        and the user selects one by entering a number."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["questions"],
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["question", "header", "options"],
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The complete question to ask. Should be clear, specific, and end with a question mark."
                            },
                            "header": {
                                "type": "string",
                                "description": "Very short label displayed as a chip/tag (max 12 chars)."
                            },
                            "options": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "required": ["label", "description"],
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "The display text for this option (1-5 words)."
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "Explanation of what this option means or what will happen if chosen."
                                        }
                                    }
                                },
                                "description": "2-4 choices. Each option should be a distinct, mutually exclusive choice."
                            }
                        }
                    },
                    "description": "Questions to ask the user (1-4 questions)"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> ToolPermissionResult {
        ToolPermissionResult::passthrough() // Always allowed - user must interact to proceed
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let questions_raw = match params.get("questions") {
            Some(Value::Array(arr)) => arr,
            _ => return ToolResult::error("questions must be an array"),
        };

        if questions_raw.is_empty() {
            return ToolResult::error("at least one question is required");
        }
        if questions_raw.len() > 4 {
            return ToolResult::error("at most 4 questions allowed");
        }

        #[derive(Clone)]
        struct OptionData {
            label: String,
            description: String,
        }
        struct Question {
            question: String,
            header: String,
            options: Vec<OptionData>,
        }

        let mut questions = Vec::new();
        for raw in questions_raw {
            let m = match raw {
                Value::Object(m) => m,
                _ => continue,
            };
            let question = m.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let header = m.get("header").and_then(|v| v.as_str()).unwrap_or("").to_string();

            let opts_raw = match m.get("options") {
                Some(Value::Array(arr)) => arr,
                _ => return ToolResult::error("each question must have an options array"),
            };
            if opts_raw.len() < 2 {
                return ToolResult::error("each question must have at least 2 options");
            }
            if opts_raw.len() > 4 {
                return ToolResult::error("each question must have at most 4 options");
            }

            let mut options = Vec::new();
            for o_raw in opts_raw {
                let om = match o_raw {
                    Value::Object(m) => m,
                    _ => continue,
                };
                options.push(OptionData {
                    label: om.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    description: om.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                });
            }
            questions.push(Question { question, header, options });
        }

        let stdin = io::stdin();
        let mut answers = Vec::new();

        for q in &questions {
            println!("\n┌─ {} ──────────────────────────────────────", q.header);
            println!("│  {}", q.question);
            println!("│");
            for (i, opt) in q.options.iter().enumerate() {
                println!("│  {}. {} — {}", i + 1, opt.label, opt.description);
            }
            println!("│");
            print!("│  Enter a number (1-{}): ", q.options.len());
            io::stdout().flush().ok();

            loop {
                let mut input = String::new();
                match stdin.lock().read_line(&mut input) {
                    Ok(0) => return ToolResult::error("stdin closed while reading answer"),
                    Ok(_) => {}
                    Err(e) => return ToolResult::error(format!("failed to read input: {}", e)),
                }
                let input = input.trim();
                if let Ok(num) = input.parse::<usize>() {
                    if num >= 1 && num <= q.options.len() {
                        answers.push((q.question.clone(), q.options[num - 1].label.clone()));
                        println!("│  Selected: {}\n└─────────────────────────────────────────────", q.options[num - 1].label);
                        break;
                    }
                }
                print!("  Please enter a number between 1 and {}: ", q.options.len());
                io::stdout().flush().ok();
            }
        }

        // Build output summary
        let mut sb = String::new();
        for (i, (q, a)) in answers.iter().enumerate() {
            if i > 0 {
                sb.push('\n');
            }
            sb.push_str(&format!("Q: {}\nA: {}\n", q, a));
        }

        ToolResult::ok(sb)
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use serde_json::json;

    #[test]
    fn test_tool_name() {
        let tool = AskUserQuestionTool;
        assert_eq!(tool.name(), "AskUserQuestion");
    }

    #[test]
    fn test_tool_schema() {
        let tool = AskUserQuestionTool;
        let schema = tool.input_schema();
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str().unwrap(), "questions");
        let props = schema.get("properties").unwrap().as_object().unwrap();
        assert!(props.contains_key("questions"));
    }

    #[test]
    fn test_tool_permissions() {
        let tool = AskUserQuestionTool;
        // Should be passthrough (Auto approval)
        let perms = tool.permissions();
        assert!(!perms.is_empty());
    }

    #[test]
    fn test_execute_no_questions() {
        let tool = AskUserQuestionTool;
        let result = tool.execute(json!({}).as_object().unwrap().clone());
        assert!(result.is_error, "missing questions should return error");
    }

    #[test]
    fn test_execute_not_array() {
        let tool = AskUserQuestionTool;
        let result = tool.execute(json!({"questions": "not an array"}).as_object().unwrap().clone());
        assert!(result.is_error, "non-array questions should return error");
    }

    #[test]
    fn test_execute_empty_array() {
        let tool = AskUserQuestionTool;
        let result = tool.execute(json!({"questions": []}).as_object().unwrap().clone());
        assert!(result.is_error, "empty questions array should return error");
    }

    #[test]
    fn test_execute_too_many_questions() {
        let questions: Vec<serde_json::Value> = (0..5).map(|_| {
            json!({
                "question": "Q?",
                "header": "H",
                "options": [
                    {"label": "A", "description": "desc a"},
                    {"label": "B", "description": "desc b"}
                ]
            })
        }).collect();
        let tool = AskUserQuestionTool;
        let result = tool.execute(json!({"questions": questions}).as_object().unwrap().clone());
        assert!(result.is_error, "more than 4 questions should return error");
        assert!(result.output.contains("4"), "should mention 4 limit, got: {}", result.output);
    }

    #[test]
    fn test_execute_too_few_options() {
        let questions = json!([{
            "question": "Q?",
            "header": "H",
            "options": [{"label": "A", "description": "only one"}]
        }]);
        let tool = AskUserQuestionTool;
        let result = tool.execute(json!({"questions": questions}).as_object().unwrap().clone());
        assert!(result.is_error, "fewer than 2 options should return error");
    }

    #[test]
    fn test_execute_no_options_key() {
        let questions = json!([{"question": "Q?", "header": "H"}]);
        let tool = AskUserQuestionTool;
        let result = tool.execute(json!({"questions": questions}).as_object().unwrap().clone());
        assert!(result.is_error, "missing options should return error");
    }
}
