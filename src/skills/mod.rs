//! Skills loader - loads and parses skill definitions

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Skill metadata parsed from frontmatter
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub always: bool,
    #[allow(dead_code)]
    pub available: bool,
    #[allow(dead_code)]
    pub commands: Vec<String>,
    pub tags: Vec<String>,
    pub version: String,
    pub requires: Vec<String>,
    pub extended_requires_bins: Vec<String>,
    pub extended_requires_env: Vec<String>,
}

/// Skill info for listing
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub source: String, // "builtin" or "workspace"
    pub available: bool,
    pub always: bool,
    pub description: String,
    #[allow(dead_code)]
    pub commands: Vec<String>,
    #[allow(dead_code)]
    pub tags: Vec<String>,
    #[allow(dead_code)]
    pub version: String,
    pub missing_deps: Vec<String>,
}

/// Skill Loader
#[derive(Debug)]
pub struct Loader {
    workspace: PathBuf,
    builtin_dir: Option<PathBuf>,
    cache: RwLock<HashMap<String, String>>,
    skill_index: RwLock<HashMap<String, SkillInfo>>,
}

impl Loader {
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            builtin_dir: None,
            cache: RwLock::new(HashMap::new()),
            skill_index: RwLock::new(HashMap::new()),
        }
    }

    /// Set the builtin skills directory
    pub fn set_builtin_dir(&mut self, dir: &Path) {
        self.builtin_dir = Some(dir.to_path_buf());
    }

    /// Refresh the skill index
    pub fn refresh(&mut self) {
        let mut cache = self.cache.write().unwrap();
        let mut skill_index = self.skill_index.write().unwrap();

        cache.clear();
        skill_index.clear();

        // Scan builtin directory
        if let Some(ref builtin) = self.builtin_dir {
            self.scan_dir(builtin, "builtin", &mut cache, &mut skill_index);
        }

        // Scan workspace
        self.scan_dir(&self.workspace, "workspace", &mut cache, &mut skill_index);
    }

    fn scan_dir(&self, dir: &Path, source: &str, cache: &mut HashMap<String, String>, index: &mut HashMap<String, SkillInfo>) {
        if !dir.is_dir() {
            return;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_name = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();

                    let skill_md = path.join("SKILL.md");
                    if skill_md.exists() {
                        if let Ok(content) = fs::read_to_string(&skill_md) {
                            let meta = parse_frontmatter(&content);

                            // Check dependencies
                            let (available, missing_deps) = check_dependencies(
                                &meta.requires,
                                &meta.extended_requires_bins,
                                &meta.extended_requires_env,
                                dir,
                            );

                            let info = SkillInfo {
                                name: skill_name.clone(),
                                path: path.clone(),
                                source: source.to_string(),
                                available,
                                always: meta.always,
                                description: meta.description.clone(),
                                commands: meta.commands.clone(),
                                tags: meta.tags.clone(),
                                version: meta.version.clone(),
                                missing_deps,
                            };

                            cache.insert(skill_name.clone(), content);
                            index.insert(skill_name, info);
                        }
                    }
                }
            }
        }
    }

    /// Load a skill's SKILL.md content
    pub fn load_skill(&self, name: &str) -> Option<String> {
        let cache = self.cache.read().unwrap();
        cache.get(name).cloned()
    }

    /// List all skills
    pub fn list_skills(&self) -> Vec<SkillInfo> {
        let index = self.skill_index.read().unwrap();
        index.values().cloned().collect()
    }

    /// Get always-on skills
    pub fn get_always_skills(&self) -> Vec<SkillInfo> {
        let index = self.skill_index.read().unwrap();
        index.values()
            .filter(|s| s.always && s.available)
            .cloned()
            .collect()
    }

    /// Build system prompt for specific skills by name
    pub fn build_system_prompt_for_skills(&self, skill_names: &[String]) -> String {
        if skill_names.is_empty() {
            return String::new();
        }

        let cache = self.cache.read().unwrap();
        let skill_index = self.skill_index.read().unwrap();

        let mut output = String::from("\n# Active Skills\n\n");
        for name in skill_names {
            if let Some(info) = skill_index.get(name) {
                if info.available {
                    if let Some(content) = cache.get(name) {
                        output.push_str(&format!("### Skill: {}\n\n", info.name));
                        output.push_str(content);
                        output.push_str("\n\n");
                    }
                }
            }
        }

        output
    }

    /// Build skills summary for system prompt
    pub fn build_skills_summary(&self) -> String {
        let skills = self.list_skills();
        if skills.is_empty() {
            return String::new();
        }

        let mut output = String::from("<skills>\n");
        for skill in skills {
            let avail = if skill.available { "true" } else { "false" };
            let always = if skill.always { "true" } else { "false" };
            output.push_str(&format!(
                "  <skill available=\"{}\" always=\"{}\">\n",
                avail, always
            ));
            output.push_str(&format!("    <name>{}</name>\n", escape_xml(&skill.name)));
            output.push_str(&format!("    <description>{}</description>\n", escape_xml(&skill.description)));
            output.push_str(&format!("    <location>{}:{}</location>\n", skill.source, skill.name));
            if !skill.available && !skill.missing_deps.is_empty() {
                output.push_str(&format!("    <requires>{}</requires>\n", escape_xml(&skill.missing_deps.join(", "))));
            }
            output.push_str("  </skill>\n");
        }
        output.push_str("</skills>");
        output
    }
}

impl Clone for Loader {
    fn clone(&self) -> Self {
        Self {
            workspace: self.workspace.clone(),
            builtin_dir: self.builtin_dir.clone(),
            cache: RwLock::new(self.cache.read().unwrap().clone()),
            skill_index: RwLock::new(self.skill_index.read().unwrap().clone()),
        }
    }
}

/// Check if a binary exists in PATH (with Windows fallback)
fn binary_exists(name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(separator) {
            let bin_path = Path::new(dir).join(name);
            if bin_path.exists() {
                return true;
            }
            // Windows: try with extensions
            if cfg!(windows) {
                for ext in &[".exe", ".cmd", ".bat"] {
                    let bin_path = Path::new(dir).join(format!("{}{}", name, ext));
                    if bin_path.exists() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Check dependencies for a skill
pub fn check_dependencies(
    requires: &[String],
    bins: &[String],
    envs: &[String],
    skill_dir: &Path,
) -> (bool, Vec<String>) {
    let mut missing = Vec::new();

    // Check requires entries
    for req in requires {
        let req_upper = req.to_uppercase();

        // Check if it's an env var (uppercase name)
        if req_upper == *req && req.contains('_') {
            if std::env::var(&req_upper).is_err() {
                missing.push(format!("env:{}", req));
            }
            continue;
        }

        // Check if it's a binary
        if binary_exists(req) {
            continue;
        }

        // Check if it's a workspace file
        if skill_dir.join(req).exists() {
            continue;
        }

        // Check if it's a builtin file
        missing.push(format!("binary:{}", req));
    }

    // Check extended_requires.bins
    for bin in bins {
        if !binary_exists(bin) {
            missing.push(format!("binary:{}", bin));
        }
    }

    // Check extended_requires.env
    for env in envs {
        if std::env::var(env).is_err() {
            missing.push(format!("env:{}", env));
        }
    }

    let available = missing.is_empty();
    (available, missing)
}

pub fn parse_frontmatter(content: &str) -> SkillMeta {
    let mut meta = SkillMeta {
        name: String::new(),
        description: String::new(),
        always: false,
        available: true,
        commands: Vec::new(),
        tags: Vec::new(),
        version: String::new(),
        requires: Vec::new(),
        extended_requires_bins: Vec::new(),
        extended_requires_env: Vec::new(),
    };

    // Simple frontmatter parsing (YAML-like)
    let lines: Vec<&str> = content.lines().collect();
    let mut in_frontmatter = false;
    let mut frontmatter_lines = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if i == 0 && line.trim() == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter && line.trim() == "---" {
            break;
        }
        if in_frontmatter {
            frontmatter_lines.push(*line);
        }
    }

    // Parse multi-line lists and extended_requires
    let mut current_key = String::new();
    let mut in_list = false;

    for line in frontmatter_lines {
        let trimmed = line.trim();

        // Check if this is a new key-value pair
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim();
            let value = line[colon_pos + 1..].trim();

            // Check if this is a top-level key (not indented)
            if !line.starts_with(' ') && !line.starts_with('\t') {
                current_key = key.to_string();
                in_list = false;

                match key {
                    "name" => meta.name = value.to_string(),
                    "description" => meta.description = value.to_string(),
                    "always" => meta.always = value == "true",
                    "version" => meta.version = value.to_string(),
                    "requires" => {
                        if value.is_empty() {
                            in_list = true;
                        } else {
                            meta.requires = value.split(',').map(|s| s.trim().to_string()).collect();
                        }
                    }
                    "tags" => {
                        if value.is_empty() {
                            in_list = true;
                        } else {
                            meta.tags = value.split(',').map(|s| s.trim().to_string()).collect();
                        }
                    }
                    "extended_requires" => {
                        in_list = true;
                    }
                    _ => {}
                }
                continue;
            }
        }

        // Handle list items (indented lines starting with -)
        if in_list && trimmed.starts_with('-') {
            let item = trimmed[1..].trim().to_string();
            match current_key.as_str() {
                "requires" => meta.requires.push(item),
                "tags" => meta.tags.push(item),
                "extended_requires" => {
                    // Parse nested key: value
                    if let Some(colon_pos) = item.find(':') {
                        let sub_key = item[..colon_pos].trim();
                        let sub_value = item[colon_pos + 1..].trim();
                        match sub_key {
                            "bins" => {
                                if sub_value.is_empty() {
                                    // Will be filled by subsequent list items
                                } else {
                                    meta.extended_requires_bins = sub_value.split(',').map(|s| s.trim().to_string()).collect();
                                }
                            }
                            "env" => {
                                if sub_value.is_empty() {
                                    // Will be filled by subsequent list items
                                } else {
                                    meta.extended_requires_env = sub_value.split(',').map(|s| s.trim().to_string()).collect();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        // Handle indented list items for extended_requires.bins/env
        if current_key == "extended_requires" && (trimmed.starts_with("bins:") || trimmed.starts_with("env:")) {
            let sub_key = trimmed[..4].trim();
            let sub_value = trimmed[4..].trim();
            if sub_key == "bins" {
                if !sub_value.is_empty() {
                    meta.extended_requires_bins = sub_value.split(',').map(|s| s.trim().to_string()).collect();
                }
            } else if sub_key == "env:" {
                if !sub_value.is_empty() {
                    meta.extended_requires_env = sub_value.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            continue;
        }

        // Handle simple key: value pairs
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() == 2 {
                let key = parts[0].trim();
                let value = parts[1].trim();

                match key {
                    "name" => meta.name = value.to_string(),
                    "description" => meta.description = value.to_string(),
                    "always" => meta.always = value == "true",
                    "version" => meta.version = value.to_string(),
                    "requires" => meta.requires = value.split(',').map(|s| s.trim().to_string()).collect(),
                    "tags" => meta.tags = value.split(',').map(|s| s.trim().to_string()).collect(),
                    _ => {}
                }
            }
        }
    }

    meta
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
