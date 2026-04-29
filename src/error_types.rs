//! Error classification system for structured error handling.
//!
//! Replaces string-matching error detection with typed error variants,
//! enabling precise retry logic, logging, and recovery strategies.

use std::time::Duration;

/// Top-level error classification for the agent loop.
#[derive(Debug, Clone)]
pub enum AgentError {
    /// Transient error that may succeed on retry
    Transient {
        source: TransientError,
        retry_after: Option<Duration>,
    },
    /// Context window exceeded
    ContextOverflow {
        current_tokens: usize,
        max_tokens: usize,
    },
    /// Orphaned tool_call/result pairs detected
    ToolPairing {
        orphaned_ids: Vec<String>,
    },
    /// Model hit max output tokens (incomplete response)
    MaxOutputTokens {
        partial_text: String,
    },
    /// Model appears confused (repeating patterns, empty responses)
    ModelConfusion {
        detected_patterns: Vec<String>,
    },
    /// Authentication failure
    Auth {
        message: String,
    },
    /// Rate limited
    RateLimit {
        retry_after: Duration,
    },
    /// Fatal non-recoverable error
    Fatal {
        message: String,
    },
}

/// Transient (retryable) error subtypes.
#[derive(Debug, Clone)]
pub enum TransientError {
    /// Network connection lost or reset
    ConnectionLost,
    /// Request timed out
    Timeout,
    /// Server returned 5xx error
    ServerError(u16),
    /// Stream stalled (no data received for too long)
    StreamStall,
}

impl AgentError {
    /// Whether this error is worth retrying.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AgentError::Transient { .. }
                | AgentError::RateLimit { .. }
                | AgentError::ContextOverflow { .. }
        )
    }

    /// Suggested delay before retry, if applicable.
    pub fn retry_delay(&self) -> Option<Duration> {
        match self {
            AgentError::Transient { retry_after, .. } => {
                Some(retry_after.unwrap_or(Duration::from_secs(2)))
            }
            AgentError::RateLimit { retry_after } => Some(*retry_after),
            AgentError::ContextOverflow { .. } => Some(Duration::ZERO), // compact immediately
            _ => None,
        }
    }

    /// Human-readable error category for logging.
    pub fn category(&self) -> &'static str {
        match self {
            AgentError::Transient { .. } => "transient",
            AgentError::ContextOverflow { .. } => "context_overflow",
            AgentError::ToolPairing { .. } => "tool_pairing",
            AgentError::MaxOutputTokens { .. } => "max_output_tokens",
            AgentError::ModelConfusion { .. } => "model_confusion",
            AgentError::Auth { .. } => "auth",
            AgentError::RateLimit { .. } => "rate_limit",
            AgentError::Fatal { .. } => "fatal",
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Transient { source, retry_after } => {
                write!(f, "Transient error: {:?}", source)?;
                if let Some(d) = retry_after {
                    write!(f, " (retry after {:?})", d)?;
                }
                Ok(())
            }
            AgentError::ContextOverflow { current_tokens, max_tokens } => {
                write!(f, "Context overflow: {} / {} tokens", current_tokens, max_tokens)
            }
            AgentError::ToolPairing { orphaned_ids } => {
                write!(f, "Orphaned tool pairs: {:?}", orphaned_ids)
            }
            AgentError::MaxOutputTokens { partial_text } => {
                write!(f, "Max output tokens reached ({} chars partial)", partial_text.len())
            }
            AgentError::ModelConfusion { detected_patterns } => {
                write!(f, "Model confusion detected: {:?}", detected_patterns)
            }
            AgentError::Auth { message } => {
                write!(f, "Authentication error: {}", message)
            }
            AgentError::RateLimit { retry_after } => {
                write!(f, "Rate limited (retry after {:?})", retry_after)
            }
            AgentError::Fatal { message } => {
                write!(f, "Fatal error: {}", message)
            }
        }
    }
}

impl std::error::Error for AgentError {}

/// Classify an error string into a structured AgentError.
///
/// This replaces the old string-matching `is_transient_error()` with
/// a function that produces typed error variants for precise handling.
pub fn classify_error(err_str: &str) -> AgentError {
    let lower = err_str.to_lowercase();

    // Auth errors (never retryable)
    if lower.contains("authentication") || lower.contains("invalid api key")
        || lower.contains("unauthorized") || lower.contains("401")
        || lower.contains("403")
    {
        return AgentError::Auth {
            message: err_str.to_string(),
        };
    }

    // Rate limiting
    if lower.contains("429") || lower.contains("rate limit") {
        let retry_after = extract_retry_after(err_str).unwrap_or(Duration::from_secs(10));
        return AgentError::RateLimit { retry_after };
    }

    // Context overflow
    if lower.contains("prompt is too long") || lower.contains("max tokens")
        || lower.contains("context_length_exceeded") || lower.contains("too many tokens")
    {
        return AgentError::ContextOverflow {
            current_tokens: 0,
            max_tokens: 0,
        };
    }

    // Server errors (5xx)
    if let Some(status) = extract_http_status(&lower) {
        if (500..600).contains(&status) {
            return AgentError::Transient {
                source: TransientError::ServerError(status),
                retry_after: None,
            };
        }
    }

    // Connection errors
    if lower.contains("connection lost") || lower.contains("connection reset")
        || lower.contains("connection error") || lower.contains("connection refused")
        || lower.contains("broken pipe") || lower.contains("peer closed")
        || lower.contains("upstream connect error")
    {
        return AgentError::Transient {
            source: TransientError::ConnectionLost,
            retry_after: None,
        };
    }

    // Timeout errors
    if lower.contains("timeout") || lower.contains("timed out")
        || lower.contains("pool timeout") || lower.contains("connect timeout")
    {
        return AgentError::Transient {
            source: TransientError::Timeout,
            retry_after: None,
        };
    }

    // Stream stall
    if lower.contains("stream stall") || lower.contains("stream error")
        || lower.contains("remote protocol error")
    {
        return AgentError::Transient {
            source: TransientError::StreamStall,
            retry_after: None,
        };
    }

    // Default: fatal
    AgentError::Fatal {
        message: err_str.to_string(),
    }
}

/// Check if an error is transient (backward-compatible with old `is_transient_error`).
///
/// Prefer using `classify_error()` for new code.
pub fn is_transient_error(err_str: &str) -> bool {
    classify_error(err_str).is_retryable()
}

/// Extract HTTP status code from error string.
fn extract_http_status(lower: &str) -> Option<u16> {
    // Look for patterns like "status: 503", "502 bad gateway", etc.
    let status_codes = [429, 500, 502, 503, 504];
    for code in status_codes {
        if lower.contains(&code.to_string()) {
            return Some(code);
        }
    }
    None
}

/// Extract retry-after duration from error string (e.g., "retry-after: 30").
fn extract_retry_after(err_str: &str) -> Option<Duration> {
    let lower = err_str.to_lowercase();
    if let Some(pos) = lower.find("retry-after") {
        let after = &lower[pos + 11..];
        let after = after.trim_start_matches(|c: char| c == ':' || c == ' ');
        if let Ok(secs) = after.split(|c: char| !c.is_ascii_digit()).next().unwrap_or("0").parse::<u64>() {
            if secs > 0 {
                return Some(Duration::from_secs(secs));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_auth_error() {
        let err = classify_error("Invalid API key provided");
        assert!(matches!(err, AgentError::Auth { .. }));
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_classify_rate_limit() {
        let err = classify_error("429 Too Many Requests");
        assert!(matches!(err, AgentError::RateLimit { .. }));
        assert!(err.is_retryable());
    }

    #[test]
    fn test_classify_context_overflow() {
        let err = classify_error("prompt is too long: max_tokens");
        assert!(matches!(err, AgentError::ContextOverflow { .. }));
        assert!(err.is_retryable());
    }

    #[test]
    fn test_classify_connection_lost() {
        let err = classify_error("Connection reset by peer");
        assert!(matches!(err, AgentError::Transient { source: TransientError::ConnectionLost, .. }));
        assert!(err.is_retryable());
    }

    #[test]
    fn test_classify_timeout() {
        let err = classify_error("Request timed out after 30s");
        assert!(matches!(err, AgentError::Transient { source: TransientError::Timeout, .. }));
    }

    #[test]
    fn test_classify_server_error() {
        let err = classify_error("502 Bad Gateway");
        assert!(matches!(err, AgentError::Transient { source: TransientError::ServerError(502), .. }));
    }

    #[test]
    fn test_classify_fatal() {
        let err = classify_error("Unknown internal error");
        assert!(matches!(err, AgentError::Fatal { .. }));
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_backward_compat_is_transient() {
        assert!(is_transient_error("connection reset"));
        assert!(is_transient_error("Request timed out"));
        assert!(is_transient_error("503 Service Unavailable"));
        assert!(is_transient_error("429 rate limit exceeded"));
        assert!(!is_transient_error("Invalid API key"));
        assert!(!is_transient_error("Unknown error"));
    }

    #[test]
    fn test_error_category() {
        assert_eq!(classify_error("connection lost").category(), "transient");
        assert_eq!(classify_error("Invalid API key").category(), "auth");
        assert_eq!(classify_error("429 rate limit").category(), "rate_limit");
        assert_eq!(classify_error("prompt is too long").category(), "context_overflow");
    }

    #[test]
    fn test_retry_delay() {
        let err = classify_error("connection reset");
        assert_eq!(err.retry_delay(), Some(Duration::from_secs(2)));

        let err = classify_error("429 rate limit");
        assert!(err.retry_delay().is_some());
    }
}
