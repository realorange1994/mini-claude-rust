use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// NotificationChannels lists the available notification channel options
pub const NOTIFICATION_CHANNELS: &[&str] = &[
    "auto",
    "iterm2",
    "iterm2_with_bell",
    "terminal_bell",
    "kitty",
    "ghostty",
    "notifications_disabled",
];

// EditorModes lists the valid editor modes
pub const EDITOR_MODES: &[&str] = &["normal", "vim"];

// TeammateModes lists the valid teammate modes for spawning
pub const TEAMMATE_MODES: &[&str] = &["auto", "tmux", "in-process"];

/// FeatureFlag is a boolean feature flag with a name
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub name: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// FeatureFlagStore persists feature flags to a JSON file in .claude/
pub struct FeatureFlagStore {
    file: PathBuf,
    flags: Mutex<HashMap<String, FeatureFlag>>,
}

impl FeatureFlagStore {
    /// Creates a store backed by .claude/feature_flags.json
    pub fn new(base_dir: &Path) -> Self {
        let file = base_dir.join(".claude").join("feature_flags.json");
        let flags = HashMap::new();

        let mut store = Self {
            file,
            flags: Mutex::new(flags),
        };

        if let Ok(data) = fs::read_to_string(&store.file) {
            if let Ok(mut loaded) = serde_json::from_str::<HashMap<String, FeatureFlag>>(&data) {
                // Set names for deserialized flags
                for (name, mut f) in loaded.drain() {
                    f.name = name.clone();
                    store.flags.lock().unwrap().insert(name, f);
                }
            }
        }

        store
    }

    /// Checks if a feature flag is enabled
    pub fn enabled(&self, name: &str) -> bool {
        let flags = self.flags.lock().unwrap();
        if let Some(f) = flags.get(name) {
            f.enabled
        } else {
            false
        }
    }

    /// Sets a feature flag to enabled
    pub fn enable(&self, name: &str, description: Option<&str>) {
        let mut flags = self.flags.lock().unwrap();
        flags.insert(
            name.to_string(),
            FeatureFlag {
                name: name.to_string(),
                enabled: true,
                description: description.map(|s| s.to_string()),
            },
        );
        self.save(&flags);
    }

    /// Sets a feature flag to disabled
    pub fn disable(&self, name: &str) {
        let mut flags = self.flags.lock().unwrap();
        if let Some(f) = flags.get_mut(name) {
            f.enabled = false;
            self.save(&flags);
        }
    }

    /// Returns all registered flags
    pub fn list(&self) -> Vec<FeatureFlag> {
        let flags = self.flags.lock().unwrap();
        flags.values().cloned().collect()
    }

    fn save(&self, flags: &HashMap<String, FeatureFlag>) {
        if let Ok(data) = serde_json::to_string_pretty(flags) {
            let _ = fs::write(&self.file, data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_feature_flag_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = FeatureFlagStore::new(dir.path());

        assert!(!store.enabled("test_flag"));

        store.enable("test_flag", Some("A test flag"));
        assert!(store.enabled("test_flag"));

        store.disable("test_flag");
        assert!(!store.enabled("test_flag"));
    }

    #[test]
    fn test_feature_flag_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = FeatureFlagStore::new(dir.path());

        store.enable("flag1", Some("First flag"));
        store.enable("flag2", None);

        let list = store.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_notification_channels() {
        assert!(NOTIFICATION_CHANNELS.contains(&"auto"));
        assert!(NOTIFICATION_CHANNELS.contains(&"terminal_bell"));
    }

    #[test]
    fn test_editor_modes() {
        assert!(EDITOR_MODES.contains(&"normal"));
        assert!(EDITOR_MODES.contains(&"vim"));
    }

    #[test]
    fn test_teammate_modes() {
        assert!(TEAMMATE_MODES.contains(&"auto"));
        assert!(TEAMMATE_MODES.contains(&"tmux"));
    }
}
