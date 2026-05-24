//! Model capabilities and caching.
//!
//! Provides model capability lookup, disk caching, and default fallbacks.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;

/// Model capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_window: i64,
    pub max_output_tokens: i64,
    pub max_thinking_tokens: i64,
    pub supports_vision: bool,
    pub supports_thinking: bool,
}

/// Default model capabilities as fallback
pub static DEFAULT_MODEL_CAPABILITIES: Lazy<HashMap<&'static str, ModelCapabilities>> =
    Lazy::new(|| {
        let mut m = HashMap::new();
        m.insert(
            "claude-sonnet-4-6-20260125",
            ModelCapabilities {
                context_window: 1_000_000,
                max_output_tokens: 64000,
                max_thinking_tokens: 32000,
                supports_vision: true,
                supports_thinking: true,
            },
        );
        m.insert(
            "claude-opus-4-6-20260302",
            ModelCapabilities {
                context_window: 1_000_000,
                max_output_tokens: 64000,
                max_thinking_tokens: 32000,
                supports_vision: true,
                supports_thinking: true,
            },
        );
        m.insert(
            "claude-haiku-4-5-20250610",
            ModelCapabilities {
                context_window: 200_000,
                max_output_tokens: 8192,
                max_thinking_tokens: 4096,
                supports_vision: true,
                supports_thinking: true,
            },
        );
        m.insert(
            "claude-sonnet-4-5-20250929",
            ModelCapabilities {
                context_window: 1_000_000,
                max_output_tokens: 64000,
                max_thinking_tokens: 32000,
                supports_vision: true,
                supports_thinking: true,
            },
        );
        m.insert(
            "claude-sonnet-4-20250514",
            ModelCapabilities {
                context_window: 1_000_000,
                max_output_tokens: 64000,
                max_thinking_tokens: 32000,
                supports_vision: true,
                supports_thinking: true,
            },
        );
        m.insert(
            "claude-opus-4-5-20250610",
            ModelCapabilities {
                context_window: 1_000_000,
                max_output_tokens: 64000,
                max_thinking_tokens: 32000,
                supports_vision: true,
                supports_thinking: true,
            },
        );
        m.insert(
            "claude-opus-4-20250514",
            ModelCapabilities {
                context_window: 1_000_000,
                max_output_tokens: 32000,
                max_thinking_tokens: 32000,
                supports_vision: true,
                supports_thinking: true,
            },
        );
        // Legacy model IDs
        m.insert(
            "claude-3-5-sonnet-20241022",
            ModelCapabilities {
                context_window: 200_000,
                max_output_tokens: 8192,
                max_thinking_tokens: 4096,
                supports_vision: true,
                supports_thinking: false,
            },
        );
        m.insert(
            "claude-3-5-haiku-20241022",
            ModelCapabilities {
                context_window: 200_000,
                max_output_tokens: 8192,
                max_thinking_tokens: 4096,
                supports_vision: true,
                supports_thinking: false,
            },
        );
        m.insert(
            "claude-3-opus-20240229",
            ModelCapabilities {
                context_window: 200_000,
                max_output_tokens: 4096,
                max_thinking_tokens: 0,
                supports_vision: true,
                supports_thinking: false,
            },
        );
        m.insert(
            "claude-3-sonnet-20240229",
            ModelCapabilities {
                context_window: 200_000,
                max_output_tokens: 4096,
                max_thinking_tokens: 0,
                supports_vision: true,
                supports_thinking: false,
            },
        );
        m.insert(
            "claude-3-haiku-20240307",
            ModelCapabilities {
                context_window: 200_000,
                max_output_tokens: 4096,
                max_thinking_tokens: 0,
                supports_vision: true,
                supports_thinking: false,
            },
        );
        m
    });

/// Model capabilities cache with disk persistence
pub struct ModelCapabilitiesCache {
    cache: RwLock<HashMap<String, ModelCapabilities>>,
    cache_dir: PathBuf,
}

impl ModelCapabilitiesCache {
    /// Create a new cache with the given directory.
    pub fn new(cache_dir: PathBuf) -> Self {
        let mc = Self {
            cache: RwLock::new(HashMap::new()),
            cache_dir,
        };
        let _ = mc.load_from_disk();
        mc
    }

    /// Create a cache using the default Claude cache directory.
    pub fn new_default() -> Self {
        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let cache_dir = home_dir.join(".claude").join("cache");
        Self::new(cache_dir)
    }

    /// Get capabilities for the given model.
    /// Checks in order: API cache > disk cache > built-in defaults > 200K fallback.
    pub fn get_model_capabilities(&self, model: &str) -> ModelCapabilities {
        // Check in-memory cache
        if let Some(caps) = self.cache.read().unwrap().get(model) {
            return caps.clone();
        }

        // Check built-in defaults
        if let Some(caps) = DEFAULT_MODEL_CAPABILITIES.get(model) {
            return caps.clone();
        }

        // 200K fallback
        ModelCapabilities {
            context_window: 200_000,
            max_output_tokens: 8192,
            max_thinking_tokens: 4096,
            supports_vision: true,
            supports_thinking: false,
        }
    }

    /// Update capabilities for a model.
    pub fn update_capabilities(&self, model: &str, caps: ModelCapabilities) {
        self.cache
            .write()
            .unwrap()
            .insert(model.to_string(), caps);
        let _ = self.save_to_disk();
    }

    /// Load cache from disk.
    fn load_from_disk(&self) -> std::io::Result<()> {
        let path = self.cache_dir.join("model_capabilities.json");
        if path.exists() {
            let data = fs::read_to_string(&path)?;
            let cached: HashMap<String, ModelCapabilities> = serde_json::from_str(&data)?;
            let mut cache = self.cache.write().unwrap();
            for (k, v) in cached {
                cache.insert(k, v);
            }
        }
        Ok(())
    }

    /// Save cache to disk.
    fn save_to_disk(&self) -> std::io::Result<()> {
        let _ = fs::create_dir_all(&self.cache_dir);
        let path = self.cache_dir.join("model_capabilities.json");
        let cache = self.cache.read().unwrap();
        let data = serde_json::to_string_pretty(&*cache)?;
        fs::write(path, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_capabilities_exist() {
        assert!(DEFAULT_MODEL_CAPABILITIES.contains_key("claude-sonnet-4-20250514"));
        assert!(DEFAULT_MODEL_CAPABILITIES.contains_key("claude-opus-4-5-20250610"));
    }

    #[test]
    fn test_get_capabilities_known_model() {
        let cache = ModelCapabilitiesCache::new(PathBuf::from("/tmp/test_cache"));
        let caps = cache.get_model_capabilities("claude-sonnet-4-20250514");
        assert_eq!(caps.context_window, 1_000_000);
        assert!(caps.supports_vision);
    }

    #[test]
    fn test_get_capabilities_unknown_model() {
        let cache = ModelCapabilitiesCache::new(PathBuf::from("/tmp/test_cache"));
        let caps = cache.get_model_capabilities("unknown-model");
        assert_eq!(caps.context_window, 200_000);
    }

    #[test]
    fn test_update_capabilities() {
        let cache = ModelCapabilitiesCache::new(PathBuf::from("/tmp/test_cache"));
        let new_caps = ModelCapabilities {
            context_window: 500_000,
            max_output_tokens: 16000,
            max_thinking_tokens: 8000,
            supports_vision: true,
            supports_thinking: true,
        };
        cache.update_capabilities("test-model", new_caps.clone());
        let retrieved = cache.get_model_capabilities("test-model");
        assert_eq!(retrieved.context_window, 500_000);
    }
}
