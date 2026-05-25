//! Data structure utilities.
//! Ported from upstream utils_data.go (232 lines).
//!
//! Provides:
//! - CircularBuffer: fixed-size buffer that evicts oldest entries
//! - JitteredBackoff: exponential backoff with jitter for retry delays
//! - PromptHistory: JSONL-based prompt history persistence

use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

// =============================================================================
// Section 1: CircularBuffer
// =============================================================================

/// Fixed-size buffer that evicts the oldest entries when full.
pub struct CircularBuffer<T> {
    data: Vec<T>,
    capacity: usize,
}

impl<T> CircularBuffer<T> {
    /// Create a new circular buffer with the given capacity.
    pub fn new(capacity: usize) -> Self {
        let cap = if capacity < 1 { 1 } else { capacity };
        Self {
            data: Vec::with_capacity(cap),
            capacity: cap,
        }
    }

    /// Append an item, evicting the oldest if at capacity.
    pub fn add(&mut self, item: T) {
        if self.data.len() >= self.capacity {
            // Evict the oldest item (shift left)
            self.data.remove(0);
            self.data.push(item);
        } else {
            self.data.push(item);
        }
    }

    /// Add multiple items to the buffer.
    pub fn add_all(&mut self, items: Vec<T>) {
        for item in items {
            self.add(item);
        }
    }

    /// Number of items currently in the buffer.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Return a copy of all items in insertion order.
    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.data.clone()
    }

    /// Return the last N items from the buffer.
    pub fn get_recent(&self, n: usize) -> Vec<T>
    where
        T: Clone,
    {
        if n >= self.data.len() {
            return self.to_vec();
        }
        self.data[self.data.len() - n..].to_vec()
    }

    /// Remove all items from the buffer.
    pub fn clear(&mut self) {
        self.data.clear();
    }
}

// =============================================================================
// Section 2: Jittered Backoff
// =============================================================================

/// Configuration for jittered exponential backoff.
pub struct JitterConfig {
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub jitter_ratio: f64,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(120),
            jitter_ratio: 0.5,
        }
    }
}

/// Compute a jittered exponential backoff delay.
///
/// Returns: delay = min(base * 2^(attempt-1), maxDelay) + uniform(0, jitterRatio * delay)
pub fn jittered_backoff(attempt: i32, config: &JitterConfig) -> Duration {
    let exponent = if attempt <= 1 { 0 } else { (attempt - 1) as u32 };
    if exponent >= 63 {
        return config.max_delay;
    }

    let base_secs = config.base_delay.as_secs_f64();
    let max_secs = config.max_delay.as_secs_f64();

    let delay_secs = base_secs * (2.0_f64.powi(exponent as i32));
    let delay_secs = if delay_secs > max_secs { max_secs } else { delay_secs };

    let jitter = rand::random::<f64>() * config.jitter_ratio * delay_secs;
    Duration::from_secs_f64(delay_secs + jitter)
}

// =============================================================================
// Section 3: Prompt History
// =============================================================================

/// A single prompt history record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PromptEntry {
    pub text: String,
    pub timestamp: String,
    pub session_id: String,
}

/// Prompt history manager that writes to a JSONL file.
pub struct PromptHistory {
    file_path: PathBuf,
    mu: Mutex<()>,
}

impl PromptHistory {
    /// Create a history manager that writes to .claude/history.jsonl.
    pub fn new(project_dir: &str) -> Self {
        let dir = PathBuf::from(project_dir).join(".claude");
        let _ = fs::create_dir_all(&dir);
        Self {
            file_path: dir.join("history.jsonl"),
            mu: Mutex::new(()),
        }
    }

    /// Append a prompt to the history file.
    pub fn record(&self, text: &str, session_id: &str) {
        let _guard = self.mu.lock().unwrap();
        let entry = PromptEntry {
            text: text.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_id: session_id.to_string(),
        };
        if let Ok(data) = serde_json::to_string(&entry) {
            if let Ok(mut f) = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&self.file_path)
            {
                let _ = f.write_all(data.as_bytes());
                let _ = f.write_all(b"\n");
            }
        }
    }

    /// Load the most recent N prompts from history.
    pub fn load_recent(&self, n: usize) -> Vec<PromptEntry> {
        let _guard = self.mu.lock().unwrap();
        if let Ok(data) = fs::read_to_string(&self.file_path) {
            let entries: Vec<PromptEntry> = data
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();

            if entries.len() > n {
                entries[entries.len() - n..].to_vec()
            } else {
                entries
            }
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circular_buffer_add() {
        let mut buf = CircularBuffer::<i32>::new(3);
        buf.add(1);
        buf.add(2);
        buf.add(3);
        assert_eq!(buf.to_vec(), vec![1, 2, 3]);

        // Adding 4 evicts 1
        buf.add(4);
        assert_eq!(buf.to_vec(), vec![2, 3, 4]);
    }

    #[test]
    fn test_circular_buffer_get_recent() {
        let mut buf = CircularBuffer::<i32>::new(5);
        for i in 1..=5 {
            buf.add(i);
        }
        assert_eq!(buf.get_recent(2), vec![4, 5]);
        assert_eq!(buf.get_recent(10), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_circular_buffer_clear() {
        let mut buf = CircularBuffer::<i32>::new(5);
        buf.add(1);
        buf.add(2);
        buf.clear();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_circular_buffer_minimum_capacity() {
        let buf = CircularBuffer::<i32>::new(0);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_jittered_backoff() {
        let config = JitterConfig::default();
        let d1 = jittered_backoff(1, &config);
        // Attempt 1: base_delay (5s) + jitter, should be >= 5s, < 10s typically
        assert!(d1 >= Duration::from_secs(5));

        let d10 = jittered_backoff(10, &config);
        // Attempt 10: capped at max_delay
        assert!(d10 <= config.max_delay + Duration::from_secs(1));
    }
}