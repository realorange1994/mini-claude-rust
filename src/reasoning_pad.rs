//! Thinking/reasoning mode detection and padding for API requests.
//! Ported from upstream reasoning_pad.go (27 lines).
//!
//! Detects whether the model supports extended thinking (chain-of-thought)
//! and provides padding to meet the minimum token requirement.

/// Models that support extended thinking mode.
const THINKING_CAPABLE_MODELS: &[&str] = &[
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "claude-3-7-sonnet",
    "claude-3-7-sonnet-latest",
];

/// Minimum tokens required for thinking mode to activate.
/// The API requires at least 1024 tokens in the thinking budget.
pub const MIN_THINKING_BUDGET: i64 = 1024;

/// Check if a model supports extended thinking mode.
pub fn supports_thinking(model: &str) -> bool {
    // Normalize model name: strip date suffix like -20250514
    let normalized = normalize_model_name(model);
    THINKING_CAPABLE_MODELS
        .iter()
        .any(|&m| normalized == m || normalized.starts_with(m))
}

/// Normalize a model name by stripping date suffixes.
/// e.g. "claude-sonnet-4-6-20250514" → "claude-sonnet-4-6"
fn normalize_model_name(model: &str) -> &str {
    // Strip common date patterns: -YYYYMMDD or -YYYY-MM-DD
    let parts: Vec<&str> = model.rsplitn(2, '-').collect();
    if parts.len() == 2 {
        // Check if the last part looks like a date (8 digits)
        if parts[0].len() == 8 && parts[0].chars().all(|c| c.is_ascii_digit()) {
            return parts[1];
        }
    }
    model
}

/// Compute the thinking budget for a given max_tokens value.
/// If max_tokens is large enough, allocate the minimum thinking budget.
/// Returns None if thinking is not supported or max_tokens is too small.
pub fn compute_thinking_budget(model: &str, max_tokens: i64) -> Option<i64> {
    if !supports_thinking(model) {
        return None;
    }

    // Need at least MIN_THINKING_BUDGET + some output tokens
    if max_tokens < MIN_THINKING_BUDGET + 256 {
        return None;
    }

    // Allocate a portion of the budget to thinking
    let thinking_budget = (max_tokens * 3 / 4).max(MIN_THINKING_BUDGET);
    Some(thinking_budget)
}

/// Generate thinking-mode padding text if the prompt is too short.
/// The API requires sufficient context for thinking mode to work.
pub fn thinking_padding(current_token_count: i64) -> &'static str {
    if current_token_count < 100 {
        // Very short prompts may not trigger thinking; add a nudge
        "\nPlease think step by step."
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supports_thinking() {
        assert!(supports_thinking("claude-sonnet-4-6"));
        assert!(supports_thinking("claude-sonnet-4-6-20250514"));
        assert!(supports_thinking("claude-opus-4-6"));
        assert!(supports_thinking("claude-3-7-sonnet"));
        assert!(!supports_thinking("claude-3-5-haiku"));
        assert!(!supports_thinking("gpt-4"));
    }

    #[test]
    fn test_compute_thinking_budget() {
        // Too small
        assert_eq!(compute_thinking_budget("claude-sonnet-4-6", 512), None);
        // Barely enough
        let budget = compute_thinking_budget("claude-sonnet-4-6", 4096);
        assert!(budget.is_some());
        assert!(budget.unwrap() >= MIN_THINKING_BUDGET);
        // Not supported model
        assert_eq!(compute_thinking_budget("gpt-4", 8192), None);
    }

    #[test]
    fn test_normalize_model_name() {
        assert_eq!(normalize_model_name("claude-sonnet-4-6-20250514"), "claude-sonnet-4-6");
        assert_eq!(normalize_model_name("claude-sonnet-4-6"), "claude-sonnet-4-6");
        assert_eq!(normalize_model_name("claude-opus-4-6-20250514"), "claude-opus-4-6");
    }

    #[test]
    fn test_thinking_padding() {
        assert!(!thinking_padding(500).is_empty());
        assert!(thinking_padding(200).is_empty());
    }
}
