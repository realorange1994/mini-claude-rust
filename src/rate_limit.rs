//! Rate limit tracking for inference API responses.
//!
//! Captures x-ratelimit-* headers from provider responses and provides
//! formatted display and retry delay estimation.
//!
//! Header schema (12 headers total):
//!   x-ratelimit-limit-requests          RPM cap
//!   x-ratelimit-limit-requests-1h       RPH cap
//!   x-ratelimit-limit-tokens            TPM cap
//!   x-ratelimit-limit-tokens-1h         TPH cap
//!   x-ratelimit-remaining-requests      requests left in minute window
//!   x-ratelimit-remaining-requests-1h   requests left in hour window
//!   x-ratelimit-remaining-tokens        tokens left in minute window
//!   x-ratelimit-remaining-tokens-1h     tokens left in hour window
//!   x-ratelimit-reset-requests          seconds until minute request window resets
//!   x-ratelimit-reset-requests-1h       seconds until hour request window resets
//!   x-ratelimit-reset-tokens            seconds until minute token window resets
//!   x-ratelimit-reset-tokens-1h         seconds until hour token window resets

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One rate-limit window (e.g. requests per minute).
#[derive(Debug, Clone)]
pub struct RateLimitBucket {
    pub limit: i64,
    pub remaining: i64,
    pub reset_seconds: f64,
    pub captured_at: Instant,
}

impl Default for RateLimitBucket {
    fn default() -> Self {
        Self {
            limit: 0,
            remaining: 0,
            reset_seconds: 0.0,
            captured_at: Instant::now(),
        }
    }
}

impl RateLimitBucket {
    pub fn used(&self) -> i64 {
        (self.limit - self.remaining).max(0)
    }

    pub fn usage_pct(&self) -> f64 {
        if self.limit <= 0 {
            return 0.0;
        }
        self.used() as f64 / self.limit as f64 * 100.0
    }

    pub fn remaining_seconds_now(&self) -> f64 {
        let elapsed = self.captured_at.elapsed().as_secs_f64();
        (self.reset_seconds - elapsed).max(0.0)
    }
}

/// Full rate-limit state parsed from response headers.
#[derive(Default)]
pub struct RateLimitState {
    inner: Mutex<RateLimitStateInner>,
}

#[derive(Default, Clone)]
struct RateLimitStateInner {
    requests_min: RateLimitBucket,
    requests_hour: RateLimitBucket,
    tokens_min: RateLimitBucket,
    tokens_hour: RateLimitBucket,
    captured_at: Option<Instant>,
    provider: String,
}

impl RateLimitState {
    pub fn has_data(&self) -> bool {
        self.inner.lock().unwrap().captured_at.is_some()
    }

    pub fn age(&self) -> Duration {
        let guard = self.inner.lock().unwrap();
        match guard.captured_at {
            Some(t) => t.elapsed(),
            None => Duration::from_secs(u64::MAX),
        }
    }

    /// Returns the bucket with highest usage percentage.
    pub fn most_constrained(&self) -> Option<(String, RateLimitBucket)> {
        let guard = self.inner.lock().unwrap();
        [
            ("requests/min", &guard.requests_min),
            ("requests/hr", &guard.requests_hour),
            ("tokens/min", &guard.tokens_min),
            ("tokens/hr", &guard.tokens_hour),
        ]
        .iter()
        .filter(|(_, b)| b.limit > 0)
        .max_by(|a, b| a.1.usage_pct().partial_cmp(&b.1.usage_pct()).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(label, bucket)| ((*label).to_string(), (*bucket).clone()))
    }

    /// Estimate delay before retry based on rate limit state.
    /// Returns None if no rate limit data or if retry should be safe now.
    pub fn retry_delay(&self) -> Option<Duration> {
        let guard = self.inner.lock().unwrap();
        let buckets = [
            &guard.requests_min,
            &guard.requests_hour,
            &guard.tokens_min,
            &guard.tokens_hour,
        ];

        let max_delay = buckets
            .iter()
            .filter(|b| b.remaining <= 0 && b.limit > 0)
            .map(|b| b.remaining_seconds_now())
            .fold(0.0_f64, f64::max);

        if max_delay <= 0.0 {
            None
        } else {
            // 10% safety margin
            Some(Duration::from_secs_f64(max_delay * 1.1))
        }
    }

    /// Merge new rate limit data into the state.
    pub fn update(&self, new: &RateLimitState) {
        let new_guard = new.inner.lock().unwrap();
        let mut guard = self.inner.lock().unwrap();

        if new_guard.requests_min.limit > 0 {
            guard.requests_min = new_guard.requests_min.clone();
        }
        if new_guard.requests_hour.limit > 0 {
            guard.requests_hour = new_guard.requests_hour.clone();
        }
        if new_guard.tokens_min.limit > 0 {
            guard.tokens_min = new_guard.tokens_min.clone();
        }
        if new_guard.tokens_hour.limit > 0 {
            guard.tokens_hour = new_guard.tokens_hour.clone();
        }
        if new_guard.captured_at.is_some() {
            guard.captured_at = new_guard.captured_at;
            guard.provider = new_guard.provider.clone();
        }
    }
}

/// Parse rate limit headers from reqwest response.
pub fn parse_rate_limit_headers(
    headers: &reqwest::header::HeaderMap,
    provider: &str,
) -> Option<RateLimitState> {
    // Normalize headers to lowercase strings
    let lowered: std::collections::HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
        .collect();

    // Quick check: at least one rate limit header must exist
    let has_any = lowered.keys().any(|k| k.starts_with("x-ratelimit-"));
    if !has_any {
        return None;
    }

    let now = Instant::now();

    let bucket = |resource: &str, suffix: &str| -> RateLimitBucket {
        let tag = format!("{}{}", resource, suffix);
        RateLimitBucket {
            limit: parse_int(lowered.get(&format!("x-ratelimit-limit-{}", tag))),
            remaining: parse_int(lowered.get(&format!("x-ratelimit-remaining-{}", tag))),
            reset_seconds: parse_float(lowered.get(&format!("x-ratelimit-reset-{}", tag))),
            captured_at: now,
        }
    };

    let inner = RateLimitStateInner {
        requests_min: bucket("requests", ""),
        requests_hour: bucket("requests", "-1h"),
        tokens_min: bucket("tokens", ""),
        tokens_hour: bucket("tokens", "-1h"),
        captured_at: Some(now),
        provider: provider.to_string(),
    };

    Some(RateLimitState {
        inner: Mutex::new(inner),
    })
}

fn parse_int(value: Option<&String>) -> i64 {
    value
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

fn parse_float(value: Option<&String>) -> f64 {
    value
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

// ── Formatting ──

fn fmt_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

fn fmt_seconds(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        let m = s / 60;
        let sec = s % 60;
        if sec > 0 {
            format!("{}m {}s", m, sec)
        } else {
            format!("{}m", m)
        }
    } else {
        let h = s / 3600;
        let m = (s % 3600) / 60;
        if m > 0 {
            format!("{}h {}m", h, m)
        } else {
            format!("{}h", h)
        }
    }
}

fn bar(pct: f64, width: usize) -> String {
    let filled = (pct / 100.0 * width as f64) as usize;
    let filled = filled.min(width);
    let empty = width - filled;
    format!("[{}{}]", "#".repeat(filled), "-".repeat(empty))
}

fn bucket_line(label: &str, bucket: &RateLimitBucket) -> String {
    if bucket.limit <= 0 {
        return format!("  {:14}  (no data)", label);
    }

    let pct = bucket.usage_pct();
    let used = fmt_count(bucket.used());
    let limit = fmt_count(bucket.limit);
    let remaining = fmt_count(bucket.remaining);
    let reset = fmt_seconds(bucket.remaining_seconds_now());

    let b = bar(pct, 20);
    format!(
        "  {:14} {} {:5.1}%  {}/{} used  ({} left, resets in {})",
        label, b, pct, used, limit, remaining, reset
    )
}

/// Format rate limit state for terminal display.
pub fn format_rate_limit_display(state: &RateLimitState) -> String {
    let guard = state.inner.lock().unwrap();
    let captured_at = guard.captured_at;
    let provider = guard.provider.clone();
    let requests_min = guard.requests_min.clone();
    let requests_hour = guard.requests_hour.clone();
    let tokens_min = guard.tokens_min.clone();
    let tokens_hour = guard.tokens_hour.clone();
    drop(guard);

    let Some(captured_at) = captured_at else {
        return "No rate limit data yet -- make an API request first.".to_string();
    };

    let age = captured_at.elapsed();
    let freshness = if age < Duration::from_secs(5) {
        "just now".to_string()
    } else if age < Duration::from_secs(60) {
        format!("{:.0}s ago", age.as_secs_f64())
    } else {
        format!("{} ago", fmt_seconds(age.as_secs_f64()))
    };

    let provider_label = if provider.is_empty() {
        "Provider".to_string()
    } else {
        let mut p = provider.clone();
        if let Some(c) = p.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        p
    };

    let mut lines = vec![
        format!("{} Rate Limits (captured {}):", provider_label, freshness),
        "".to_string(),
        bucket_line("Requests/min", &requests_min),
        bucket_line("Requests/hr", &requests_hour),
        "".to_string(),
        bucket_line("Tokens/min", &tokens_min),
        bucket_line("Tokens/hr", &tokens_hour),
    ];

    // Warnings
    let buckets = [
        ("requests/min", &requests_min),
        ("requests/hr", &requests_hour),
        ("tokens/min", &tokens_min),
        ("tokens/hr", &tokens_hour),
    ];

    let warnings: Vec<String> = buckets
        .iter()
        .filter(|(_, b)| b.limit > 0 && b.usage_pct() >= 80.0)
        .map(|(label, b)| {
            format!(
                "  [!] {} at {:.0}% -- resets in {}",
                label,
                b.usage_pct(),
                fmt_seconds(b.remaining_seconds_now())
            )
        })
        .collect();

    if !warnings.is_empty() {
        lines.push("".to_string());
        lines.extend(warnings);
    }

    lines.join("\n")
}

/// One-line compact summary for status bars.
pub fn format_rate_limit_compact(state: &RateLimitState) -> String {
    let guard = state.inner.lock().unwrap();
    if guard.captured_at.is_none() {
        return "No rate limit data.".to_string();
    }

    let mut parts = Vec::new();
    let rm = &guard.requests_min;
    if rm.limit > 0 {
        parts.push(format!("RPM: {}/{}", rm.remaining, rm.limit));
    }
    let rh = &guard.requests_hour;
    if rh.limit > 0 {
        parts.push(format!(
            "RPH: {}/{}",
            fmt_count(rh.remaining),
            fmt_count(rh.limit)
        ));
    }
    let tm = &guard.tokens_min;
    if tm.limit > 0 {
        parts.push(format!(
            "TPM: {}/{}",
            fmt_count(tm.remaining),
            fmt_count(tm.limit)
        ));
    }
    let th = &guard.tokens_hour;
    if th.limit > 0 {
        parts.push(format!(
            "TPH: {}/{}",
            fmt_count(th.remaining),
            fmt_count(th.limit)
        ));
    }

    parts.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_default() {
        let b = RateLimitBucket::default();
        assert_eq!(b.limit, 0);
        assert_eq!(b.remaining, 0);
        assert_eq!(b.used(), 0);
        assert_eq!(b.usage_pct(), 0.0);
    }

    #[test]
    fn test_bucket_usage() {
        let b = RateLimitBucket {
            limit: 100,
            remaining: 75,
            reset_seconds: 30.0,
            captured_at: Instant::now(),
        };
        assert_eq!(b.used(), 25);
        assert_eq!(b.usage_pct(), 25.0);
    }

    #[test]
    fn test_fmt_count() {
        assert_eq!(fmt_count(799), "799");
        assert_eq!(fmt_count(1500), "1.5K");
        assert_eq!(fmt_count(33599), "33.6K");
        assert_eq!(fmt_count(7999856), "8.0M");
    }

    #[test]
    fn test_fmt_seconds() {
        assert_eq!(fmt_seconds(30.0), "30s");
        assert_eq!(fmt_seconds(134.0), "2m 14s");
        assert_eq!(fmt_seconds(3600.0), "1h");
        assert_eq!(fmt_seconds(3660.0), "1h 1m");
    }

    #[test]
    fn test_bar() {
        assert_eq!(bar(0.0, 20), "[--------------------]");
        assert_eq!(bar(50.0, 20), "[##########----------]");
        assert_eq!(bar(100.0, 20), "[####################]");
    }

    #[test]
    fn test_state_retry_delay_exhausted() {
        let state = RateLimitState::default();
        let now = Instant::now();
        {
            let mut guard = state.inner.lock().unwrap();
            guard.requests_min = RateLimitBucket {
                limit: 100,
                remaining: 0,
                reset_seconds: 60.0,
                captured_at: now,
            };
            guard.captured_at = Some(now);
        }

        let delay = state.retry_delay().unwrap();
        // Should be ~66s (60 * 1.1 safety margin), minus tiny elapsed
        assert!(delay.as_secs() > 60);
        assert!(delay.as_secs() < 70);
    }

    #[test]
    fn test_state_retry_delay_available() {
        let state = RateLimitState::default();
        {
            let mut guard = state.inner.lock().unwrap();
            guard.requests_min = RateLimitBucket {
                limit: 100,
                remaining: 50,
                reset_seconds: 60.0,
                captured_at: Instant::now(),
            };
            guard.captured_at = Some(Instant::now());
        }

        // Remaining > 0, so no delay needed
        assert!(state.retry_delay().is_none());
    }

    #[test]
    fn test_state_no_data() {
        let state = RateLimitState::default();
        assert!(!state.has_data());
        assert!(state.retry_delay().is_none());
    }

    #[test]
    fn test_format_compact_empty() {
        let state = RateLimitState::default();
        assert_eq!(format_rate_limit_compact(&state), "No rate limit data.");
    }

    #[test]
    fn test_format_display_empty() {
        let state = RateLimitState::default();
        assert!(format_rate_limit_display(&state).contains("No rate limit data"));
    }

    #[test]
    fn test_rate_limit_bucket_new() {
        let b = RateLimitBucket::default();
        assert_eq!(b.limit, 0);
        assert_eq!(b.remaining, 0);
    }

    #[test]
    fn test_rate_limit_state_retry_delay_no_constraint() {
        let state = RateLimitState::default();
        // Default state has no data, so retry_delay should be None (0 delay)
        assert!(state.retry_delay().is_none());
    }

    #[test]
    fn test_rate_limit_state_retry_delay_exhausted() {
        let state = RateLimitState::default();
        {
            let mut guard = state.inner.lock().unwrap();
            guard.requests_min = RateLimitBucket {
                limit: 100,
                remaining: 0,
                reset_seconds: 60.0,
                captured_at: Instant::now(),
            };
            guard.captured_at = Some(Instant::now());
        }

        let delay = state.retry_delay();
        assert!(delay.is_some());
        let d = delay.unwrap();
        assert!(d.as_secs_f64() > 0.0);
    }

    #[test]
    fn test_parse_rate_limit_headers_empty() {
        let headers = reqwest::header::HeaderMap::new();
        let result = parse_rate_limit_headers(&headers, "test");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_rate_limit_headers_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-limit-requests", "60".parse().unwrap());
        headers.insert("x-ratelimit-remaining-requests", "45".parse().unwrap());
        headers.insert("x-ratelimit-reset-requests", "30".parse().unwrap());
        headers.insert("x-ratelimit-limit-tokens", "100000".parse().unwrap());
        headers.insert("x-ratelimit-remaining-tokens", "80000".parse().unwrap());
        headers.insert("x-ratelimit-reset-tokens", "30".parse().unwrap());

        let state = parse_rate_limit_headers(&headers, "test_provider").unwrap();
        assert!(state.has_data());

        let guard = state.inner.lock().unwrap();
        assert_eq!(guard.requests_min.limit, 60);
        assert_eq!(guard.requests_min.remaining, 45);
        assert_eq!(guard.requests_min.reset_seconds, 30.0);
        assert_eq!(guard.tokens_min.limit, 100000);
        assert_eq!(guard.tokens_min.remaining, 80000);
        assert_eq!(guard.provider, "test_provider");
    }

    #[test]
    fn test_format_rate_limit_compact_no_data() {
        let state = RateLimitState::default();
        assert_eq!(format_rate_limit_compact(&state), "No rate limit data.");
    }
}
