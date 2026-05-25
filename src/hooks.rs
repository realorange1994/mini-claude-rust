//! Hook Manager module for compact lifecycle events
//!
//! Provides pre-compact and post-compact hook registration and execution.
//! Hooks are called synchronously with a timeout context.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

/// Trigger type for compaction events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTrigger {
    Manual,
    Auto,
    SmCompact,
}

impl std::fmt::Display for HookTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookTrigger::Manual => write!(f, "manual"),
            HookTrigger::Auto => write!(f, "auto"),
            HookTrigger::SmCompact => write!(f, "sm_compact"),
        }
    }
}

/// Input passed to pre-compact hooks
#[derive(Debug, Clone)]
pub struct PreCompactInput {
    pub trigger: HookTrigger,
    /// Instructions already queued for the summarizer; hooks can append to this
    pub custom_instructions: String,
}

/// Output from pre-compact hooks
#[derive(Debug, Clone, Default)]
pub struct PreCompactOutput {
    /// Additional instructions for the compaction prompt
    pub custom_instructions: String,
    /// Message to display to the user (logged, not injected into prompt)
    pub user_message: String,
}

/// Input passed to post-compact hooks
#[derive(Debug, Clone)]
pub struct PostCompactInput {
    pub trigger: HookTrigger,
    /// The summary that replaced the compacted conversation
    pub compact_summary: String,
    /// Files that were re-injected post-compaction
    pub recovered_files: Vec<String>,
}

/// Output from post-compact hooks
#[derive(Debug, Clone, Default)]
pub struct PostCompactOutput {
    /// Message to display to the user
    pub user_message: String,
    /// Content to inject as an attachment (added to prompt context)
    pub attachment: String,
}

/// Pre-compact hook handler signature
pub type PreCompactHandler =
    Arc<dyn Fn(PreCompactInput) -> PreCompactOutput + Send + Sync>;

/// Post-compact hook handler signature
pub type PostCompactHandler =
    Arc<dyn Fn(PostCompactInput) -> PostCompactOutput + Send + Sync>;

/// A registered hook entry
struct HookEntry {
    name: String,
    handler: Arc<dyn Fn(PreCompactInput) -> PreCompactOutput + Send + Sync>,
    timeout: Duration,
}

/// Thread-safe hook manager for compact lifecycle events
pub struct HookManager {
    pre_compact_hooks: Arc<Mutex<Vec<HookEntry>>>,
    post_compact_prelude: Arc<Mutex<Vec<(String, PostCompactHandler)>>>,
    post_compact_epilogue: Arc<Mutex<Vec<(String, PostCompactHandler)>>>,
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

impl HookManager {
    /// Create a new HookManager
    pub fn new() -> Self {
        Self {
            pre_compact_hooks: Arc::new(Mutex::new(Vec::new())),
            post_compact_prelude: Arc::new(Mutex::new(Vec::new())),
            post_compact_epilogue: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a pre-compact hook with the given timeout.
    /// The handler is called synchronously before compaction runs.
    pub fn register_pre_compact<H>(&self, name: &str, handler: H, timeout: Duration)
    where
        H: Fn(PreCompactInput) -> PreCompactOutput + Send + Sync + 'static,
    {
        let entry = HookEntry {
            name: name.to_string(),
            handler: Arc::new(handler),
            timeout,
        };
        let guard = self.pre_compact_hooks.blocking_lock();
        guard.push(entry);
    }

    /// Register a post-compact prelude hook.
    /// Prelude hooks run AFTER the summary is generated but BEFORE it is injected.
    /// They receive the raw summary and can modify it or add attachments.
    pub fn register_post_compact_prelude<H>(&self, name: &str, handler: H)
    where
        H: Fn(PostCompactInput) -> PostCompactOutput + Send + Sync + 'static,
    {
        let mut guard = self.post_compact_prelude.blocking_lock();
        guard.push((name.to_string(), Arc::new(handler)));
    }

    /// Register a post-compact epilogue hook.
    /// Epilogue hooks run AFTER the summary is injected into context.
    /// They are mainly for side effects (notifications, cleanup, etc.).
    pub fn register_post_compact_epilogue<H>(&self, name: &str, handler: H)
    where
        H: Fn(PostCompactInput) -> PostCompactOutput + Send + Sync + 'static,
    {
        let mut guard = self.post_compact_epilogue.blocking_lock();
        guard.push((name.to_string(), Arc::new(handler)));
    }

    /// Execute all pre-compact hooks sequentially.
    /// Outputs are merged: CustomInstructions concatenated, UserMessage appended.
    pub async fn execute_pre_compact_hooks(
        &self,
        input: PreCompactInput,
    ) -> (PreCompactOutput, Option<String>) {
        let guard = self.pre_compact_hooks.lock().await;
        if guard.is_empty() {
            return (PreCompactOutput::default(), None);
        }

        let mut result = PreCompactOutput::default();
        let mut first_err: Option<String> = None;

        for entry in guard.iter() {
            let timeout = if entry.timeout.is_zero() {
                Duration::from_secs(5)
            } else {
                entry.timeout
            };

            // Create a closure that captures the input and handler
            let input_clone = input.clone();
            let handler = Arc::clone(&entry.handler);

            // Run with timeout
            let out = match tokio::time::timeout(timeout, async {
                handler(input_clone)
            })
            .await
            {
                Ok(out) => out,
                Err(_) => {
                    let err_msg = format!("[hook:{}] timed out after {:?}", entry.name, timeout);
                    if first_err.is_none() {
                        first_err = Some(err_msg.clone());
                    } else {
                        first_err = first_err.as_ref().map(|e| format!("{}\n{}", e, err_msg));
                    }
                    result.user_message.push_str(&format!(
                        "\nPreCompact [hook:{}] timed out after {:?}",
                        entry.name, timeout
                    ));
                    continue;
                }
            };

            // Merge CustomInstructions
            if !out.custom_instructions.is_empty() {
                if result.custom_instructions.is_empty() {
                    result.custom_instructions = out.custom_instructions;
                } else {
                    result.custom_instructions.push_str("\n\n");
                    result.custom_instructions.push_str(&out.custom_instructions);
                }
            }

            // Append UserMessage
            if !out.user_message.is_empty() {
                if result.user_message.is_empty() {
                    result.user_message = format!("PreCompact [hook:{}] completed: {}", entry.name, out.user_message);
                } else {
                    result.user_message.push_str(&format!(
                        "\nPreCompact [hook:{}] completed: {}",
                        entry.name, out.user_message
                    ));
                }
            }
        }

        (result, first_err)
    }

    /// Execute post-compact prelude hooks (run BEFORE summary injection).
    /// These hooks can modify the summary or add attachments.
    pub async fn execute_post_compact_prelude_hooks(
        &self,
        input: PostCompactInput,
    ) -> PostCompactOutput {
        let guard = self.post_compact_prelude.lock().await;
        if guard.is_empty() {
            return PostCompactOutput::default();
        }

        let mut result = PostCompactOutput::default();

        for (name, handler) in guard.iter() {
            let timeout = Duration::from_secs(5); // Default 5s for post-compact
            let input_clone = input.clone();
            let handler = Arc::clone(handler);

            let out = match tokio::time::timeout(timeout, async {
                handler(input_clone)
            })
            .await
            {
                Ok(out) => out,
                Err(_) => {
                    eprintln!("[hook:{}] post-compact prelude timed out", name);
                    continue;
                }
            };

            // Only take the attachment from prelude hooks
            if !out.attachment.is_empty() {
                if result.attachment.is_empty() {
                    result.attachment = out.attachment;
                } else {
                    result.attachment.push_str("\n\n");
                    result.attachment.push_str(&out.attachment);
                }
            }
        }

        result
    }

    /// Execute post-compact epilogue hooks (run AFTER summary injection).
    /// These are mainly for side effects and notifications.
    pub async fn execute_post_compact_epilogue_hooks(
        &self,
        input: PostCompactInput,
    ) -> (PostCompactOutput, Option<String>) {
        let guard = self.post_compact_epilogue.lock().await;
        if guard.is_empty() {
            return (PostCompactOutput::default(), None);
        }

        let mut result = PostCompactOutput::default();
        let mut first_err: Option<String> = None;

        for (name, handler) in guard.iter() {
            let timeout = Duration::from_secs(5);
            let input_clone = input.clone();
            let handler = Arc::clone(handler);

            let out = match tokio::time::timeout(timeout, async {
                handler(input_clone)
            })
            .await
            {
                Ok(out) => out,
                Err(_) => {
                    let err_msg = format!("[hook:{}] timed out after 5s", name);
                    if first_err.is_none() {
                        first_err = Some(err_msg.clone());
                    }
                    result.user_message.push_str(&format!(
                        "\nPostCompact [hook:{}] timed out",
                        name
                    ));
                    continue;
                }
            };

            // Append UserMessage
            if !out.user_message.is_empty() {
                if result.user_message.is_empty() {
                    result.user_message = format!("PostCompact [hook:{}] completed: {}", name, out.user_message);
                } else {
                    result.user_message.push_str(&format!(
                        "\nPostCompact [hook:{}] completed: {}",
                        name, out.user_message
                    ));
                }
            }
        }

        (result, first_err)
    }

    /// Execute all post-compact hooks (prelude then epilogue).
    /// Returns merged output from all hooks.
    pub async fn execute_post_compact_hooks(
        &self,
        input: PostCompactInput,
    ) -> PostCompactOutput {
        let prelude_out = self.execute_post_compact_prelude_hooks(input.clone()).await;

        let (epilogue_out, _) = self.execute_post_compact_epilogue_hooks(input).await;

        // Merge outputs
        let mut result = prelude_out;
        if !epilogue_out.user_message.is_empty() {
            if result.user_message.is_empty() {
                result.user_message = epilogue_out.user_message;
            } else {
                result.user_message.push_str(&epilogue_out.user_message);
            }
        }

        result
    }

    /// Returns the number of registered hooks
    pub async fn hook_count(&self) -> usize {
        let pre = self.pre_compact_hooks.lock().await.len();
        let prelude = self.post_compact_prelude.lock().await.len();
        let epilogue = self.post_compact_epilogue.lock().await.len();
        pre + prelude + epilogue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hook_manager_empty() {
        let manager = HookManager::new();
        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_none());
        assert!(result.custom_instructions.is_empty());
        assert!(result.user_message.is_empty());
    }

    #[tokio::test]
    async fn test_pre_compact_hook_basic() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "test_hook",
            |input| PreCompactOutput {
                custom_instructions: format!("additional for trigger {:?}", input.trigger),
                user_message: "hook ran successfully".to_string(),
            },
            Duration::from_secs(5),
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: "original instructions".to_string(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_none());
        assert!(result.custom_instructions.contains("additional"));
        assert!(result.user_message.contains("hook ran successfully"));
    }

    #[tokio::test]
    async fn test_pre_compact_hook_merge_instructions() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "hook1",
            |_| PreCompactOutput {
                custom_instructions: "instruction 1".to_string(),
                user_message: String::new(),
            },
            Duration::from_secs(5),
        );

        manager.register_pre_compact(
            "hook2",
            |_| PreCompactOutput {
                custom_instructions: "instruction 2".to_string(),
                user_message: String::new(),
            },
            Duration::from_secs(5),
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Manual,
            custom_instructions: String::new(),
        };

        let (result, _) = manager.execute_pre_compact_hooks(input).await;
        assert!(result.custom_instructions.contains("instruction 1"));
        assert!(result.custom_instructions.contains("instruction 2"));
    }

    #[tokio::test]
    async fn test_post_compact_prelude_hook() {
        let manager = HookManager::new();

        manager.register_post_compact_prelude(
            "test_prelude",
            |input| PostCompactOutput {
                user_message: String::new(),
                attachment: format!("attachment from summary: {}",
                    &input.compact_summary.chars().take(50).collect::<String>()),
            },
        );

        let input = PostCompactInput {
            trigger: HookTrigger::Auto,
            compact_summary: "This is the full summary text".to_string(),
            recovered_files: vec![],
        };

        let result = manager.execute_post_compact_prelude_hooks(input).await;
        assert!(result.attachment.contains("attachment from summary"));
    }

    #[tokio::test]
    async fn test_post_compact_epilogue_hook() {
        let manager = HookManager::new();

        manager.register_post_compact_epilogue(
            "test_epilogue",
            |input| PostCompactOutput {
                user_message: format!("Notified about {} recovered files",
                    input.recovered_files.len()),
                attachment: String::new(),
            },
        );

        let input = PostCompactInput {
            trigger: HookTrigger::SmCompact,
            compact_summary: "Summary".to_string(),
            recovered_files: vec!["file1.rs".to_string(), "file2.rs".to_string()],
        };

        let (result, _) = manager.execute_post_compact_epilogue_hooks(input).await;
        assert!(result.user_message.contains("Notified about 2 recovered files"));
    }

    #[tokio::test]
    async fn test_hook_timeout() {
        let manager = HookManager::new();

        // Register a hook that takes too long
        manager.register_pre_compact(
            "slow_hook",
            |_| async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                PreCompactOutput::default()
            },
            Duration::from_millis(50), // Very short timeout
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_some()); // Should have timed out
        assert!(result.user_message.contains("timed out"));
    }

    #[tokio::test]
    async fn test_multiple_hooks_first_error_tracked() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "failing_hook",
            |_| PreCompactOutput {
                custom_instructions: String::new(),
                user_message: "first".to_string(),
            },
            Duration::from_secs(5),
        );

        manager.register_pre_compact(
            "slow_hook",
            |_| async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                PreCompactOutput::default()
            },
            Duration::from_millis(50),
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Auto,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_some());
        assert!(result.user_message.contains("first"));
        assert!(result.user_message.contains("timed out"));
    }

    #[tokio::test]
    async fn test_default_timeout() {
        let manager = HookManager::new();

        manager.register_pre_compact(
            "no_timeout_specified",
            |input| PreCompactOutput {
                custom_instructions: format!("got input with trigger {:?}", input.trigger),
                user_message: String::new(),
            },
            Duration::ZERO, // Should use default 5s
        );

        let input = PreCompactInput {
            trigger: HookTrigger::Manual,
            custom_instructions: String::new(),
        };

        let (result, err) = manager.execute_pre_compact_hooks(input).await;
        assert!(err.is_none());
        assert!(result.custom_instructions.contains("Manual"));
    }
}
