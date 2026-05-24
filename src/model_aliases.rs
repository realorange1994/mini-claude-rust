//! Model alias resolution and default model management.
//!
//! Supports alias-to-model resolution, [1m] context window suffix,
//! legacy model remapping, and default model getters.

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;
use std::sync::RwLock;

/// Model family type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    Opus,
    Sonnet,
    Haiku,
}

/// Alias-to-family mapping
static MODEL_ALIAS_FAMILY: Lazy<HashMap<&'static str, ModelFamily>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("opus", ModelFamily::Opus);
    m.insert("sonnet", ModelFamily::Sonnet);
    m.insert("haiku", ModelFamily::Haiku);
    m.insert("best", ModelFamily::Opus); // best = current Opus
    m.insert("fast", ModelFamily::Sonnet);
    m
});

/// Legacy model remapping for backward compatibility
static LEGACY_MODEL_REMAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("claude-3-opus-20240229", "claude-opus-4-5-20250610");
    m.insert("claude-3-5-sonnet-20240620", "claude-sonnet-4-20250514");
    m.insert("claude-3-5-sonnet-20241022", "claude-sonnet-4-20250514");
    m
});

/// Default models (can be overridden at runtime)
static DEFAULT_MODELS: Lazy<RwLock<DefaultModels>> = Lazy::new(|| {
    RwLock::new(DefaultModels {
        opus: "claude-opus-4-5-20250610".to_string(),
        sonnet: "claude-sonnet-4-20250514".to_string(),
        haiku: "claude-haiku-4-5-20250610".to_string(),
    })
});

struct DefaultModels {
    opus: String,
    sonnet: String,
    haiku: String,
}

/// Check if the model string has the [1m] suffix (case-insensitive).
pub fn has_1m_context(model: &str) -> bool {
    Regex::new(r"(?i)\[1m\]$")
        .map(|re| re.is_match(model))
        .unwrap_or(false)
}

/// Check if a model ID (without [1m] suffix) supports the 1M context window feature.
pub fn model_supports_1m(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    lower.contains("claude-opus-4")
        || lower.contains("claude-sonnet-4")
        || lower.contains("claude-3-7-sonnet")
}

/// Resolve a user-specified model string to an actual model ID.
/// Returns (resolved_model, was_alias).
pub fn resolve_model_alias(model: &str) -> (String, bool) {
    let model = model.trim();
    let normalized = model.to_lowercase();

    // Extract [1m] suffix before resolution
    let has_1m = has_1m_context(&normalized);
    let re = Regex::new(r"(?i)\[1m\]$").unwrap();
    let model_str = re.replace_all(&normalized, "").trim().to_string();

    // Check if it's a known alias
    if let Some(family) = MODEL_ALIAS_FAMILY.get(model_str.as_str()) {
        let resolved = match family {
            ModelFamily::Opus => get_default_opus_model(),
            ModelFamily::Sonnet => get_default_sonnet_model(),
            ModelFamily::Haiku => get_default_haiku_model(),
        };
        // Re-append [1m] only if the family supports it
        let resolved = if has_1m && model_supports_1m(&resolved) {
            format!("{}[1m]", resolved)
        } else {
            resolved
        };
        return (resolved, true);
    }

    // Check legacy Opus model IDs
    if is_legacy_opus_id(&model_str) {
        let resolved = get_default_opus_model();
        let resolved = if has_1m && model_supports_1m(&resolved) {
            format!("{}[1m]", resolved)
        } else {
            resolved
        };
        return (resolved, true);
    }

    // Check backward-compatible legacy aliases
    if let Some(resolved) = LEGACY_MODEL_REMAP.get(model_str.as_str()) {
        let resolved = resolved.to_string();
        let resolved = if has_1m && model_supports_1m(&resolved) {
            format!("{}[1m]", resolved)
        } else {
            resolved
        };
        return (resolved, true);
    }

    // Not an alias — return as-is, preserving [1m] suffix if present
    if has_1m {
        let stripped = re.replace_all(model, "").trim().to_string();
        return (format!("{}[1m]", stripped), false);
    }
    (model.to_string(), false)
}

/// Check if the model string is a legacy Opus 4.0 or 4.1 ID.
fn is_legacy_opus_id(model_str: &str) -> bool {
    let lower = model_str.to_lowercase();
    lower.starts_with("claude-opus-4-0-")
        || lower.starts_with("claude-opus-4-1-")
        || lower.starts_with("claude-opus-4.0-")
        || lower.starts_with("claude-opus-4.1-")
}

/// Get the current default Opus model.
pub fn get_default_opus_model() -> String {
    DEFAULT_MODELS.read().unwrap().opus.clone()
}

/// Get the current default Sonnet model.
pub fn get_default_sonnet_model() -> String {
    DEFAULT_MODELS.read().unwrap().sonnet.clone()
}

/// Get the current default Haiku model.
pub fn get_default_haiku_model() -> String {
    DEFAULT_MODELS.read().unwrap().haiku.clone()
}

/// Set the default Opus model at runtime.
pub fn set_default_opus_model(model: &str) {
    DEFAULT_MODELS.write().unwrap().opus = model.to_string();
}

/// Set the default Sonnet model at runtime.
pub fn set_default_sonnet_model(model: &str) {
    DEFAULT_MODELS.write().unwrap().sonnet = model.to_string();
}

/// Set the default Haiku model at runtime.
pub fn set_default_haiku_model(model: &str) {
    DEFAULT_MODELS.write().unwrap().haiku = model.to_string();
}

/// Get the default model based on subscription type.
pub fn get_default_model(subscription_type: &str) -> String {
    match subscription_type {
        "enterprise" => get_default_opus_model(),
        "claude_ai" | "api" => get_default_sonnet_model(),
        _ => get_default_sonnet_model(),
    }
}

/// Extract the canonical model family from a full model ID.
/// E.g., "claude-opus-4-5-20250610" → "claude-opus-4-5"
pub fn extract_canonical_model_name(model_id: &str) -> String {
    let name = model_id.to_lowercase();
    let re = Regex::new(r"(?i)\[1m\]$").unwrap();
    let name = re.replace_all(&name, "").to_string();

    let canonical_mappings = [
        ("claude-opus-4-7", "claude-opus-4-7"),
        ("claude-opus-4-6", "claude-opus-4-6"),
        ("claude-opus-4-5", "claude-opus-4-5"),
        ("claude-opus-4-1", "claude-opus-4-1"),
        ("claude-opus-4", "claude-opus-4"),
        ("claude-sonnet-4-6", "claude-sonnet-4-6"),
        ("claude-sonnet-4-5", "claude-sonnet-4-5"),
        ("claude-sonnet-4", "claude-sonnet-4"),
        ("claude-haiku-4-5", "claude-haiku-4-5"),
        ("claude-3-7-sonnet", "claude-3-7-sonnet"),
        ("claude-3-5-sonnet", "claude-3-5-sonnet"),
        ("claude-3-5-haiku", "claude-3-5-haiku"),
        ("claude-3-opus", "claude-3-opus"),
    ];

    for (pattern, canonical) in &canonical_mappings {
        if name.contains(pattern) {
            return canonical.to_string();
        }
    }
    name
}

/// Get the context window size for a model string.
pub fn get_context_window_for_model(model: &str) -> i64 {
    let normalized = model.to_lowercase().trim().to_string();

    // [1m] suffix — explicit client-side opt-in
    if has_1m_context(&normalized) {
        return 1_000_000;
    }

    // Default fallback
    200_000
}

/// Format token count for display
pub fn format_token_count(count: i64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.0}K", count as f64 / 1_000.0)
    } else {
        format!("{}", count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_1m_context() {
        assert!(has_1m_context("sonnet[1m]"));
        assert!(has_1m_context("opus[1M]"));
        assert!(!has_1m_context("sonnet"));
    }

    #[test]
    fn test_model_supports_1m() {
        assert!(model_supports_1m("claude-opus-4-5-20250610"));
        assert!(model_supports_1m("claude-sonnet-4-20250514"));
        assert!(!model_supports_1m("claude-3-opus"));
    }

    #[test]
    fn test_resolve_alias_sonnet() {
        let (resolved, was_alias) = resolve_model_alias("sonnet");
        assert!(was_alias);
        assert!(resolved.contains("sonnet"));
    }

    #[test]
    fn test_resolve_alias_opus() {
        let (resolved, was_alias) = resolve_model_alias("opus");
        assert!(was_alias);
        assert!(resolved.contains("opus"));
    }

    #[test]
    fn test_resolve_alias_with_1m() {
        let (resolved, was_alias) = resolve_model_alias("sonnet[1m]");
        assert!(was_alias);
        assert!(resolved.contains("[1m]"));
    }

    #[test]
    fn test_resolve_non_alias() {
        let (resolved, was_alias) = resolve_model_alias("claude-3-opus-20240229");
        assert!(!was_alias);
    }

    #[test]
    fn test_extract_canonical() {
        assert_eq!(
            extract_canonical_model_name("claude-opus-4-5-20250610"),
            "claude-opus-4-5"
        );
        assert_eq!(
            extract_canonical_model_name("claude-sonnet-4-20250514"),
            "claude-sonnet-4"
        );
    }

    #[test]
    fn test_context_window() {
        assert_eq!(get_context_window_for_model("sonnet[1m]"), 1_000_000);
        assert_eq!(get_context_window_for_model("sonnet"), 200_000);
    }

    #[test]
    fn test_format_token_count() {
        assert_eq!(format_token_count(1_000_000), "1.0M");
        assert_eq!(format_token_count(200_000), "200K");
        assert_eq!(format_token_count(500), "500");
    }

    #[test]
    fn test_is_legacy_opus() {
        assert!(is_legacy_opus_id("claude-opus-4-0-20250101"));
        assert!(is_legacy_opus_id("claude-opus-4-1-20250101"));
        assert!(!is_legacy_opus_id("claude-opus-4-5-20250610"));
    }
}
