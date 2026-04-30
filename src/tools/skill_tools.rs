//! Skill tools - ReadSkill, ListSkills, and SearchSkill

use crate::skills::Loader;
use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Character budget for search results output
const CHAR_BUDGET: usize = 4000;

// ─── ReadSkillTool ───

pub struct ReadSkillTool {
    loader: Arc<Loader>,
}

impl ReadSkillTool {
    pub fn new(loader: Arc<Loader>) -> Self {
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

// ─── ListSkillsTool ───

pub struct ListSkillsTool {
    loader: Arc<Loader>,
}

impl ListSkillsTool {
    pub fn new(loader: Arc<Loader>) -> Self {
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
            return ToolResult::ok("No skills available.".to_string());
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
                "  {} [{}{}] -- {}",
                skill.name, status, always, skill.description
            ));
            if let Some(when) = &skill.when_to_use {
                output.push_str(&format!(" ({})", when));
            }
            output.push('\n');
            if !skill.available && !skill.missing_deps.is_empty() {
                output.push_str(&format!("    Missing: {}\n", skill.missing_deps.join(", ")));
            }
        }

        output.push_str("\nUse search_skills to find skills by topic. Use read_skill to load full instructions.");

        ToolResult::ok(output.trim().to_string())
    }
}

// ─── SearchSkillTool ───

pub struct SearchSkillTool {
    loader: Arc<Loader>,
}

impl SearchSkillTool {
    pub fn new(loader: Arc<Loader>) -> Self {
        Self { loader }
    }
}

impl Tool for SearchSkillTool {
    fn name(&self) -> &str {
        "search_skills"
    }

    fn description(&self) -> &str {
        "Search available skills by name, description, tags, or usage guidance. \
         Use this to discover skills relevant to your current task before attempting \
         to build functionality from scratch."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query - skill name, topic, or description keyword"
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
            Some(q) if !q.trim().is_empty() => q.trim().to_string(),
            _ => return ToolResult::error("Error: query is required"),
        };

        let skills = self.loader.list_skills();
        if skills.is_empty() {
            return ToolResult::ok("No skills available.".to_string());
        }

        let query_lower = query.to_lowercase();
        let query_terms: Vec<&str> = query_lower.split_whitespace().collect();
        if query_terms.is_empty() {
            return ToolResult::error("Error: query is required");
        }

        // Score each skill
        let mut scored: Vec<(&crate::skills::SkillInfo, f64)> = Vec::new();
        for skill in &skills {
            let score = score_skill(skill, &query_terms);
            if score > 0.0 {
                scored.push((skill, score));
            }
        }

        // Sort by score descending
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        if scored.is_empty() {
            return ToolResult::ok(format!(
                "No skills found for \"{}\". Use list_skills to see all available skills.",
                query
            ));
        }

        // Format results within budget
        format_results(&scored, &query)
    }
}

/// Score a skill against query terms (lightweight relevance scoring)
fn score_skill(skill: &crate::skills::SkillInfo, query_terms: &[&str]) -> f64 {
    let mut score = 0.0;
    let name_lower = skill.name.to_lowercase();
    let desc_lower = skill.description.to_lowercase();
    let when_lower = skill.when_to_use.as_ref().map(|s| s.to_lowercase()).unwrap_or_default();
    let tags_lower: Vec<String> = skill.tags.iter().map(|t| t.to_lowercase()).collect();

    for term in query_terms {
        // Exact name match
        if name_lower == *term {
            score += 100.0;
        }
        // Name contains term
        else if name_lower.contains(term) {
            score += 50.0;
        }

        // Description contains term
        if desc_lower.contains(term) {
            score += 20.0;
        }

        // Tag exact match
        if tags_lower.iter().any(|t| t == term || t.contains(term)) {
            score += 30.0;
        }

        // when_to_use contains term
        if when_lower.contains(term) {
            score += 15.0;
        }
    }

    score
}

/// Format search results within character budget
fn format_results(
    scored: &[(&crate::skills::SkillInfo, f64)],
    query: &str,
) -> ToolResult {
    let mut output = format!("Matching skills for \"{}\":\n", query);

    let mut included = 0;
    for (skill, _score) in scored {
        let mut entry = format!("  - **{}**: {}", skill.name, skill.description);
        if let Some(when) = &skill.when_to_use {
            entry.push_str(&format!(" ({})", when));
        }
        if !skill.tags.is_empty() {
            entry.push_str(&format!(" [{}]", skill.tags.join(", ")));
        }
        if !skill.available {
            entry.push_str(" (unavailable)");
        }
        entry.push('\n');

        if output.len() + entry.len() > CHAR_BUDGET {
            break;
        }

        output.push_str(&entry);
        included += 1;
    }

    let remaining = scored.len() - included;
    output.push_str(&format!(
        "\nFound {} skill(s). Use read_skill to load full instructions.",
        scored.len()
    ));
    if remaining > 0 {
        output.push_str(&format!(" (+{} more, use search_skills with different terms)", remaining));
    }

    ToolResult::ok(output.trim().to_string())
}
