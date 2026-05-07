//! Rules loader - Load rules from settings.json files

use std::path::Path;

use super::rule_parser::{parse_rules, ParsedRule};
use super::rule_store::RuleStore;

/// Permissions config from settings.json
#[derive(Debug, Default, serde::Deserialize)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

/// Load rules from a PermissionsConfig struct
pub fn load_rules_from_config(cfg: &PermissionsConfig, source: &str) -> Box<RuleStore> {
    let store = RuleStore::new();

    if !cfg.deny.is_empty() {
        let rules = parse_rules(&cfg.deny, "deny");
        store.add_rules(source, "deny", &rules);
    }

    if !cfg.ask.is_empty() {
        let rules = parse_rules(&cfg.ask, "ask");
        store.add_rules(source, "ask", &rules);
    }

    if !cfg.allow.is_empty() {
        let rules = parse_rules(&cfg.allow, "allow");
        store.add_rules(source, "allow", &rules);
    }

    Box::new(store)
}

/// Load rules from a settings.json file
pub fn load_rules_from_file(path: &Path) -> Result<(Box<RuleStore>, String), String> {
    let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let source = source_from_path(path);

    // Try to parse as full ClaudeSettings first
    #[derive(serde::Deserialize)]
    struct FullSettings {
        permissions: Option<PermissionsConfig>,
    }
    if let Ok(settings) = serde_json::from_str::<FullSettings>(&data) {
        if let Some(perms) = settings.permissions {
            return Ok((load_rules_from_config(&perms, &source), source));
        }
    }

    // Maybe it's just a permissions section alone
    if let Ok(perms) = serde_json::from_str::<PermissionsConfig>(&data) {
        return Ok((load_rules_from_config(&perms, &source), source));
    }

    Err(format!("failed to parse settings file: {}", path.display()))
}

/// Determine source label from a settings file path
fn source_from_path(path: &Path) -> String {
    let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let dir = path.parent().and_then(|p| p.to_str()).unwrap_or("");

    match file {
        "settings.json" => {
            if is_home_dir(dir) {
                "userSettings".to_string()
            } else {
                "projectSettings".to_string()
            }
        }
        "settings.local.json" => "localSettings".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Check if a directory path is the user's home .claude directory
fn is_home_dir(dir: &str) -> bool {
    let home = dirs::home_dir()
        .or_else(|| std::env::var("USERPROFILE").ok().map(std::path::PathBuf::from))
        .or_else(|| std::env::var("HOME").ok().map(std::path::PathBuf::from))
        .map(|p| p.join(".claude"))
        .and_then(|p| p.to_str().map(String::from));

    if let Some(home) = home {
        let abs_dir = std::path::Path::new(dir);
        let abs_home = std::path::Path::new(&home);
        abs_dir == abs_home
    } else {
        false
    }
}

/// Get the home .claude directory
fn home_claude_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir()
        .or_else(|| std::env::var("USERPROFILE").ok().map(std::path::PathBuf::from))
        .or_else(|| std::env::var("HOME").ok().map(std::path::PathBuf::from))
        .map(|p| p.join(".claude"))
}

/// Load rules from all available settings sources.
/// Returns a merged RuleStore with additive rules.
pub fn load_rules_from_all_sources(project_dir: &Path) -> Box<RuleStore> {
    let mut stores: Vec<Box<RuleStore>> = Vec::new();

    let load = |path: &std::path::Path| -> Option<Box<RuleStore>> {
        match load_rules_from_file(path) {
            Ok((store, _)) => Some(store),
            Err(_) => None,
        }
    };

    // Load from home directory first (lowest priority)
    if let Some(home) = home_claude_dir() {
        if let Some(store) = load(&home.join("settings.local.json")) {
            stores.push(store);
        }
        if let Some(store) = load(&home.join("settings.json")) {
            stores.push(store);
        }
    }

    // Load from project directory (highest priority)
    let proj_claude = project_dir.join(".claude");
    if let Some(store) = load(&proj_claude.join("settings.local.json")) {
        stores.push(store);
    }
    if let Some(store) = load(&proj_claude.join("settings.json")) {
        stores.push(store);
    }

    // Merge all stores (additive)
    if stores.is_empty() {
        return Box::new(RuleStore::new());
    }

    // Collect all rules from all stores and add to a new merged store
    let merged = Box::new(RuleStore::new());
    for store in &stores {
        // We need to access the inner rules - use get_all_rules as a proxy
        for behavior in &["deny", "ask", "allow"] {
            for rule in store.get_all_rules(behavior) {
                merged.add_rules("merged", behavior, &[rule]);
            }
        }
    }

    merged
}
