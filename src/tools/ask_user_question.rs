//! AskUserQuestionTool - allows the model to ask the user questions with multiple-choice options.

use crate::tools::{Tool, ToolResult};
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

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None // Always allowed - user must interact to proceed
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
