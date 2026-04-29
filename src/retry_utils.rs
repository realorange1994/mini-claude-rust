//! Retry utilities — jittered backoff for decorrelated retries.
//!
//! Replaces fixed exponential backoff with jittered delays to prevent
//! thundering-herd retry spikes when multiple sessions hit the same
//! rate-limited provider concurrently.

use std::time::Duration;

/// Compute a jittered exponential backoff delay.
///
/// Returns: delay = min(base * 2^(attempt-1), max_delay) + uniform(0, jitter_ratio * delay)
///
/// Parameters:
/// - `attempt`: 1-based retry attempt number
/// - `base_delay`: base delay for attempt 1 (default: 5s)
/// - `max_delay`: maximum delay cap (default: 120s)
/// - `jitter_ratio`: fraction of computed delay to use as jitter range (default: 0.5)
pub fn jittered_backoff(attempt: usize, base_delay: Duration, max_delay: Duration, jitter_ratio: f64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(62);
    let base = base_delay.as_millis() as f64;
    let max = max_delay.as_millis() as f64;

    let delay = (base * 2.0_f64.powi(exponent as i32)).min(max);

    // Add uniform random jitter in [0, jitter_ratio * delay]
    let jitter = fastrand::f64() * jitter_ratio * delay;
    Duration::from_millis((delay + jitter) as u64)
}

/// Default jittered backoff: base=5s, max=120s, jitter=0.5
pub fn jittered_backoff_default(attempt: usize) -> Duration {
    jittered_backoff(
        attempt,
        Duration::from_secs(5),
        Duration::from_secs(120),
        0.5,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_delay() {
        // Attempt 1 with no jitter would be 5s, with jitter it's 5s-7.5s
        let d = jittered_backoff(1, Duration::from_secs(5), Duration::from_secs(120), 0.0);
        assert_eq!(d, Duration::from_secs(5));
    }

    #[test]
    fn test_exponential_growth() {
        // Attempt 3: 5 * 2^2 = 20s
        let d = jittered_backoff(3, Duration::from_secs(5), Duration::from_secs(120), 0.0);
        assert_eq!(d, Duration::from_secs(20));
    }

    #[test]
    fn test_max_cap() {
        // Attempt 10: 5 * 2^9 = 2560s -> capped at 120s
        let d = jittered_backoff(10, Duration::from_secs(5), Duration::from_secs(120), 0.0);
        assert_eq!(d, Duration::from_secs(120));
    }

    #[test]
    fn test_jitter_adds_delay() {
        // With jitter_ratio=0.5, delay is between base and base*1.5
        let d1 = jittered_backoff(1, Duration::from_secs(5), Duration::from_secs(120), 0.5);
        assert!(d1 >= Duration::from_secs(5));
        assert!(d1 <= Duration::from_millis(7500));
    }

    #[test]
    fn test_attempt_zero_treated_as_one() {
        let d = jittered_backoff(0, Duration::from_secs(5), Duration::from_secs(120), 0.0);
        assert_eq!(d, Duration::from_secs(5));
    }
}
