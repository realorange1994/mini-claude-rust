//! Skills loader for skill definitions from disk.
//! Ported from upstream skills/loader.go (831 lines).
//!
//! Provides loading, parsing, and discovery of SKILL.md files
//! for both builtin and workspace skills.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Represents the parsed frontmatter of a SKILL.md file.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub always: bool,
    pub available: bool,
    pub commands: Vec<String>,
    pub tags: Vec<String>,
    pub version: String,
    pub requires: Vec<String>,
    pub ext_bins: Vec<String>,
    pub ext_env: Vec<String>,
    pub when_to_use: String,
    pub paths: Vec<String>,
}

/// Skill info with computed properties.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub path: String,
    pub source: String, // "builtin" or "workspace"
    pub available: bool,
    pub always: bool,
    pub description: String,
    pub commands: Vec<String>,
    pub tags: Vec<String>,
    pub version: String,
    pub missing_deps: Vec<String>,
    pub when_to_use: String,
    pub paths: Vec<String>,
}

/// Represents a skill discovered from an MCP server resource.
#[derive(Debug, Clone)]
pub struct MCPSkillResource {
    pub server: String,
    pub uri: String,
    pub name: String,
    pub description: String,
    pub content: String,
}

/// Interface trait for MCP skill discovery.
/// Deliberately avoids direct import of the mcp package.
pub trait MCPResourceClient: Send + Sync {
    /// Discover skill resources from MCP servers.
    fn discover_skill_resources(&self) -> Vec<MCPSkillResource>;
}

/// Orchestrates skill discovery from MCP servers.
pub struct MCPSkillDiscovery {
    client: Box<dyn MCPResourceClient>,
}

impl MCPSkillDiscovery {
    /// Create a new MCP skill discovery instance.
    pub fn new(client: Box<dyn MCPResourceClient>) -> Self {
        Self { client }
    }

    /// Discover skills from MCP server resources.
    /// Queries for skill:// resources and converts to SkillInfo.
    pub fn discover_skills(&self) -> Vec<SkillInfo> {
        let resources = self.client.discover_skill_resources();
        if resources.is_empty() {
            return Vec::new();
        }

        resources
            .into_iter()
            .map(|r| SkillInfo {
                name: r.name.clone(),
                path: r.uri.clone(),
                source: format!("mcp:{}", r.server),
                available: true,
                always: false,
                description: r.description.clone(),
                commands: vec![format!("/{}", r.name)],
                tags: Vec::new(),
                version: String::new(),
                missing_deps: Vec::new(),
                when_to_use: String::new(),
                paths: Vec::new(),
            })
            .collect()
    }

    /// Load skill content by name from a discovered MCP skill.
    pub fn load_skill_content(&self, name: &str) -> Option<String> {
        for r in self.client.discover_skill_resources() {
            if r.name == name {
                return Some(r.content);
            }
        }
        None
    }
}

/// Bridges an MCP manager to the MCPResourceClient interface.
pub struct MCPManagerAdapter {
    discover_fn: Box<dyn Fn() -> Vec<MCPSkillResource> + Send + Sync>,
}

impl MCPManagerAdapter {
    /// Create a new adapter with a discovery function.
    pub fn new<F>(discover_fn: F) -> Self
    where
        F: Fn() -> Vec<MCPSkillResource> + Send + Sync + 'static,
    {
        Self {
            discover_fn: Box::new(discover_fn),
        }
    }
}

impl MCPResourceClient for MCPManagerAdapter {
    fn discover_skill_resources(&self) -> Vec<MCPSkillResource> {
        (self.discover_fn)()
    }
}

impl SkillInfo {
    /// Check if this skill applies to the current project directory.
    pub fn is_applicable(&self, project_dir: &str) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        let base = Path::new(project_dir).file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        for pattern in &self.paths {
            // Simple glob: * matches any segment
            if pattern == "*" || pattern == "**" || pattern == &base {
                return true;
            }
            // Pattern like "*/my-project"
            if pattern.starts_with("*/") && pattern.len() > 2 {
                if base == pattern[2..] {
                    return true;
                }
            }
            // Pattern like "my-project/*"
            if pattern.ends_with("/*") && pattern.len() > 2 {
                let prefix = &pattern[..pattern.len() - 2];
                if project_dir.ends_with(prefix) {
                    return true;
                }
            }
        }
        false
    }
}

/// Skills loader from disk.
pub struct SkillLoader {
    workspace_dir: PathBuf,
    builtin_dir: Option<PathBuf>,
    skills: HashMap<String, (SkillInfo, String)>, // name -> (info, content)
}

impl SkillLoader {
    /// Create a new loader.
    pub fn new(workspace_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
            builtin_dir: None,
            skills: HashMap::new(),
        }
    }

    /// Set the builtin skills directory.
    pub fn set_builtin_dir(&mut self, dir: impl Into<PathBuf>) {
        self.builtin_dir = Some(dir.into());
    }

    /// Re-scan skill directories and rebuild the index.
    pub fn refresh(&mut self) -> std::io::Result<()> {
        self.skills.clear();

        if let Some(builtin_dir) = self.builtin_dir.clone() {
            self.scan_dir(&builtin_dir, "builtin")?;
        }
        let workspace_dir = self.workspace_dir.clone();
        self.scan_dir(&workspace_dir, "workspace")?;

        Ok(())
    }

    /// Scan a directory for skills.
    fn scan_dir(&mut self, dir: &Path, source: &str) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = path.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let skill_file = path.join("SKILL.md");
            if !skill_file.exists() {
                continue;
            }
            // Workspace overrides builtin
            if self.skills.contains_key(&name) {
                continue;
            }

            if let Ok(content) = fs::read_to_string(&skill_file) {
                let meta = parse_frontmatter(&content);
                let info = self.build_skill_info(
                    &name,
                    &skill_file.to_string_lossy(),
                    source,
                    &meta,
                );
                self.skills.insert(name, (info, content));
            }
        }

        Ok(())
    }

    /// Build SkillInfo from metadata.
    fn build_skill_info(
        &self,
        name: &str,
        path: &str,
        source: &str,
        meta: &SkillMeta,
    ) -> SkillInfo {
        let mut missing_deps = Vec::new();
        let mut available = meta.available;

        for req in &meta.requires {
            if !self.check_dependency(req) {
                available = false;
                missing_deps.push(format!("Missing: {}", req));
            }
        }

        for bin in &meta.ext_bins {
            if !exists_in_path(bin) {
                available = false;
                missing_deps.push(format!("CLI: {}", bin));
            }
        }

        for env in &meta.ext_env {
            if std::env::var(env).unwrap_or_default().is_empty() {
                available = false;
                missing_deps.push(format!("ENV: {}", env));
            }
        }

        SkillInfo {
            name: name.to_string(),
            path: path.to_string(),
            source: source.to_string(),
            available,
            always: meta.always,
            description: meta.description.clone(),
            commands: meta.commands.clone(),
            tags: meta.tags.clone(),
            version: meta.version.clone(),
            missing_deps,
            when_to_use: meta.when_to_use.clone(),
            paths: meta.paths.clone(),
        }
    }

    /// Check if a dependency is met.
    fn check_dependency(&self, req: &str) -> bool {
        // Check if it's an environment variable
        if is_env_var_name(req) && std::env::var(req).unwrap_or_default().is_empty() == false {
            return true;
        }
        if exists_in_path(req) {
            return true;
        }
        // Check if it's another skill
        self.skills.contains_key(req)
    }

    /// Get the full SKILL.md content.
    pub fn load_skill(&self, name: &str) -> Option<String> {
        self.skills.get(name).map(|(_, content)| content.clone())
    }

    /// Get always-on skills that are available.
    pub fn get_always_skills(&self) -> Vec<SkillInfo> {
        let mut result: Vec<SkillInfo> = self
            .skills
            .values()
            .filter(|(info, _)| info.always && info.available)
            .map(|(info, _)| info.clone())
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// List all skills, optionally filtering unavailable.
    pub fn list_skills(&self, filter_unavailable: bool) -> Vec<SkillInfo> {
        let mut result: Vec<SkillInfo> = self
            .skills
            .values()
            .filter(|(info, _)| !filter_unavailable || info.available)
            .map(|(info, _)| info.clone())
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// List skills applicable to a project directory.
    pub fn list_skills_for_project(
        &self,
        project_dir: &str,
        filter_unavailable: bool,
    ) -> Vec<SkillInfo> {
        let mut result: Vec<SkillInfo> = self
            .skills
            .values()
            .filter(|(info, _)| {
                (!filter_unavailable || info.available) && info.is_applicable(project_dir)
            })
            .map(|(info, _)| info.clone())
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// Build a system prompt section from skills.
    pub fn build_system_prompt(&self, names: &[String]) -> String {
        if names.is_empty() {
            return String::new();
        }

        let mut parts = Vec::new();
        parts.push("\n# Active Skills\n".to_string());

        for name in names {
            if let Some((info, content)) = self.skills.get(name) {
                if info.available {
                    parts.push(format!("### Skill: {}\n\n{}\n", info.name, content));
                }
            }
        }

        parts.join("\n\n---\n\n")
    }

    /// Expand skill variables in content.
    pub fn expand_skill_variables(
        content: &str,
        skill_dir: &str,
        session_id: &str,
    ) -> String {
        let replacements = [
            ("${CLAUDE_SKILL_DIR}", skill_dir.to_string()),
            ("${CLAUDE_SESSION_ID}", session_id.to_string()),
            (
                "${CLAUDE_PROJECT_DIR}",
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            (
                "${CLAUDE_CONFIG_DIR}",
                dirs::home_dir()
                    .map(|h| h.join(".claude").to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
        ];

        let mut result = content.to_string();
        for (placeholder, value) in replacements {
            result = result.replace(placeholder, &value);
        }
        result
    }
}

/// Parse frontmatter from SKILL.md content.
pub fn parse_frontmatter(content: &str) -> SkillMeta {
    if !content.starts_with("---") {
        return SkillMeta {
            available: true,
            ..Default::default()
        };
    }

    let rest = &content[3..];
    let end_idx = rest.find("\n---").unwrap_or(rest.len());
    let frontmatter = &rest[..end_idx];

    let mut meta = SkillMeta {
        available: true,
        ..Default::default()
    };

    let mut current_key = String::new();

    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();

        // Multi-line list item
        if indent > 0 && trimmed.starts_with("- ") {
            let val = trimmed[2..].trim().to_string();
            match current_key.as_str() {
                "requires" => meta.requires.push(val),
                "commands" => meta.commands.push(val),
                "tags" => meta.tags.push(val),
                "bins" => meta.ext_bins.push(val),
                "env" => meta.ext_env.push(val),
                "paths" => meta.paths.push(val),
                _ => {}
            }
            continue;
        }

        if let Some(colon_idx) = trimmed.find(':') {
            let key = trimmed[..colon_idx].trim();
            let val = trimmed[colon_idx + 1..].trim();

            // Remove inline comments
            let val = if let Some(idx) = val.find(" #") {
                val[..idx].trim()
            } else {
                val
            };

            if val.is_empty() {
                current_key = key.to_string();
                continue;
            }

            current_key.clear();

            let val = unquote(val);

            match key {
                "name" => meta.name = val,
                "description" => meta.description = val,
                "always" => meta.always = parse_bool(&val),
                "available" => meta.available = parse_bool(&val),
                "version" => meta.version = val,
                "commands" => meta.commands = parse_inline_list(&val),
                "tags" => meta.tags = parse_inline_list(&val),
                "requires" => {
                    if val.starts_with('[') {
                        meta.requires = parse_inline_list(&val);
                    } else {
                        current_key = "requires".to_string();
                    }
                }
                "when_to_use" => meta.when_to_use = val,
                "paths" => {
                    if val.starts_with('[') {
                        meta.paths = parse_inline_list(&val);
                    } else {
                        current_key = "paths".to_string();
                    }
                }
                _ => {}
            }
        }
    }

    meta
}

impl Default for SkillMeta {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            always: false,
            available: true,
            commands: Vec::new(),
            tags: Vec::new(),
            version: String::new(),
            requires: Vec::new(),
            ext_bins: Vec::new(),
            ext_env: Vec::new(),
            when_to_use: String::new(),
            paths: Vec::new(),
        }
    }
}

fn parse_bool(s: &str) -> bool {
    matches!(s, "true" | "yes" | "True" | "Yes")
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'"' && bytes[s.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[s.len() - 1] == b'\'')
        {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

fn parse_inline_list(s: &str) -> Vec<String> {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('[') {
        return Vec::new();
    }

    let s = &s[1..];
    let s = s.trim_end_matches(']').trim();
    if s.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quote = None;

    for c in s.chars() {
        match (in_quote, c) {
            (None, '"' | '\'') => in_quote = Some(c),
            (Some(q), c2) if c2 == q => in_quote = None,
            (None, ',') => {
                result.push(unquote(current.trim()));
                current.clear();
            }
            _ => current.push(c),
        }
    }

    if !current.is_empty() {
        result.push(unquote(current.trim()));
    }

    result
}

fn exists_in_path(bin: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        // Try with and without extensions
        if which::which(bin).is_ok() {
            return true;
        }
        let lower = bin.to_lowercase();
        for ext in &[".exe", ".cmd", ".bat"] {
            if !lower.ends_with(ext) {
                if which::which(format!("{}{}", bin, ext)).is_ok() {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(target_os = "windows"))]
    {
        which::which(bin).is_ok()
    }
}

fn is_env_var_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && s.chars().next().unwrap().is_ascii_uppercase()
}

/// Strip frontmatter from SKILL.md content.
pub fn strip_frontmatter(content: &str) -> &str {
    if !content.starts_with("---") {
        return content.trim();
    }

    let rest = &content[3..];
    if let Some(end_idx) = rest.find("\n---") {
        return rest[end_idx + 4..].trim();
    }

    content.trim()
}

/// Parse frontmatter from SKILL.md content (public API with validation).
/// Returns an error if frontmatter delimiters are missing or both name and description are empty.
pub fn parse_frontmatter_public(content: &str) -> Result<SkillMeta, String> {
    if !content.starts_with("---") {
        return Err("Content does not start with frontmatter delimiter '---'".to_string());
    }

    let rest = &content[3..];
    let end_idx = rest.find("\n---").ok_or("Missing closing frontmatter delimiter '---'")?;
    let frontmatter = &rest[..end_idx];

    if frontmatter.trim().is_empty() {
        return Err("Frontmatter section is empty".to_string());
    }

    // Build a full content string to parse (with the opening --- already consumed)
    let full_content = &content[..3 + end_idx + 4];
    let meta = parse_frontmatter(full_content);

    if meta.name.is_empty() && meta.description.is_empty() {
        return Err("Frontmatter must contain either 'name' or 'description'".to_string());
    }

    Ok(meta)
}

/// Expand skill variables in content (alias matching Go's ExpandSkillVariables).
/// Replaces ${CLAUDE_SKILL_DIR}, ${CLAUDE_SESSION_ID}, ${CLAUDE_PROJECT_DIR}, ${CLAUDE_CONFIG_DIR}.
pub fn expand_skill_variables(
    content: &str,
    skill_dir: &str,
    session_id: &str,
) -> String {
    SkillLoader::expand_skill_variables(content, skill_dir, session_id)
}

/// Expand skill content with arguments.
/// Replaces $ARGUMENTS with all args joined, and $ARG_<NAME> with corresponding values.
pub fn expand_skill_content(content: &str, args: &std::collections::HashMap<String, String>) -> String {
    let mut result = content.to_string();

    // Replace $ARGUMENTS with all values joined
    if let Some(all_args) = args.get("ARGUMENTS") {
        result = result.replace("$ARGUMENTS", all_args);
    }

    // Replace $ARG_<NAME> with corresponding values
    for (key, value) in args {
        if key.starts_with("ARG_") {
            result = result.replace(&format!("${}", key), value);
        }
    }

    result
}

/// Escape XML special characters in a string.
/// Replaces & -> &amp;, < -> &lt;, > -> &gt;, " -> &quot;, ' -> &apos;.
pub fn escape_xml(s: &str) -> String {
    let s = s.replace('&', "&amp;");
    let s = s.replace('<', "&lt;");
    let s = s.replace('>', "&gt;");
    let s = s.replace('"', "&quot;");
    s.replace('\'', "&apos;")
}

/// Build an XML-formatted summary of available skills.
/// Returns a <skills> root element with <skill> children for each skill.
pub fn build_skills_summary(loader: &SkillLoader) -> String {
    let mut parts = Vec::new();
    parts.push("<skills>".to_string());

    let mut skills: Vec<_> = loader.skills.values().collect();
    skills.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    for (info, _) in skills {
        let avail = info.available.to_string();
        let always = info.always.to_string();
        parts.push(format!(
            "  <skill available=\"{}\" always=\"{}\">",
            escape_xml(&avail), escape_xml(&always)
        ));
        parts.push(format!("    <name>{}</name>", escape_xml(&info.name)));
        parts.push(format!("    <description>{}</description>", escape_xml(&info.description)));
        parts.push(format!("    <location>{}</location>", escape_xml(&format!("{}:{}", info.source, info.name))));

        if !info.available && !info.missing_deps.is_empty() {
            parts.push(format!(
                "    <requires>{}</requires>",
                escape_xml(&info.missing_deps.join(", "))
            ));
        }

        parts.push("  </skill>".to_string());
    }

    parts.push("</skills>".to_string());
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_inline_list() {
        assert_eq!(parse_inline_list("[a, b, c]"), vec!["a", "b", "c"]);
        assert_eq!(parse_inline_list("[\"a\", \"b\"]"), vec!["a", "b"]);
        assert!(parse_inline_list("[]").is_empty());
        assert!(parse_inline_list("not a list").is_empty());
    }

    #[test]
    fn test_parse_bool() {
        assert!(parse_bool("true"));
        assert!(parse_bool("yes"));
        assert!(!parse_bool("false"));
        assert!(!parse_bool("no"));
    }

    #[test]
    fn test_unquote() {
        assert_eq!(unquote("\"hello\""), "hello");
        assert_eq!(unquote("'hello'"), "hello");
        assert_eq!(unquote("hello"), "hello");
    }

    #[test]
    fn test_strip_frontmatter() {
        let content = "---\nname: test\n---\n\nContent here";
        assert_eq!(strip_frontmatter(content), "Content here");
    }

    #[test]
    fn test_is_env_var_name() {
        assert!(is_env_var_name("HOME"));
        assert!(is_env_var_name("MY_VAR_123"));
        assert!(!is_env_var_name("lower"));
        assert!(!is_env_var_name(""));
    }

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\nname: my-skill\ndescription: A test skill\nalways: true\ncommands: [/my-skill]\ntags: [test, demo]\n---\n\nContent here";
        let meta = parse_frontmatter(content);
        assert_eq!(meta.name, "my-skill");
        assert_eq!(meta.description, "A test skill");
        assert!(meta.always);
        assert_eq!(meta.commands, vec!["/my-skill"]);
        assert_eq!(meta.tags, vec!["test", "demo"]);
    }
}
