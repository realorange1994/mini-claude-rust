//! Multi-source settings hierarchy.
//! Ported from upstream multi_settings.go (239 lines).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Precedence level of a settings source (higher overrides lower).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsLevel {
    /// Built-in defaults (lowest priority)
    Default,
    /// ~/.claude/settings.json
    Global,
    /// .claude/settings.json (project root)
    Project,
    /// .claude/settings.local.json (worktree-specific)
    Worktree,
    /// Runtime session overrides (highest priority)
    Session,
}

impl SettingsLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Global => "global",
            Self::Project => "project",
            Self::Worktree => "worktree",
            Self::Session => "session",
        }
    }
}

impl std::fmt::Display for SettingsLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A single settings file at a specific level.
#[derive(Debug, Clone)]
pub struct SettingsFile {
    pub level: SettingsLevel,
    pub path: String,
    pub values: HashMap<String, serde_json::Value>,
    pub loaded: bool,
}

/// 5-level settings hierarchy.
pub struct MultiSourceSettings {
    sources: Vec<SettingsFile>,
}

/// Returns the home directory for claude config (~/.claude).
fn home_claude_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

impl MultiSourceSettings {
    /// Create a settings manager with all 5 levels.
    pub fn new(project_dir: &str) -> Self {
        let mut ms = Self {
            sources: Vec::with_capacity(5),
        };

        // Level 0: defaults (no file, applied programmatically)
        ms.sources.push(SettingsFile {
            level: SettingsLevel::Default,
            path: String::new(),
            values: HashMap::new(),
            loaded: false,
        });

        // Level 1: global (~/.claude/settings.json)
        if let Some(home) = home_claude_dir() {
            ms.sources.push(SettingsFile {
                level: SettingsLevel::Global,
                path: home.join("settings.json").to_string_lossy().to_string(),
                values: HashMap::new(),
                loaded: false,
            });
        } else {
            ms.sources.push(SettingsFile {
                level: SettingsLevel::Global,
                path: String::new(),
                values: HashMap::new(),
                loaded: false,
            });
        }

        // Level 2: project (.claude/settings.json)
        let project_settings = if !project_dir.is_empty() {
            format!("{}/.claude/settings.json", project_dir)
        } else {
            String::new()
        };
        ms.sources.push(SettingsFile {
            level: SettingsLevel::Project,
            path: project_settings,
            values: HashMap::new(),
            loaded: false,
        });

        // Level 3: worktree (.claude/settings.local.json)
        let project_worktree = if !project_dir.is_empty() {
            format!("{}/.claude/settings.local.json", project_dir)
        } else {
            String::new()
        };
        ms.sources.push(SettingsFile {
            level: SettingsLevel::Worktree,
            path: project_worktree,
            values: HashMap::new(),
            loaded: false,
        });

        // Level 4: session (runtime only, no file)
        ms.sources.push(SettingsFile {
            level: SettingsLevel::Session,
            path: String::new(),
            values: HashMap::new(),
            loaded: false,
        });

        // Load all files
        for i in 0..ms.sources.len() {
            if !ms.sources[i].path.is_empty() {
                ms.load_file(i);
            }
        }

        ms
    }

    /// Read a settings file into the source at index i.
    fn load_file(&mut self, i: usize) {
        let path = self.sources[i].path.clone();
        match fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<HashMap<String, serde_json::Value>>(&data) {
                Ok(values) => {
                    self.sources[i].values = values;
                    self.sources[i].loaded = true;
                }
                Err(_) => {
                    self.sources[i].loaded = false;
                }
            },
            Err(_) => {
                self.sources[i].loaded = false;
            }
        }
    }

    /// Get the effective value for a key, respecting precedence.
    /// Higher levels override lower levels.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        // Search from highest to lowest priority
        for i in (0..self.sources.len()).rev() {
            if let Some(v) = self.sources[i].values.get(key) {
                return Some(v);
            }
        }
        None
    }

    /// Get a string value for a key.
    pub fn get_string(&self, key: &str) -> Option<String> {
        self.get(key).and_then(|v| v.as_str().map(|s| s.to_string()))
    }

    /// Get an integer value for a key.
    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.as_i64())
    }

    /// Get a bool value for a key.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(|v| v.as_bool())
    }

    /// Set a session-level override (highest priority).
    pub fn set_session(&mut self, key: &str, value: serde_json::Value) {
        self.sources[SettingsLevel::Session as usize]
            .values
            .insert(key.to_string(), value);
    }

    /// Get the fully merged settings map (all levels applied).
    pub fn merged(&self) -> HashMap<String, serde_json::Value> {
        let mut result = HashMap::new();
        for src in &self.sources {
            for (k, v) in &src.values {
                result.insert(k.clone(), v.clone());
            }
        }
        result
    }

    /// Return which level provides the effective value for a key.
    pub fn source_of(&self, key: &str) -> SettingsLevel {
        for i in (0..self.sources.len()).rev() {
            if self.sources[i].values.contains_key(key) {
                return self.sources[i].level;
            }
        }
        SettingsLevel::Default
    }

    /// Return all loaded settings sources.
    pub fn sources(&self) -> &[SettingsFile] {
        &self.sources
    }

    /// Get the path for a settings file at the given level.
    pub fn get_path(&self, level: SettingsLevel) -> Option<&str> {
        self.sources
            .get(level as usize)
            .map(|s| s.path.as_str())
            .filter(|p| !p.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_settings_levels() {
        assert_eq!(SettingsLevel::Default.as_str(), "default");
        assert_eq!(SettingsLevel::Global.as_str(), "global");
        assert_eq!(SettingsLevel::Project.as_str(), "project");
        assert_eq!(SettingsLevel::Worktree.as_str(), "worktree");
        assert_eq!(SettingsLevel::Session.as_str(), "session");
    }

    #[test]
    fn test_session_override() {
        let ms = MultiSourceSettings::new("/tmp/test_project");
        // Should have 5 levels
        assert_eq!(ms.sources.len(), 5);
    }

    #[test]
    fn test_set_and_get() {
        let mut ms = MultiSourceSettings::new("/tmp/test_project2");
        ms.set_session("test_key", json!("test_value"));
        assert_eq!(ms.get_string("test_key"), Some("test_value".to_string()));
    }

    #[test]
    fn test_precedence() {
        let mut ms = MultiSourceSettings::new("/tmp/test_project3");
        ms.set_session("key", json!("session"));
        assert_eq!(ms.source_of("key"), SettingsLevel::Session);
    }
}
