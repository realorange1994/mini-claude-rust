use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore, Notify};
use tokio::time::timeout;

// Sequential: runs async functions sequentially, returning results in order
pub async fn sequential<T, R, F, Fut>(items: Vec<T>, f: F) -> Vec<R>
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = R>,
{
    let mut results = Vec::with_capacity(items.len());
    for item in items {
        results.push(f(item).await);
    }
    results
}

// SequentialConcurrent: runs async functions with a concurrency limit
pub async fn sequential_concurrent<T, R, F, Fut>(items: Vec<T>, limit: usize, f: F) -> Vec<R>
where
    F: Fn(T) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = R> + Send,
    T: Send + 'static,
    R: Send + 'static,
{
    if items.is_empty() {
        return Vec::new();
    }

    let semaphore = Arc::new(Semaphore::new(limit));
    let results = Arc::new(Mutex::new(vec![None::<R>; items.len()]));
    let mut handles = Vec::new();

    for (i, item) in items.into_iter().enumerate() {
        let sem = semaphore.clone();
        let res = results.clone();
        let f = f.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = f(item).await;
            res.lock().await[i] = Some(result);
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    results.lock().await.into_iter().map(|r| r.unwrap()).collect()
}

// DeferredPromise: a deferred resolution pattern
pub struct DeferredPromise<T> {
    value: Arc<Mutex<Option<T>>>,
    notify: Arc<Notify>,
    resolved: Arc<Mutex<bool>>,
}

impl<T: Clone + Send + 'static> DeferredPromise<T> {
    pub fn new() -> Self {
        Self {
            value: Arc::new(Mutex::new(None)),
            notify: Arc::new(Notify::new()),
            resolved: Arc::new(Mutex::new(false)),
        }
    }

    pub async fn resolve(&self, val: T) {
        let mut resolved = self.resolved.lock().await;
        if !*resolved {
            *resolved = true;
            *self.value.lock().await = Some(val);
            self.notify.notify_one();
        }
    }

    pub async fn reject(&self) {
        let mut resolved = self.resolved.lock().await;
        if !*resolved {
            *resolved = true;
            self.notify.notify_one();
        }
    }

    pub async fn is_resolved(&self) -> bool {
        *self.resolved.lock().await
    }

    pub async fn wait(&self) -> Option<T> {
        if self.is_resolved().await {
            return self.value.lock().await.clone();
        }
        self.notify.notified().await;
        self.value.lock().await.clone()
    }
}

// Sleep: pauses for the specified duration
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

// Sleep with abort: if the cancel signal is received, return early
pub async fn sleep_with_abort(duration: Duration, cancel: tokio::sync::watch::Receiver<bool>, throw_on_abort: bool) -> Result<(), String> {
    tokio::select! {
        _ = tokio::time::sleep(duration) => Ok(()),
        _ = cancel.changed() => {
            if throw_on_abort {
                Err("aborted".to_string())
            } else {
                Ok(())
            }
        }
    }
}

// WithTimeout: wraps a future with a timeout
pub async fn with_timeout<F, R>(future: F, dur: Duration, msg: Option<&str>) -> Result<R, String>
where
    F: Future<Output = Result<R, String>>,
{
    match timeout(dur, future).await {
        Ok(result) => result,
        Err(_) => Err(msg.unwrap_or("operation timed out").to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sequential() {
        let items = vec![1, 2, 3];
        let results = sequential(items, |x| async move { x * 2 }).await;
        assert_eq!(results, vec![2, 4, 6]);
    }

    #[tokio::test]
    async fn test_deferred_promise() {
        let dp = DeferredPromise::new();
        assert!(!dp.is_resolved().await);
        dp.resolve(42).await;
        assert!(dp.is_resolved().await);
        assert_eq!(dp.wait().await, Some(42));
    }

    #[tokio::test]
    async fn test_sleep() {
        let start = std::time::Instant::now();
        sleep(Duration::from_millis(50)).await;
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[tokio::test]
    async fn test_with_timeout() {
        let result = with_timeout(
            async { Ok::<i32, String>(42) },
            Duration::from_secs(1),
            None,
        )
        .await;
        assert_eq!(result, Ok(42));
    }

    #[tokio::test]
    async fn test_with_timeout_expired() {
        let result = with_timeout(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok::<i32, String>(42)
            },
            Duration::from_millis(50),
            Some("custom timeout"),
        )
        .await;
        assert_eq!(result, Err("custom timeout".to_string()));
    }
}
