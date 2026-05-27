// Core modules (always compiled)
pub mod agent_loop;
pub mod agent_memory_updater;
pub mod agent_profile;
pub mod agent_progress;
pub mod agent_sub;
pub mod adaptive_prompt;
pub mod auto_classifier;
pub mod beta_headers;
pub mod cache_break_detection;
pub mod cache_metrics;
pub mod cache_optimizer;
pub mod compact;
pub mod config;
pub mod consecutive_failure_tracker;
pub mod context;
pub mod context_references;
pub mod cost_tracker;
pub mod error_types;
pub mod file_lock;
pub mod json_utils;
pub mod memoize;
pub mod model_aliases;
pub mod model_capabilities;
pub mod normalize;
pub mod permissions;
pub mod process_manager;
pub mod prompt_caching;
pub mod rate_limit;
pub mod reasoning_pad;
pub mod reasoning_retention;
pub mod redundant_call_detector;
pub mod retry_utils;
pub mod storm_breaker;
pub mod status;
pub mod streaming;
pub mod streaming_executor;
pub mod string_utils;
pub mod system_prompt;
pub mod tool_list_fingerprint;
pub mod tool_scavenge;
pub mod tokens;
pub mod truncate_utils;
pub mod utf8_sanitize;
pub mod utils;
pub mod tools;
pub mod slice_ansi;

// Optional modules - feature-gated
// Currently compiled as hybrid (always present) for build compatibility.
// To make a feature truly optional:
//   1. Keep only #[cfg(feature = "...")] (remove the #[cfg(not(...))])
//   2. Add #[cfg] guards to all downstream code that uses the module types
//   3. Verify: cargo build --no-default-features

#[cfg(feature = "feature-cron")]
pub mod cron;
#[cfg(not(feature = "feature-cron"))]
pub mod cron;

#[cfg(feature = "feature-mcp")]
pub mod mcp;
#[cfg(not(feature = "feature-mcp"))]
pub mod mcp;

#[cfg(feature = "feature-skills")]
pub mod skills;
#[cfg(not(feature = "feature-skills"))]
pub mod skills;

#[cfg(feature = "feature-filehistory")]
pub mod filehistory;
#[cfg(not(feature = "feature-filehistory"))]
pub mod filehistory;

#[cfg(feature = "feature-session-memory")]
pub mod session_memory;
#[cfg(not(feature = "feature-session-memory"))]
pub mod session_memory;

#[cfg(feature = "feature-session-persistence")]
pub mod session_persistence;
#[cfg(not(feature = "feature-session-persistence"))]
pub mod session_persistence;

#[cfg(feature = "feature-work-task")]
pub mod work_task;
#[cfg(not(feature = "feature-work-task"))]
pub mod work_task;

#[cfg(feature = "feature-telemetry")]
pub mod telemetry;
#[cfg(not(feature = "feature-telemetry"))]
pub mod telemetry;

#[cfg(feature = "feature-error-reporter")]
pub mod error_reporter;
#[cfg(not(feature = "feature-error-reporter"))]
pub mod error_reporter;

#[cfg(feature = "feature-claudemd")]
pub mod claudemd;
#[cfg(not(feature = "feature-claudemd"))]
pub mod claudemd;

#[cfg(feature = "feature-cleanup")]
pub mod cleanup;
#[cfg(not(feature = "feature-cleanup"))]
pub mod cleanup;

#[cfg(feature = "feature-settings")]
pub mod multi_settings;
#[cfg(not(feature = "feature-settings"))]
pub mod multi_settings;

#[cfg(feature = "feature-semver")]
pub mod semver;
#[cfg(not(feature = "feature-semver"))]
pub mod semver;

#[cfg(feature = "feature-proactive-budget")]
pub mod proactive_budget;
#[cfg(not(feature = "feature-proactive-budget"))]
pub mod proactive_budget;

#[cfg(feature = "feature-transcript")]
pub mod transcript;
#[cfg(not(feature = "feature-transcript"))]
pub mod transcript;
pub mod transcript_builder;

#[cfg(feature = "feature-todo")]
pub mod todo_reminder;
#[cfg(not(feature = "feature-todo"))]
pub mod todo_reminder;

#[cfg(feature = "feature-task")]
pub mod task_store;
#[cfg(not(feature = "feature-task"))]
pub mod task_store;