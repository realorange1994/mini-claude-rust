//! Error classification system for structured error handling.
//!
//! Replaces string-matching error detection with typed error variants,
//! enabling precise retry logic, logging, and recovery strategies.
//!
//! 15-category taxonomy matching the Go version's classifyError with recovery hints.

use std::time::Duration;

/// Top-level error classification for the agent loop (15 categories).
#[derive(Debug, Clone, PartialEq)]
pub enum AgentError {
    /// Transient error that may succeed on retry (network, 5xx)
    Transient { source: TransientError },
    /// Non-retryable generic error
    NonRetryable { message: String },
    /// Context window exceeded -- compress before retry
    ContextOverflow {
        current_tokens: usize,
        max_tokens: usize,
    },
    /// Orphaned tool_call/result pairs detected (API error 2013)
    ToolPairing { orphaned_ids: Vec<String> },
    /// Rate limited (429) -- backoff + retry
    RateLimit { retry_after: Duration },
    /// Billing failure (402 / credit exhausted) -- rotate key or fallback
    Billing { message: String },
    /// Model not found -- fallback to different model
    ModelNotFound { model: String },
    /// Payload too large (413) -- compress context before retry
    PayloadTooLarge { message: String },
    /// Provider overloaded (503/529)
    Overloaded { message: String },
    /// Request or connection timeout
    Timeout { source: TransientError },
    /// Bad request / format error (400)
    FormatError { message: String },
    /// Authentication failure (401/403)
    Auth { message: String },
    /// Thinking block signature invalid
    ThinkingSig { message: String },
    /// Long context tier rate limit (429 + "extra usage" / "long context")
    LongContextTier { retry_after: Duration },
    /// Unclassifiable -- retry with backoff
    Unknown { message: String },
    /// Model hit max output tokens (incomplete response)
    MaxOutputTokens { partial_text: String },
    /// Model appears confused (repeating patterns, empty responses)
    ModelConfusion { detected_patterns: Vec<String> },
    /// Fatal non-recoverable error
    Fatal { message: String },
}

/// Transient (retryable) error subtypes.
#[derive(Debug, Clone, PartialEq)]
pub enum TransientError {
    ConnectionLost,
    Timeout,
    ServerError(u16),
    StreamStall,
    Transient,
}

/// Recovery hints from error classification.
#[derive(Debug, Clone, Default)]
pub struct RecoveryHints {
    /// Should compress context before retry
    pub compress: bool,
    /// Should rotate API key
    pub rotate_key: bool,
    /// Should fallback to different provider/model
    pub fallback: bool,
}

/// Classify result combining the error type with recovery hints.
#[derive(Debug, Clone)]
pub struct ClassifyResult {
    pub error: AgentError,
    pub retryable: bool,
    pub hints: RecoveryHints,
    pub status_code: u16,
}

impl AgentError {
    /// Whether this error is worth retrying.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AgentError::Transient { .. }
                | AgentError::RateLimit { .. }
                | AgentError::ContextOverflow { .. }
                | AgentError::Overloaded { .. }
                | AgentError::Timeout { .. }
                | AgentError::LongContextTier { .. }
        )
    }

    /// Suggested delay before retry, if applicable.
    pub fn retry_delay(&self) -> Option<Duration> {
        match self {
            AgentError::Transient { source } => {
                Some(match source {
                    TransientError::Timeout => Duration::from_secs(5),
                    _ => Duration::from_secs(2),
                })
            }
            AgentError::RateLimit { retry_after } => Some(*retry_after),
            AgentError::LongContextTier { retry_after } => Some(*retry_after),
            AgentError::ContextOverflow { .. } => Some(Duration::ZERO),
            AgentError::Overloaded { .. } => Some(Duration::from_secs(10)),
            _ => None,
        }
    }

    /// Human-readable error category for logging.
    pub fn category(&self) -> &'static str {
        match self {
            AgentError::Transient { .. } => "transient",
            AgentError::NonRetryable { .. } => "non_retryable",
            AgentError::ContextOverflow { .. } => "context_overflow",
            AgentError::ToolPairing { .. } => "tool_pairing",
            AgentError::RateLimit { .. } => "rate_limit",
            AgentError::Billing { .. } => "billing",
            AgentError::ModelNotFound { .. } => "model_not_found",
            AgentError::PayloadTooLarge { .. } => "payload_too_large",
            AgentError::Overloaded { .. } => "overloaded",
            AgentError::Timeout { .. } => "timeout",
            AgentError::FormatError { .. } => "format_error",
            AgentError::Auth { .. } => "auth",
            AgentError::ThinkingSig { .. } => "thinking_signature",
            AgentError::LongContextTier { .. } => "long_context_tier",
            AgentError::Unknown { .. } => "unknown",
            AgentError::MaxOutputTokens { .. } => "max_output_tokens",
            AgentError::ModelConfusion { .. } => "model_confusion",
            AgentError::Fatal { .. } => "fatal",
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Transient { source } => write!(f, "Transient error: {:?}", source),
            AgentError::NonRetryable { message } => write!(f, "Non-retryable: {}", message),
            AgentError::ContextOverflow { current_tokens, max_tokens } => {
                write!(f, "Context overflow: {} / {} tokens", current_tokens, max_tokens)
            }
            AgentError::ToolPairing { orphaned_ids } => {
                write!(f, "Orphaned tool pairs: {:?}", orphaned_ids)
            }
            AgentError::RateLimit { retry_after } => {
                write!(f, "Rate limited (retry after {:?})", retry_after)
            }
            AgentError::Billing { message } => write!(f, "Billing error: {}", message),
            AgentError::ModelNotFound { model } => write!(f, "Model not found: {}", model),
            AgentError::PayloadTooLarge { message } => write!(f, "Payload too large: {}", message),
            AgentError::Overloaded { message } => write!(f, "Overloaded: {}", message),
            AgentError::Timeout { source } => write!(f, "Timeout: {:?}", source),
            AgentError::FormatError { message } => write!(f, "Format error: {}", message),
            AgentError::Auth { message } => write!(f, "Auth error: {}", message),
            AgentError::ThinkingSig { message } => write!(f, "Thinking signature: {}", message),
            AgentError::LongContextTier { retry_after } => {
                write!(f, "Long context tier (retry after {:?})", retry_after)
            }
            AgentError::Unknown { message } => write!(f, "Unknown error: {}", message),
            AgentError::MaxOutputTokens { partial_text } => {
                write!(f, "Max output tokens reached ({} chars)", partial_text.len())
            }
            AgentError::ModelConfusion { detected_patterns } => {
                write!(f, "Model confusion: {:?}", detected_patterns)
            }
            AgentError::Fatal { message } => write!(f, "Fatal: {}", message),
        }
    }
}

impl std::error::Error for AgentError {}

// ─── Pattern arrays (matching Go's error patterns) ───

const BILLING_PATTERNS: &[&str] = &[
    "insufficient credits", "insufficient_quota", "credit balance",
    "credits have been exhausted", "top up your credits",
    "payment required", "billing hard limit",
    "exceeded your current quota", "account is deactivated",
    "plan does not include",
];

const RATE_LIMIT_PATTERNS: &[&str] = &[
    "rate limit", "rate_limit", "too many requests", "throttled",
    "requests per minute", "tokens per minute", "requests per day",
    "try again in", "please retry after", "resource_exhausted",
    "rate increased too quickly", "throttlingexception",
    "too many concurrent requests", "servicequotaexceededexception",
];

const USAGE_LIMIT_PATTERNS: &[&str] = &[
    "usage limit", "quota", "limit exceeded", "key limit exceeded",
];

const USAGE_LIMIT_TRANSIENT_SIGNALS: &[&str] = &[
    "try again", "retry", "resets at", "reset in", "wait",
    "requests remaining", "periodic", "window",
];

const CONTEXT_OVERFLOW_PATTERNS: &[&str] = &[
    "context_length", "maximum context", "too many tokens",
    "prompt_too_long", "token limit", "context_exceeded",
    "max_tokens_exceeded", "context window", "context limit",
    "prompt exceeds max length", "prompt is too long",
    "exceeds the limit", "reduce the length", "context size",
    "exceeds the max_model_len", "max_model_len",
    "engine prompt length", "input is too long",
    "maximum model length", "context length exceeded",
    "truncating input", "slot context", "n_ctx_slot",
    "max input token", "input token",
    "exceeds the maximum number of input tokens",
];

const MODEL_NOT_FOUND_PATTERNS: &[&str] = &[
    "is not a valid model", "invalid model", "model not found",
    "model_not_found", "does not exist", "no such model",
    "unknown model", "unsupported model",
];

const AUTH_PATTERNS: &[&str] = &[
    "invalid api key", "invalid_api_key", "authentication",
    "unauthorized", "forbidden", "invalid token", "token expired",
    "token revoked", "access denied",
];

const SERVER_DISCONNECT_PATTERNS: &[&str] = &[
    "server disconnected", "peer closed connection",
    "connection reset by peer", "connection was closed",
    "network connection lost", "unexpected eof",
    "incomplete chunked read",
];

const NETWORK_ERROR_PATTERNS: &[&str] = &[
    "connection refused", "connection reset", "connection timed out",
    "connection error", "connection lost", "no such host",
    "temporary failure", "dns error", "network error",
    "network is unreachable", "network unreachable", "host unreachable",
    "socket error", "tcp error", "broken pipe",
];

const SERVER_ERROR_PATTERNS: &[&str] = &[
    "internal server error", "bad gateway",
    "service unavailable", "gateway timeout",
];

const TRANSPORT_ERROR_TYPES: &[&str] = &[
    "readtimeout", "connecttimeout", "pooltimeout",
    "connecterror", "remoteprotocolerror",
    "connectionerror", "connectionreseterror",
    "connectionabortederror", "brokenpipeerror",
    "timeouterror", "readerror",
    "serverdisconnectederror",
];

// ─── Core classification function ───

/// Classify an error message into a ClassifyResult with recovery hints.
///
/// Priority-ordered pipeline matching Go's classifyError:
/// 1. Status code classification
/// 2. Error code classification (from body)
/// 3. Message pattern matching
/// 4. Server disconnect + large session heuristic
/// 5. Transport error heuristics
/// 6. Fallback: unknown
pub fn classify_error(err_msg: &str, approx_tokens: usize, context_length: usize) -> ClassifyResult {
    let lower = err_msg.to_lowercase();
    let status_code = extract_http_status(err_msg);

    let result = |error: AgentError| -> ClassifyResult {
        ClassifyResult {
            retryable: error.is_retryable(),
            hints: RecoveryHints::default(),
            error,
            status_code,
        }
    };

    let with_hints = |error: AgentError, hints: RecoveryHints| -> ClassifyResult {
        ClassifyResult {
            retryable: error.is_retryable(),
            hints,
            error,
            status_code,
        }
    };

    // ── Status code classification ──

    if status_code == 401 {
        return with_hints(
            AgentError::Auth { message: err_msg.to_string() },
            RecoveryHints { fallback: true, rotate_key: true, ..Default::default() },
        );
    }

    if status_code == 403 {
        if lower.contains("key limit exceeded") || lower.contains("spending limit") {
            return with_hints(
                AgentError::Billing { message: err_msg.to_string() },
                RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
            );
        }
        return with_hints(
            AgentError::Auth { message: err_msg.to_string() },
            RecoveryHints { fallback: true, ..Default::default() },
        );
    }

    if status_code == 402 {
        return classify_402(&lower, err_msg, status_code);
    }

    if status_code == 404 {
        if matches_any(&lower, MODEL_NOT_FOUND_PATTERNS) {
            return with_hints(
                AgentError::ModelNotFound { model: err_msg.to_string() },
                RecoveryHints { fallback: true, ..Default::default() },
            );
        }
        return result(AgentError::Unknown { message: err_msg.to_string() });
    }

    if status_code == 413 {
        return with_hints(
            AgentError::PayloadTooLarge { message: err_msg.to_string() },
            RecoveryHints { compress: true, ..Default::default() },
        );
    }

    if status_code == 429 {
        let retry_after = extract_retry_after(err_msg).unwrap_or(Duration::from_secs(10));
        if lower.contains("long context") || lower.contains("extra usage") {
            return with_hints(
                AgentError::LongContextTier { retry_after },
                RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
            );
        }
        return with_hints(
            AgentError::RateLimit { retry_after },
            RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        );
    }

    if status_code == 400 {
        return classify_400(&lower, err_msg, approx_tokens, context_length, status_code);
    }

    if status_code == 500 || status_code == 502 {
        return result(AgentError::Transient { source: TransientError::ServerError(status_code) });
    }

    if status_code == 503 || status_code == 529 {
        return result(AgentError::Overloaded { message: err_msg.to_string() });
    }

    if status_code >= 400 && status_code < 500 {
        return with_hints(
            AgentError::FormatError { message: err_msg.to_string() },
            RecoveryHints { fallback: true, ..Default::default() },
        );
    }

    if status_code >= 500 && status_code < 600 {
        return result(AgentError::Transient { source: TransientError::ServerError(status_code) });
    }

    // ── Message pattern matching (no status code) ──

    // Context overflow -- not retryable without compression
    if matches_any(&lower, CONTEXT_OVERFLOW_PATTERNS) {
        return with_hints(
            AgentError::ContextOverflow { current_tokens: approx_tokens, max_tokens: context_length },
            RecoveryHints { compress: true, ..Default::default() },
        );
    }

    // Tool pairing
    if lower.contains("2013") || lower.contains("tool call result does not follow tool call") {
        return result(AgentError::ToolPairing { orphaned_ids: vec![] });
    }

    // Billing
    if matches_any(&lower, BILLING_PATTERNS) {
        return with_hints(
            AgentError::Billing { message: err_msg.to_string() },
            RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        );
    }

    // Rate limit (before usage limit to avoid misclassification)
    if matches_any(&lower, RATE_LIMIT_PATTERNS) {
        return with_hints(
            AgentError::RateLimit { retry_after: extract_retry_after(err_msg).unwrap_or(Duration::from_secs(10)) },
            RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        );
    }

    // Usage limit with disambiguation
    if matches_any(&lower, USAGE_LIMIT_PATTERNS) {
        if matches_any(&lower, USAGE_LIMIT_TRANSIENT_SIGNALS) {
            return with_hints(
                AgentError::RateLimit { retry_after: extract_retry_after(err_msg).unwrap_or(Duration::from_secs(10)) },
                RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
            );
        }
        return with_hints(
            AgentError::Billing { message: err_msg.to_string() },
            RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        );
    }

    // Model not found
    if matches_any(&lower, MODEL_NOT_FOUND_PATTERNS) {
        return with_hints(
            AgentError::ModelNotFound { model: err_msg.to_string() },
            RecoveryHints { fallback: true, ..Default::default() },
        );
    }

    // Auth patterns
    if matches_any(&lower, AUTH_PATTERNS) {
        return with_hints(
            AgentError::Auth { message: err_msg.to_string() },
            RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        );
    }

    // Thinking signature
    if lower.contains("thinking") && (lower.contains("signature") || lower.contains("block")) {
        return result(AgentError::ThinkingSig { message: err_msg.to_string() });
    }

    // Server disconnect + large session → context overflow heuristic
    if matches_any(&lower, SERVER_DISCONNECT_PATTERNS) && status_code == 0 {
        let is_large = approx_tokens > 0 && (approx_tokens > context_length * 6 / 10 || approx_tokens > 120_000);
        if is_large {
            return with_hints(
                AgentError::ContextOverflow { current_tokens: approx_tokens, max_tokens: context_length },
                RecoveryHints { compress: true, ..Default::default() },
            );
        }
        return result(AgentError::Timeout { source: TransientError::Timeout });
    }

    // Network errors -- retryable (check AFTER timeout to avoid "connection timed out" misclass)
    if matches_any(&lower, NETWORK_ERROR_PATTERNS) && !lower.contains("timeout") && !lower.contains("timed out") {
        return result(AgentError::Transient { source: TransientError::ConnectionLost });
    }

    // Server errors without status code -- retryable
    if matches_any(&lower, SERVER_ERROR_PATTERNS) {
        return result(AgentError::Transient { source: TransientError::ServerError(0) });
    }

    // Transport error heuristics (must come after more specific patterns)
    if lower.contains("timeout") || lower.contains("timed out") || lower.contains("deadline exceeded") {
        return result(AgentError::Timeout { source: TransientError::Timeout });
    }
    if matches_any(&lower, TRANSPORT_ERROR_TYPES) {
        return result(AgentError::Timeout { source: TransientError::Timeout });
    }

    // Transient error keyword
    if lower.contains("transient") {
        return result(AgentError::Transient { source: TransientError::Transient });
    }

    // ── Fallback ──
    with_hints(
        AgentError::Unknown { message: err_msg.to_string() },
        RecoveryHints { ..Default::default() },
    )
}

/// Classify 402 (Payment Required) errors -- billing vs transient usage limit.
fn classify_402(lower: &str, err_msg: &str, status_code: u16) -> ClassifyResult {
    if matches_any(lower, USAGE_LIMIT_PATTERNS) && matches_any(lower, USAGE_LIMIT_TRANSIENT_SIGNALS) {
        return ClassifyResult {
            error: AgentError::RateLimit {
                retry_after: extract_retry_after(err_msg).unwrap_or(Duration::from_secs(10)),
            },
            retryable: true,
            hints: RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
            status_code,
        };
    }
    ClassifyResult {
        error: AgentError::Billing { message: err_msg.to_string() },
        retryable: false,
        hints: RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        status_code,
    }
}

/// Classify 400 (Bad Request) errors -- context overflow, model not found, or format error.
fn classify_400(
    lower: &str, err_msg: &str,
    approx_tokens: usize, context_length: usize,
    status_code: u16,
) -> ClassifyResult {
    let make = |error: AgentError, hints: RecoveryHints| -> ClassifyResult {
        ClassifyResult { retryable: error.is_retryable(), hints, error, status_code }
    };

    if matches_any(lower, CONTEXT_OVERFLOW_PATTERNS) {
        return make(
            AgentError::ContextOverflow { current_tokens: approx_tokens, max_tokens: context_length },
            RecoveryHints { compress: true, ..Default::default() },
        );
    }

    if matches_any(lower, MODEL_NOT_FOUND_PATTERNS) {
        return make(
            AgentError::ModelNotFound { model: err_msg.to_string() },
            RecoveryHints { fallback: true, ..Default::default() },
        );
    }

    if matches_any(lower, RATE_LIMIT_PATTERNS) {
        return make(
            AgentError::RateLimit { retry_after: extract_retry_after(err_msg).unwrap_or(Duration::from_secs(10)) },
            RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        );
    }

    if matches_any(lower, BILLING_PATTERNS) {
        return make(
            AgentError::Billing { message: err_msg.to_string() },
            RecoveryHints { rotate_key: true, fallback: true, ..Default::default() },
        );
    }

    // Generic 400 + large session → probable context overflow
    let is_large = approx_tokens > 0 && (approx_tokens > context_length * 4 / 10 || approx_tokens > 80_000);
    if is_large {
        return make(
            AgentError::ContextOverflow { current_tokens: approx_tokens, max_tokens: context_length },
            RecoveryHints { compress: true, ..Default::default() },
        );
    }

    make(
        AgentError::FormatError { message: err_msg.to_string() },
        RecoveryHints { fallback: true, ..Default::default() },
    )
}

// ─── Utility functions ───

fn matches_any(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| text.contains(p))
}

fn extract_http_status(err_msg: &str) -> u16 {
    use regex::Regex;
    use std::sync::OnceLock;

    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:(?:status|HTTP|http)\s*[:=]?\s*|^|\s)(\d{3})\b").ok()
    });
    let re = match re {
        Some(r) => r,
        None => return 0,
    };
    if let Some(caps) = re.captures(err_msg) {
        if let Ok(code) = caps[1].parse::<u16>() {
            if (400..600).contains(&code) {
                return code;
            }
        }
    }
    0
}

fn extract_retry_after(err_msg: &str) -> Option<Duration> {
    let lower = err_msg.to_lowercase();
    if let Some(pos) = lower.find("retry-after") {
        let after = &lower[pos + 11..];
        let after = after.trim_start_matches(|c: char| c == ':' || c == ' ');
        if let Some(num_str) = after.split(|c: char| !c.is_ascii_digit()).next() {
            if let Ok(secs) = num_str.parse::<u64>() {
                if secs > 0 {
                    return Some(Duration::from_secs(secs));
                }
            }
        }
    }
    None
}

/// Backward-compatible wrapper: check if error is transient.
pub fn is_transient_error(err_msg: &str) -> bool {
    classify_error(err_msg, 0, 0).retryable
}

/// Check if the error is a context window overflow.
pub fn is_context_length_error(err_msg: &str) -> bool {
    let lower = err_msg.to_lowercase();
    matches_any(&lower, CONTEXT_OVERFLOW_PATTERNS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_auth_401() {
        let r = classify_error("401 Unauthorized", 0, 0);
        assert!(!r.retryable);
        assert_eq!(r.error.category(), "auth");
        assert!(r.hints.fallback);
        assert!(r.hints.rotate_key);
    }

    #[test]
    fn test_classify_billing_402() {
        let r = classify_error("402 insufficient credits", 0, 0);
        assert!(!r.retryable);
        assert_eq!(r.error.category(), "billing");
        assert!(r.hints.rotate_key);
    }

    #[test]
    fn test_classify_rate_limit_429() {
        let r = classify_error("429 Too Many Requests", 0, 0);
        assert!(r.retryable);
        assert_eq!(r.error.category(), "rate_limit");
        assert!(r.hints.rotate_key);
        assert!(r.hints.fallback);
    }

    #[test]
    fn test_classify_context_overflow_pattern() {
        let r = classify_error("prompt is too long: context_length_exceeded", 150_000, 200_000);
        assert_eq!(r.error.category(), "context_overflow");
        assert!(r.hints.compress);
        assert!(r.retryable); // retryable -- compress and retry
    }

    #[test]
    fn test_classify_context_overflow_400_large_session() {
        let r = classify_error("400 Bad Request: something went wrong", 90_000, 200_000);
        assert_eq!(r.error.category(), "context_overflow");
        assert!(r.hints.compress);
    }

    #[test]
    fn test_classify_model_not_found() {
        let r = classify_error("404 model not found: gpt-99", 0, 0);
        assert_eq!(r.error.category(), "model_not_found");
        assert!(!r.retryable);
        assert!(r.hints.fallback);
    }

    #[test]
    fn test_classify_payload_too_large_413() {
        let r = classify_error("413 Payload Too Large", 0, 0);
        assert_eq!(r.error.category(), "payload_too_large");
        assert!(r.hints.compress);
    }

    #[test]
    fn test_classify_overloaded_503() {
        let r = classify_error("503 Service Unavailable", 0, 0);
        assert_eq!(r.error.category(), "overloaded");
        assert!(r.retryable);
    }

    #[test]
    fn test_classify_timeout_pattern() {
        let r = classify_error("connection timed out after 30s", 0, 0);
        assert_eq!(r.error.category(), "timeout");
        assert!(r.retryable);
    }

    #[test]
    fn test_classify_network_error() {
        let r = classify_error("connection refused", 0, 0);
        assert_eq!(r.error.category(), "transient");
        assert!(r.retryable);

        // "connection reset by peer" matches server_disconnect patterns → timeout
        let r2 = classify_error("connection reset by peer", 0, 0);
        assert_eq!(r2.error.category(), "timeout");
        assert!(r2.retryable);
    }

    #[test]
    fn test_classify_server_disconnect_large_session() {
        let r = classify_error("server disconnected unexpectedly", 130_000, 200_000);
        assert_eq!(r.error.category(), "context_overflow");
        assert!(r.hints.compress);
    }

    #[test]
    fn test_classify_server_disconnect_small_session() {
        let r = classify_error("server disconnected unexpectedly", 1000, 200_000);
        assert_eq!(r.error.category(), "timeout");
        assert!(r.retryable);
    }

    #[test]
    fn test_classify_unknown_fallback() {
        let r = classify_error("something completely unexpected", 0, 0);
        assert_eq!(r.error.category(), "unknown");
        assert!(!r.retryable);
    }

    #[test]
    fn test_backward_compat_is_transient() {
        assert!(is_transient_error("connection reset by peer"));
        assert!(is_transient_error("503 Service Unavailable"));
        assert!(is_transient_error("429 rate limit"));
        assert!(!is_transient_error("Invalid API key"));
    }

    #[test]
    fn test_is_context_length_error() {
        assert!(is_context_length_error("context_length_exceeded"));
        assert!(is_context_length_error("prompt is too long"));
        assert!(!is_context_length_error("normal error"));
    }

    #[test]
    fn test_retry_delay() {
        let r = classify_error("connection reset", 0, 0);
        assert!(r.error.retry_delay().is_some());

        let r = classify_error("429 rate limit", 0, 0);
        assert!(r.error.retry_delay().is_some());
        assert!(r.error.retry_delay().unwrap() > Duration::from_secs(0));
    }

    #[test]
    fn test_classify_billing_patterns() {
        assert_eq!(classify_error("insufficient credits", 0, 0).error.category(), "billing");
        assert_eq!(classify_error("credit balance too low", 0, 0).error.category(), "billing");
        assert_eq!(classify_error("payment required", 0, 0).error.category(), "billing");
    }

    #[test]
    fn test_classify_usage_limit_transient() {
        // "usage limit" + "try again" → transient rate limit
        let r = classify_error("usage limit exceeded, try again in 60 seconds", 0, 0);
        assert_eq!(r.error.category(), "rate_limit");
        assert!(r.retryable);
    }

    #[test]
    fn test_classify_usage_limit_permanent() {
        // "usage limit" without transient signal → billing
        let r = classify_error("usage limit exceeded permanently", 0, 0);
        assert_eq!(r.error.category(), "billing");
        assert!(!r.retryable);
    }

    #[test]
    fn test_classify_long_context_tier() {
        let r = classify_error("429 rate limit: extra usage for long context", 0, 0);
        assert_eq!(r.error.category(), "long_context_tier");
        assert!(r.retryable);
    }

    #[test]
    fn test_recovery_hints_default() {
        let h = RecoveryHints::default();
        assert!(!h.compress);
        assert!(!h.rotate_key);
        assert!(!h.fallback);
    }

    #[test]
    fn test_display_format() {
        let r = classify_error("401 Unauthorized", 0, 0);
        let display = format!("{}", r.error);
        assert!(display.contains("Auth"));
        assert!(display.contains("401"));
    }
}
