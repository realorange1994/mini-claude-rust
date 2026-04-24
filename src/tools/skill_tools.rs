//! Skill tools - Skills system integration

use crate::tools::{Tool, ToolResult};
use crate::skills::Loader as SkillLoader;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub struct ReadSkillTool {
    loader: Arc<SkillLoader>,
}

impl ReadSkillTool {
    pub fn new(loader: Arc<SkillLoader>) -> Self {
        Self { loader }
    }
}

impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Read a skill's SKILL.md file. Use list_skills first to discover available skills."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the skill to read."
                }
            },
            "required": ["name"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("Error: name is required"),
        };

        match self.loader.load_skill(name) {
            Some(content) => ToolResult::ok(content),
            None => ToolResult::error(format!("Error: Skill not found: {}", name)),
        }
    }
}

pub struct ListSkillsTool {
    loader: Arc<SkillLoader>,
}

impl ListSkillsTool {
    pub fn new(loader: Arc<SkillLoader>) -> Self {
        Self { loader }
    }
}

impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }

    fn description(&self) -> &str {
        "List available skills. Shows name, description, and availability status."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, _params: HashMap<String, Value>) -> ToolResult {
        let skills = self.loader.list_skills();

        if skills.is_empty() {
            return ToolResult::ok("No skills found.".to_string());
        }

        let mut output = format!("Skills ({} total)\n", skills.len());

        for skill in skills {
            let status = if skill.available {
                "available".to_string()
            } else {
                "unavailable".to_string()
            };
            let always = if skill.always {
                " (always-on)".to_string()
            } else {
                String::new()
            };
            output.push_str(&format!(
                "  {} [{}{}] — {}\n",
                skill.name, status, always, skill.description
            ));
            if !skill.available && !skill.missing_deps.is_empty() {
                output.push_str(&format!("    Missing: {}\n", skill.missing_deps[0]));
            }
        }

        ToolResult::ok(output.trim().to_string())
    }
}
