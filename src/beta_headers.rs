//! Beta headers for API requests.
//!
//! Manages feature flags and beta headers sent to the API.

use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::sync::RwLock;

/// Global beta features registry
static BETA_FEATURES: Lazy<RwLock<HashSet<String>>> = Lazy::new(|| {
    let mut features = HashSet::new();
    // Default beta features
    features.insert("interleaved-thinking-2025-05-14".to_string());
    features.insert("code-execution-2025-05-22".to_string());
    RwLock::new(features)
});

/// Get the current beta header value for API requests.
pub fn get_beta_header() -> String {
    let features = BETA_FEATURES.read().unwrap();
    features
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Enable a beta feature.
pub fn enable_beta_feature(feature: &str) {
    BETA_FEATURES.write().unwrap().insert(feature.to_string());
}

/// Disable a beta feature.
pub fn disable_beta_feature(feature: &str) {
    BETA_FEATURES.write().unwrap().remove(feature);
}

/// Check if a beta feature is enabled.
pub fn is_beta_feature_enabled(feature: &str) -> bool {
    BETA_FEATURES.read().unwrap().contains(feature)
}

/// Reset beta features to defaults.
pub fn reset_beta_features() {
    let mut features = BETA_FEATURES.write().unwrap();
    features.clear();
    features.insert("interleaved-thinking-2025-05-14".to_string());
    features.insert("code-execution-2025-05-22".to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_beta_header_not_empty() {
        let header = get_beta_header();
        assert!(!header.is_empty());
    }

    #[test]
    fn test_enable_disable_feature() {
        enable_beta_feature("test-feature");
        assert!(is_beta_feature_enabled("test-feature"));
        disable_beta_feature("test-feature");
        assert!(!is_beta_feature_enabled("test-feature"));
    }

    #[test]
    fn test_reset_features() {
        enable_beta_feature("custom-feature");
        reset_beta_features();
        assert!(!is_beta_feature_enabled("custom-feature"));
        assert!(is_beta_feature_enabled("interleaved-thinking-2025-05-14"));
    }
}
