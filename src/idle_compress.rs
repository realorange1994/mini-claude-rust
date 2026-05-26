//! Idle compression timer.
//! Ported from upstream idle_compress.go (143 lines).
//!
//! Automatically compresses the conversation when the user has been idle
//! (not typing) for the configured delay. The timer only triggers if the
//! conversation is large enough to warrant compression.
//!
//! Integration:
//!   - Call start() after agent loop completes (before waiting for user input)
//!   - Call cancel() before reading user input (ReadLine/ReadString)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Report from a compression run.
#[derive(Debug, Clone, Default)]
pub struct CompressionReport {
    pub pre_tokens: i64,
    pub post_tokens: i64,
    pub pre_entries: usize,
    pub post_entries: usize,
    pub saved_tokens: i64,
}

/// Shared state protected by a mutex.
struct IdleCompressorState {
    delay: Duration,
    min_tokens: i64,
    min_messages: usize,
    consumed_turns: usize,
    callback: Option<Box<dyn Fn() -> CompressionReport + Send + Sync>>,
}

/// Idle compression timer.
pub struct IdleCompressor {
    state: Arc<Mutex<IdleCompressorState>>,
    compressing: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl IdleCompressor {
    /// Create a new idle compressor with the given delay.
    /// Default thresholds: 20K tokens, 30 messages.
    pub fn new(delay: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(IdleCompressorState {
                delay,
                min_tokens: 20_000,
                min_messages: 30,
                consumed_turns: 0,
                callback: None,
            })),
            compressing: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }

    /// Set the minimum thresholds for triggering compression.
    pub fn set_thresholds(&self, min_tokens: i64, min_messages: usize) {
        let mut s = self.state.lock().unwrap();
        s.min_tokens = min_tokens;
        s.min_messages = min_messages;
    }

    /// Set the callback to run when idle compression triggers.
    pub fn set_callback<F>(&self, callback: F)
    where
        F: Fn() -> CompressionReport + Send + Sync + 'static,
    {
        let mut s = self.state.lock().unwrap();
        s.callback = Some(Box::new(callback));
    }

    /// Start monitoring for idle compression.
    /// Must be called after agent run completes and before waiting for user input.
    ///
    /// Returns true if the timer was started, false if the conversation is too small.
    pub fn start<F>(&mut self, should_compact: F, consumed_turns: usize) -> bool
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        // Stop any existing timer
        self.cancel();

        // Check if conversation is large enough
        if !should_compact() {
            return false;
        }

        {
            let mut s = self.state.lock().unwrap();
            s.consumed_turns = consumed_turns;
        }
        self.compressing.store(false, Ordering::Relaxed);
        self.stopped.store(false, Ordering::Relaxed);

        let state = Arc::clone(&self.state);
        let compressing = Arc::clone(&self.compressing);
        let stopped = Arc::clone(&self.stopped);
        let should_compact = Arc::new(should_compact);

        let handle = std::thread::spawn(move || {
            let delay = {
                let s = state.lock().unwrap();
                s.delay
            };
            std::thread::sleep(delay);

            // Check if stopped
            if stopped.load(Ordering::Relaxed) {
                return;
            }

            // Double-check should_compact
            if !should_compact() {
                return;
            }

            // Only one compression at a time
            if compressing.swap(true, Ordering::Relaxed) {
                return;
            }

            // Run the callback
            {
                let s = state.lock().unwrap();
                if let Some(ref cb) = s.callback {
                    let report = cb();
                    eprintln!(
                        "[idle] User idle for {:?}, compressed: {} -> {} entries, {} -> {} tokens (saved {})",
                        delay,
                        report.pre_entries, report.post_entries,
                        report.pre_tokens, report.post_tokens, report.saved_tokens
                    );
                } else {
                    eprintln!("[idle] User idle for {:?}, compression triggered (no callback configured)", delay);
                }
            }

            compressing.store(false, Ordering::Relaxed);
        });

        self.handle = Some(handle);
        true
    }

    /// Cancel the idle timer. Must be called before reading user input.
    /// If compression is in progress, it attempts to cancel it.
    pub fn cancel(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.stopped.store(true, Ordering::Relaxed);
            self.compressing.store(false, Ordering::Relaxed);
            let _ = handle.join();
        }
    }

    /// Returns true if an idle-triggered compression is in progress.
    pub fn is_compressing(&self) -> bool {
        self.compressing.load(Ordering::Relaxed)
    }

    /// Returns the delay duration.
    pub fn delay(&self) -> Duration {
        let s = self.state.lock().unwrap();
        s.delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI32;
    use std::time::Duration;

    #[test]
    fn test_idle_timer_thresholds() {
        let timer = IdleCompressor::new(Duration::from_millis(50));
        timer.set_thresholds(1000, 5);

        // Too small - should return false
        let started = timer.start(
            || false, // should_compact returns false
            0,
        );
        assert!(!started);
    }

    #[test]
    fn test_idle_timer_fires_when_enough_content() {
        let counter = Arc::new(AtomicI32::new(0));
        let counter_clone = Arc::clone(&counter);

        let mut timer = IdleCompressor::new(Duration::from_millis(100));
        timer.set_callback(move || {
            counter_clone.store(1, Ordering::Relaxed);
            CompressionReport {
                pre_tokens: 5000,
                post_tokens: 2000,
                pre_entries: 20,
                post_entries: 10,
                saved_tokens: 3000,
            }
        });

        let started = timer.start(|| true, 0);
        assert!(started);

        // Wait for it to fire
        std::thread::sleep(Duration::from_millis(200));
        timer.cancel();

        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_idle_timer_cancelled_before_fire() {
        let counter = Arc::new(AtomicI32::new(0));
        let counter_clone = Arc::clone(&counter);

        let mut timer = IdleCompressor::new(Duration::from_millis(500));
        timer.set_callback(move || {
            counter_clone.store(1, Ordering::Relaxed);
            CompressionReport::default()
        });

        let started = timer.start(|| true, 0);
        assert!(started);

        // Cancel immediately
        timer.cancel();

        // Wait longer than the delay - callback should not have fired
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_is_compressing() {
        let timer = IdleCompressor::new(Duration::from_secs(60));
        assert!(!timer.is_compressing());
    }
}
