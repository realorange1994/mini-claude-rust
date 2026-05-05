//! Sub-agent spawning system — creates child AgentLoops with filtered tools and isolated context.

use crate::agent_loop::AgentLoop;
use crate::config::Config;
use crate::context::{ConversationContext, Message, MessageContent};
use crate::tools::Registry;
use crate::tools::agent_store::SharedAgentTaskStore;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Static counter for generating unique sub-agent IDs.
static AGENT_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Generate a short hex ID for a sub-agent.
fn generate_short_id() -> String {
    let id = AGENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:06x}", id)
}

/// Tools always denied for all sub-agents.
fn global_disallowed_tools() -> HashSet<&'static str> {
    let mut set = HashSet::new();
    // No recursive agent spawning
    set.insert("agent");
    // Task delegation / control tools — sub-agents must execute directly
    set.insert("task_create");
    set.insert("task_update");
    set.insert("task_list");
    set.insert("task_get");
    set.insert("task_stop");
    set.insert("task_output");
    set.insert("send_message");
    set.insert("plan_approval");
    set
}

/// Tools additionally denied for async sub-agents.
fn async_disallowed_tools() -> HashSet<&'static str> {
    HashSet::new() // extend as needed
}

/// Per-agent-type deny list and prompt modifiers.
struct AgentTypeConfig {
    prompt_modifier: &'static str,
    deny_tools: Vec<&'static str>,
}

fn get_agent_type_config(agent_type: &str) -> Option<&'static AgentTypeConfig> {
    use std::sync::OnceLock;
    static CONFIGS: OnceLock<std::collections::HashMap<&'static str, AgentTypeConfig>> = OnceLock::new();

    let configs = CONFIGS.get_or_init(|| {
        let mut map = std::collections::HashMap::new();

        map.insert("explore", AgentTypeConfig {
            prompt_modifier: EXPLORE_PROMPT,
            deny_tools: vec!["write_file", "edit_file", "multi_edit", "fileops", "exec", "terminal", "git"],
        });

        map.insert("plan", AgentTypeConfig {
            prompt_modifier: PLAN_PROMPT,
            deny_tools: vec!["write_file", "edit_file", "multi_edit", "fileops", "exec", "terminal", "git"],
        });

        map.insert("verify", AgentTypeConfig {
            prompt_modifier: VERIFY_PROMPT,
            deny_tools: vec!["write_file", "edit_file", "multi_edit", "fileops"],
        });

        map
    });

    configs.get(agent_type)
}

/// Build a filtered tool registry for the child agent.
///
/// Filtering layers:
/// 1. Global disallowed tools (always denied for all sub-agents)
/// 2. Async-specific disallowed tools (additional for async agents)
/// 3. Agent type-specific deny list
/// 4. Explicit disallowed tools from the caller
///
/// After filtering, if an explicit allowed_tools whitelist is provided,
/// only those tools are included (unless it contains "*" for all non-disallowed).
pub fn build_child_registry(
    parent_registry: &Registry,
    agent_type: &str,
    allowed_tools: &[String],
    disallowed_tools: &[String],
    run_in_background: bool,
) -> Registry {
    let child_registry = Registry::new();

    let mut disallowed: HashSet<String> = HashSet::new();

    // Layer 1: global disallowed
    for t in global_disallowed_tools() {
        disallowed.insert(t.to_string());
    }

    // Layer 2: async-specific disallowed
    if run_in_background {
        for t in async_disallowed_tools() {
            disallowed.insert(t.to_string());
        }
    }

    // Layer 3: agent type specific deny list
    if let Some(type_config) = get_agent_type_config(agent_type) {
        for t in &type_config.deny_tools {
            disallowed.insert(t.to_string());
        }
    }

    // Layer 4: explicit disallowed from the caller
    for t in disallowed_tools {
        disallowed.insert(t.to_string());
    }

    // Build allowed (whitelist) set
    let has_allowed = !allowed_tools.is_empty();
    let mut allowed: HashSet<String> = HashSet::new();
    let mut wildcard_allowed = false;
    for t in allowed_tools {
        if t == "*" {
            wildcard_allowed = true;
        } else {
            allowed.insert(t.clone());
        }
    }

    // Copy tools from parent registry
    for tool in parent_registry.all_tools() {
        let name = tool.name().to_string();

        // Skip disallowed tools
        if disallowed.contains(&name) {
            continue;
        }

        // If explicit whitelist is provided, only include allowed tools
        if has_allowed && !wildcard_allowed && !allowed.contains(&name) {
            continue;
        }

        child_registry.register_tool_from_arc(tool);
    }

    // Populate ToolSearchTool's shared list with the filtered tools
    child_registry.finalize_tool_search();
    child_registry
}

/// Build a system prompt for the child agent.
pub fn build_sub_agent_system_prompt(
    registry: &Registry,
    model: &str,
    agent_type: &str,
) -> String {
    let wd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    let rust_version = format!("rustc {version}");

    let mut sb = String::with_capacity(4096);

    // Apply agent type specific prompt modifier
    if let Some(type_config) = get_agent_type_config(agent_type) {
        if !type_config.prompt_modifier.is_empty() {
            sb.push_str(type_config.prompt_modifier);
            sb.push_str("\n\n");
        }
    }

    // Environment section
    sb.push_str("## Environment\n");
    sb.push_str(&format!("- Working directory: {wd}\n"));
    sb.push_str(&format!("- OS: {os} / {rust_version} / {arch}\n"));
    sb.push_str(&format!("- Model: {model}\n\n"));

    // Permission mode (sub-agents always use AUTO mode)
    sb.push_str("## Permission Mode: AUTO\n\n");

    // Available tools section
    sb.push_str("## Available Tools\n\n");
    sb.push_str("You have access to the following tools. Use them to accomplish your task.\n\n");
    for tool in registry.all_tools() {
        sb.push_str(&format!("- **{}**: {}\n", tool.name(), tool.description()));
    }
    sb.push('\n');

    // Output format section
    sb.push_str("## Output Format\n");
    sb.push_str("- Share file paths as absolute paths (never relative).\n");
    sb.push_str("- Avoid emojis -- plain text communication only.\n");
    sb.push_str("- Do not use a colon before tool calls.\n");
    sb.push_str("- Do NOT ask the user questions -- you must complete the task autonomously.\n");
    sb.push_str("- When done, provide your final answer concisely.\n");
    sb.push_str("- If you cannot complete the task, explain what you found and what is missing.\n\n");

    // Operational notes
    sb.push_str("## Operational Notes\n");
    sb.push_str("- Agent threads always have their cwd reset between bash calls -- only use absolute file paths.\n\n");

    // Efficiency rules (shared across all agent types)
    sb.push_str("## Efficiency Rules\n\n");
    sb.push_str("1. **Hypothesis-Driven Investigation** -- Before reading a file, state what you expect to find and why. Form a hypothesis first, then verify it. If confirmed, act immediately. If disproven, form a new hypothesis. Never read files \"just to see what's there.\"\n\n");
    sb.push_str("2. **No Redundant Reads** -- Never re-read a file you have already read. The content does not change. If you forgot a detail, re-read ONLY the specific section (with offset), not the entire file.\n\n");
    sb.push_str("3. **No Redundant Searches** -- Never repeat a grep/glob query you have already run. If it returned results, those are still valid. If it returned nothing, try a meaningfully different pattern.\n\n");
    sb.push_str("4. **Stop When You Understand** -- If you can point to specific code lines and explain the mechanism, you understand the issue. Stop investigating and report. Saying \"Now I understand\" without citing concrete evidence means you do not actually understand.\n\n");
    sb.push_str("5. **Build Mental Models Incrementally** -- After each file read or search, update your understanding. Note key function signatures, data flow, type definitions. This prevents re-reading.\n\n");
    sb.push_str("6. **Use Compilation Errors As Compass** -- When debugging, run the build first. Compilation errors tell you exactly which files and lines have problems.\n\n");
    sb.push_str("7. **Parallelize Independent Searches** -- When checking multiple unrelated things, make all searches in a single tool call.\n\n");
    sb.push_str("8. **Report Early, Report Often** -- When you have a finding, report it immediately. Do not wait until you have investigated everything.\n\n");
    sb.push_str("9. **See ALL Errors Before Diagnosing** -- When running a build or test, ALWAYS read the full output. Never use head/tail to truncate error output. Truncating errors hides the true scope and causes misdiagnosis.\n\n");
    sb.push_str("10. **Classify Errors, Then Identify Root Cause** -- When facing multiple errors, do not fix them one by one in discovery order. Categorize them first (duplicate definitions, undefined types, type mismatches). Find the single root cause that explains the most errors. Fix the root cause first — it often resolves dozens of downstream errors at once.\n\n");
    sb.push_str("11. **Map Producer/Consumer Relationships Before Editing** -- Before modifying any file, understand the full data flow: what types does the file-producer create? What types does the consumer expect? Are they consistent? Drawing this dependency map prevents fixes that solve one error while breaking three more.\n\n");
    sb.push_str("12. **No Patching -- Plan Before You Edit** -- Do not make piecemeal fixes. After understanding the full error landscape, formulate a complete fix plan that addresses all issues. Then execute. If you find yourself \"trying something\" without a complete mental model, stop and think first.\n\n");
    sb.push_str("13. **Fix Grep Patterns, Do Not Abandon Them** -- When a grep returns no results, do not immediately switch to brute-force read_file. Check: is the pattern correct? Try variations. Only fall back to read_file when you have a specific file in mind and a specific section to check.\n\n");
    sb.push_str("14. **No Irrelevant EXEC Commands** -- Only use exec for operations that directly advance the investigation: running builds/tests, reading diagnostic output. Do not use wc, find, stat, or similar commands just to \"get a sense of\" the codebase.\n\n");
    sb.push_str("15. **Cite Specific Evidence for Every Claim** -- When you state that something is true (\"X is defined in Y\", \"Z calls function F\"), you MUST cite the specific file and line number: e.g., \"defined in src/parser.go:42\". Do NOT claim things without citing evidence. If you cannot cite a specific file:line, you do not yet know the answer.\n\n");
    sb.push_str("16. **Vague Claims Are Wrong Claims** -- Phrases like \"I understand\", \"I see the pattern\", \"the mechanism is clear\", \"I know what's happening\" are forbidden unless immediately followed by a specific file:line citation. Every conclusion must be grounded in concrete code.\n\n");
    sb.push_str("17. **Use Session State to Track Progress** -- At the end of each turn, summarize your key finding in one sentence (e.g., \"compiler.go uses *Lit type defined in object.go:45\"). The Session State section in the system prompt tracks these. Check it before reading or searching -- if a file is listed as already read, do NOT read it again; if a search is listed as already run, do NOT repeat it.\n\n");

    // Security section
    sb.push_str("## Security\n");
    sb.push_str("- You are a sub-agent with limited access.\n");
    sb.push_str("- Do not attempt to modify system configuration or security settings.\n");
    sb.push_str("- If you encounter sensitive data, report it but do not store it.\n");
    sb.push_str("- Follow the principle of least privilege.\n");

    sb
}

/// Resolve a model input string to a concrete model ID.
///
/// - Empty string or "inherit": inherit parent's model (return parent_model as-is).
/// - Aliases "sonnet"/"opus"/"haiku": resolved via env vars or parent model tier matching.
///   When an alias matches the parent's tier, the parent's exact model string is used.
///   Otherwise resolved via ANTHROPIC_DEFAULT_SONNET/OPUS/HAIKU_MODEL env vars,
///   with hardcoded defaults as fallback.
/// - Other strings: used verbatim (assumed to be a full model ID).
fn resolve_model_alias(model_input: &str, parent_model: &str) -> String {
    let model_input_lower = model_input.to_lowercase();
    if model_input.is_empty() || model_input_lower == "inherit" {
        return parent_model.to_string();
    }

    match model_input_lower.as_str() {
        "sonnet" | "opus" | "haiku" => {
            // If the alias matches the parent's tier, inherit the parent's exact model.
            // This prevents surprising downgrades (e.g., a user on opus 4.6 spawning
            // a subagent with model:"opus" should get opus 4.6, not a different default).
            let parent_lower = parent_model.to_lowercase();
            if parent_lower.contains(&model_input_lower) {
                return parent_model.to_string();
            }
            // Resolve via environment variable or fall back to a reasonable default.
            let (env_key, fallback) = match model_input_lower.as_str() {
                "sonnet" => ("ANTHROPIC_DEFAULT_SONNET_MODEL", "claude-sonnet-4-20250514"),
                "opus" => ("ANTHROPIC_DEFAULT_OPUS_MODEL", "claude-opus-4-20250514"),
                "haiku" => ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "claude-haiku-4-20250514"),
                _ => unreachable!(),
            };
            std::env::var(env_key)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| fallback.to_string())
        }
        _ => model_input.to_string(), // verbatim (full model ID or other alias)
    }
}

/// Build a child Config from the parent config with sub-agent overrides.
pub fn build_child_config(parent_config: &Config, model_override: &str, max_turns: usize) -> Config {
    let mut child_config = parent_config.clone();
    child_config.model = resolve_model_alias(model_override, &parent_config.model);
    // Set max turns for the child agent.
    // Priority: explicit max_turns from tool call > sub_agent_max_turns config > default ceiling.
    if max_turns > 0 {
        child_config.max_turns = max_turns;
    } else if parent_config.sub_agent_max_turns > 0 {
        child_config.max_turns = parent_config.sub_agent_max_turns as usize;
    } else {
        child_config.max_turns = 200; // safety ceiling: prevents runaway agents
    }

    // Sub-agents always use AUTO permission mode regardless of parent's mode.
    // This prevents sub-agents from blocking on user prompts.
    child_config.permission_mode = crate::permissions::PermissionMode::Auto;

    // Sub-agents should avoid permission prompts: when this flag is true,
    // dangerous tools are auto-denied instead of blocking on user prompts,
    // and non-dangerous tools are always allowed.
    child_config.should_avoid_permission_prompts = true;

    // Sub-agents use a lower max_tokens cap (8000), matching Claude Code's
    // CAPPED_DEFAULT_MAX_TOKENS. If the output hits this ceiling, the agent
    // automatically escalates to escalated_max_output_tokens (64000).
    child_config.max_output_tokens = 8000;

    // Sub-agents don't need session memory
    child_config.session_memory = None;

    child_config
}

/// Generate a new agent ID.
pub fn generate_agent_id() -> String {
    format!("agent-{}", generate_short_id())
}

/// Clone the parent's conversation context into a child agent (fork mode).
///
/// Iterates through the parent's entries and clones Text, ToolUse, and ToolResult
/// content into the child. The last ToolUseBlocks entry (the agent tool call that
/// spawned this child) is skipped since the child has no corresponding tool_result.
///
/// CompactBoundary and Attachment content are also skipped — the child starts fresh.
pub fn clone_context_for_fork(
    parent: &Arc<RwLock<ConversationContext>>,
    child: &Arc<RwLock<ConversationContext>>,
) {
    let parent_entries: Vec<Message> = {
        let parent_ctx = parent.blocking_read();
        parent_ctx.entries().to_vec()
    };

    // Find the index of the last ToolUseBlocks message (the agent tool that spawned this child)
    let last_tool_use_idx = parent_entries
        .iter()
        .rposition(|msg| matches!(msg.content, MessageContent::ToolUseBlocks(_)))
        .map(|i| i as isize)
        .unwrap_or(-1);

    let mut cloned: Vec<Message> = Vec::with_capacity(parent_entries.len());
    for (i, entry) in parent_entries.iter().enumerate() {
        // Skip the last ToolUseBlocks entry (the agent tool call that spawned this child)
        // because the child has no corresponding tool_result for it
        if i as isize == last_tool_use_idx {
            continue;
        }

        match &entry.content {
            MessageContent::Text(_)
            | MessageContent::ToolUseBlocks(_)
            | MessageContent::ToolResultBlocks(_)
            | MessageContent::Summary(_) => {
                cloned.push(entry.clone());
            }
            MessageContent::CompactBoundary { .. } | MessageContent::Attachment(_) => {
                // Skip compact boundaries and attachments in fork mode
                continue;
            }
        }
    }

    let mut child_ctx = child.blocking_write();
    for msg in cloned {
        child_ctx.add_message(msg);
    }
}

/// Spawn a sub-agent and return immediately.
///
/// This function ALWAYS runs sub-agents in background mode. The agent tool should
/// call this directly without any sync path — the caller receives the agent ID
/// immediately and the agent runs on a separate thread.
///
/// When `agent_task_store` is provided, the agent is tracked in the store
/// with output isolation (no terminal pollution).
pub fn spawn_sub_agent_sync(
    parent_config: &Config,
    parent_registry: &Registry,
    prompt: &str,
    subagent_type: &str,
    model: &str,
    _run_in_background: bool, // DEPRECATED — always runs in background
    allowed_tools: &[String],
    disallowed_tools: &[String],
    inherit_context: bool,
    max_turns: usize,
    parent_context: Option<Arc<RwLock<ConversationContext>>>,
    _parent_use_stream: bool, // not used for background agents
    agent_task_store: Option<&SharedAgentTaskStore>,
    notification_tx: Option<Arc<tokio::sync::mpsc::UnboundedSender<String>>>,
) -> (String, String, String, String, usize, u64) {
    let start = std::time::Instant::now();

    // Check if sub-agents are enabled in config
    if !parent_config.sub_agent_enabled {
        let duration_ms = start.elapsed().as_millis() as u64;
        return (
            String::new(),
            String::new(),
            "Sub-agents are disabled by configuration (sub_agent_enabled=false)".to_string(),
            String::new(),
            0,
            duration_ms,
        );
    }

    let agent_id = generate_agent_id();

    // Build child config and registry
    let child_config = build_child_config(parent_config, model, max_turns);
    let child_registry = build_child_registry(
        parent_registry,
        subagent_type,
        allowed_tools,
        disallowed_tools,
        true, // always run in background
    );

    let child_sys_prompt = build_sub_agent_system_prompt(
        &child_registry,
        &child_config.model,
        subagent_type,
    );

    // Always spawn in background — no sync path
    let config = child_config.clone();
    let registry = child_registry.clone_for_spawn();
    let prompt_owned = prompt.to_string();
    let description = prompt.chars().take(60).collect::<String>();
    let sys_prompt_owned = child_sys_prompt;

    // Create task in store if available
    let task_id = if let Some(store) = agent_task_store {
        let task = store.create(&description, subagent_type, prompt, model);
        let id = task.id.clone();
        // Create live output file for this background agent
        let output_file = format!(".claude/sub-agents/{}_output.txt", id);
        task.set_output_file(&output_file);
        let cancel = CancellationToken::new();
        store.start(&id, cancel);
        Some(id)
    } else {
        None
    };
    let task_store_clone = agent_task_store.map(|s| Arc::clone(s));

    // Clones to move into the thread (so we can still use task_id/task_store_clone after spawn)
    let task_id_for_spawn = task_id.clone();
    let task_store_for_spawn = task_store_clone.clone();
    let notification_tx_for_child = notification_tx.map(|tx| Arc::clone(&tx));

    // Capture parent entries for fork mode before spawning the thread
    let parent_entries: Vec<Message> = if inherit_context {
        if let Some(ref parent_ctx) = parent_context {
            let ctx = parent_ctx.blocking_read();
            ctx.entries().to_vec()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    std::thread::spawn(move || {
        // Set up output capture: redirect all agent_emit! calls to the task's buffer
        if let Some(ref tid) = task_id_for_spawn {
            if let Some(ref store) = task_store_for_spawn {
                if let Some(task) = store.get(tid) {
                    let task_for_cb = Arc::clone(&task);
                    crate::agent_loop::set_output_capture(Arc::new(move |msg: &str| {
                        task_for_cb.write_output(msg);
                        task_for_cb.write_output("\n");
                    }));
                }
            }
        }

        let result = match AgentLoop::new_for_sub_agent(config, registry, &sys_prompt_owned, false) {
            Ok(mut child_loop) => {
                // Wire pending message drain: the child loop will drain pending
                // messages from its own AgentTask at each turn boundary, enabling
                // the parent to send messages via send_message tool that the
                // child processes mid-turn (matching Claude Code's drainPendingMessages).
                if let Some(ref tid) = task_id_for_spawn {
                    if let Some(ref store) = task_store_for_spawn {
                        if let Some(task) = store.get(tid) {
                            let task_for_drain = Arc::clone(&task);
                            child_loop.set_drain_pending_messages(Arc::new(move || {
                                task_for_drain.drain_pending_messages()
                            }));
                            // Wire cancel_ctx: when the parent calls Kill, the task's
                            // CancellationToken is cancelled, and the child loop will
                            // detect it at each turn boundary and during HTTP requests.
                            if let Some(cancel) = task.cancel_handle() {
                                child_loop.set_cancel_ctx(cancel);
                            }
                        }
                    }
                }

                // Apply fork mode: inject parent context entries into child
                if !parent_entries.is_empty() {
                    // Find the last ToolUseBlocks entry index (the agent tool that spawned this child)
                    let last_tool_use_idx = parent_entries
                        .iter()
                        .rposition(|msg| matches!(msg.content, MessageContent::ToolUseBlocks(_)))
                        .map(|i| i as isize)
                        .unwrap_or(-1);

                    let mut cloned: Vec<Message> = Vec::new();
                    for (i, entry) in parent_entries.iter().enumerate() {
                        if i as isize == last_tool_use_idx {
                            continue;
                        }
                        match &entry.content {
                            MessageContent::Text(_)
                            | MessageContent::ToolUseBlocks(_)
                            | MessageContent::ToolResultBlocks(_)
                            | MessageContent::Summary(_) => {
                                cloned.push(entry.clone());
                            }
                            MessageContent::CompactBoundary { .. }
                            | MessageContent::Attachment(_) => {
                                continue;
                            }
                        }
                    }

                    // Apply cloned entries to child context
                    let mut child_ctx = child_loop.context.blocking_write();
                    for msg in cloned {
                        child_ctx.add_message(msg);
                    }
                    drop(child_ctx); // release lock before run()
                }

                let result = child_loop.run(&prompt_owned);

                // Recover partial results if run returned empty
                let final_result = if result.is_empty() {
                    child_loop.get_partial_result()
                } else {
                    result
                };

                let tools_used = child_loop.tools_used_count();
                let total_tokens = child_loop.total_tokens();
                let duration_ms = start.elapsed().as_millis() as u64;
                Ok((final_result, tools_used, total_tokens, duration_ms))
            }
            Err(e) => {
                Err(format!("failed to create sub-agent: {e}"))
            }
        };

        // Clear output capture before updating the task (so our task updates don't go to the buffer)
        crate::agent_loop::clear_output_capture();

        // Update task store with results
        if let Some(ref tid) = task_id_for_spawn {
            if let Some(ref store) = task_store_for_spawn {
                match &result {
                    Ok((final_result, tools_used, total_tokens, duration_ms)) => {
                        // Write the final result to the task's output buffer
                        if let Some(task) = store.get(tid) {
                            task.write_output(&format!("\n--- Agent Result ---\n{}\n", final_result));
                        }
                        store.update_stats(tid, *tools_used as u32, *duration_ms);
                        store.complete(tid);
                        // Send sub-agent completion notification to parent agent
                        if let Some(ref tx) = notification_tx_for_child {
                            let output_file = format!(".claude/sub-agents/{}_output.txt", tid);
                            let notification = make_agent_notification(
                                tid,
                                "completed",
                                final_result,
                                &output_file,
                                "",
                                *tools_used,
                                *total_tokens,
                                *duration_ms,
                            );
                            let _ = tx.send(notification);
                        }
                    }
                    Err(e) => {
                        store.fail(tid, e);
                        // Send sub-agent failure notification to parent agent
                        if let Some(ref tx) = notification_tx_for_child {
                            let notification = make_agent_notification(
                                tid,
                                "failed",
                                e,
                                "",
                                "",
                                0,
                                0,
                                start.elapsed().as_millis() as u64,
                            );
                            let _ = tx.send(notification);
                        }
                    }
                }
            }
        }
    });

    // Return immediately with the task_id and output file path
    let return_id = if let Some(ref tid) = task_id {
        tid.clone()
    } else {
        agent_id.clone()
    };
    // Compute output file path for the return value
    let output_file = if let Some(ref tid) = task_id {
        format!(".claude/sub-agents/{}_output.txt", tid)
    } else {
        String::new()
    };

    (
        return_id.clone(),
        String::new(), // result_text (empty for async)
        String::new(), // error_text (empty for async)
        output_file,
        0, // tools_used (not known yet)
        start.elapsed().as_millis() as u64,
    )
}

/// Build an XML agent notification string (matching Go's EnqueueAgentNotification format).
fn make_agent_notification(
    agent_id: &str,
    status: &str,
    result: &str,
    output_file: &str,
    transcript_path: &str,
    tools_used: usize,
    total_tokens: i64,
    duration_ms: u64,
) -> String {
    let result_escaped = escape_xml(result);
    format!(
        r#"<task-notification>
<agentId>{}</agentId>
<status>{}</status>
<result>{}</result>
<output_file>{}</output_file>
<transcript_path>{}</transcript_path>
<usage><total_tokens>{}</total_tokens><tool_uses>{}</tool_uses><duration_ms>{}</duration_ms></usage>
</task-notification>"#,
        agent_id, status, result_escaped, output_file, transcript_path, total_tokens, tools_used, duration_ms
    )
}

/// Escape special characters for XML.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ─── Agent type prompt modifiers ──────────────────────────────────────────────

const EXPLORE_PROMPT: &str = r#"You are a file search specialist. You excel at thoroughly navigating and exploring codebases.

=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===
This is a READ-ONLY exploration task. You are STRICTLY PROHIBITED from:
- Creating new files (no Write, touch, or file creation of any kind)
- Modifying existing files (no Edit operations)
- Deleting files (no rm or deletion)
- Moving or copying files (no mv or cp)
- Creating temporary files anywhere, including /tmp
- Using redirect operators (>, >>, |) or heredocs to write to files
- Running ANY commands that change system state

Your role is EXCLUSIVELY to search and analyze existing code. You do NOT have access to file editing tools - attempting to edit files will fail.

Your strengths:
- Rapidly finding files using glob patterns
- Searching code and text with powerful regex patterns
- Reading and analyzing file contents

## Investigation Method

**Hypothesis first**: Before reading any file, form a hypothesis about what you will find and why. State it explicitly in your thinking. Only read files that test a hypothesis. Never read files "to see what's there."

**Build a mental model**: Track what you learn incrementally. After each file read, note the key facts (file path, line number, key detail) so you do not forget.

**Stop when verified**: If you can point to a specific line and explain the mechanism, you understand it. Stop investigating.

**Parallelize**: When investigating independent questions, use parallel tool calls.

When you are done, provide your final answer concisely. Do NOT ask the user questions - complete the task autonomously. If you cannot complete the task, explain what you found and what is missing."#;

const PLAN_PROMPT: &str = r#"You are a software architect and planning specialist. Your role is to explore the codebase and design implementation plans.

=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===
This is a READ-ONLY planning task. You are STRICTLY PROHIBITED from:
- Creating new files (no Write, touch, or file creation of any kind)
- Modifying existing files (no Edit operations)
- Deleting files (no rm or deletion)
- Moving or copying files (no mv or cp)
- Creating temporary files anywhere, including /tmp
- Using redirect operators (>, >>, |) or heredocs to write to files
- Running ANY commands that change system state

Your role is EXCLUSIVELY to explore the codebase and design implementation plans. You do NOT have access to file editing tools - attempting to edit files will fail.

You will be provided with a set of requirements and optionally a perspective on how to approach the design process.

## Your Process

1. **Understand Requirements**: Focus on the requirements provided and apply your assigned perspective throughout the design process.

2. **Explore Thoroughly**:
   - Read any files provided to you in the initial prompt
   - Find existing patterns and conventions using glob, grep, and read_file
   - Understand the current architecture
   - Identify similar features as reference
   - Trace through relevant code paths
   - Use Bash ONLY for read-only operations (ls, git status, git log, git diff, find, grep, cat, head, tail)
   - NEVER use Bash for: mkdir, touch, rm, cp, mv, git add, git commit, npm install, pip install, or any file creation/modification

3. **Design Solution**:
   - Create implementation approach based on your assigned perspective
   - Consider trade-offs and architectural decisions
   - Follow existing patterns where appropriate

4. **Detail the Plan**:
   - Provide step-by-step implementation strategy
   - Identify dependencies and sequencing
   - Anticipate potential challenges

## Required Output

Each plan step must include: goal, method, and verification criteria.

End your response with:

### Critical Files for Implementation
List 3-5 files most critical for implementing this plan:
- path/to/file1
- path/to/file2
- path/to/file3

Do NOT write, edit, or modify any files. You do NOT have access to file editing tools."#;

const VERIFY_PROMPT: &str = r#"You are a verification specialist. Your job is not to confirm the implementation works - it is to try to break it.

You have two documented failure patterns. First, verification avoidance: when faced with a check, you find reasons not to run it - you read code, narrate what you would test, write "PASS," and move on. Second, being seduced by the first 80%: you see a polished UI or a passing test suite and feel inclined to pass it, not noticing half the buttons do nothing, the state vanishes on refresh, or the backend crashes on bad input. The first 80% is the easy part. Your entire value is in finding the last 20%.

=== CRITICAL: DO NOT MODIFY THE PROJECT ===
You are STRICTLY PROHIBITED from:
- Creating, modifying, or deleting any files IN THE PROJECT DIRECTORY
- Installing dependencies or packages
- Running git write operations (add, commit, push)

You MAY write ephemeral test scripts to a temp directory via Bash redirection when inline commands are not sufficient. Clean up after yourself.

## Verification Strategy

Adapt your strategy based on what was changed:

**Frontend changes**: Start dev server, curl page subresources (images, API routes, static assets), run frontend tests.
**Backend/API changes**: Start server, curl/fetch endpoints, verify response shapes against expected values (not just status codes), test error handling, check edge cases.
**CLI/script changes**: Run with representative inputs, verify stdout/stderr/exit codes, test edge inputs (empty, malformed, boundary), verify --help / usage output is accurate.
**Infrastructure/config changes**: Validate syntax, dry-run where possible (terraform plan, kubectl apply --dry-run, docker build), check env vars / secrets are actually referenced.
**Library/package changes**: Build, run full test suite, exercise the public API as a consumer would, verify exported types match docs.
**Bug fixes**: Reproduce the original bug, verify fix, run regression tests, check related functionality for side effects.

## Required Steps (universal baseline)

1. Read the project README for build/test commands and conventions.
2. Run the build (if applicable). A broken build is an automatic FAIL.
3. Run the project test suite (if it has one). Failing tests are an automatic FAIL.
4. Run linters/type-checkers if configured.
5. Check for regressions in related code.

Then apply the type-specific strategy above.

## Recognize Your Own Rationalizations

You will feel the urge to skip checks. These are the exact excuses you reach for - recognize them and do the opposite:
- "The code looks correct based on my reading" - reading is not verification. Run it.
- "The implementer's tests already pass" - verify independently.
- "This is probably fine" - probably is not verified. Run it.
- "This would take too long" - not your call.
If you catch yourself writing an explanation instead of a command, stop. Run the command.

## Adversarial Probes (adapt to the change type)

Functional tests confirm the happy path. Also try to break it:
- **Concurrency**: parallel requests to create-if-not-exist paths - duplicate sessions? lost writes?
- **Boundary values**: 0, -1, empty string, very long strings, unicode, MAX_INT
- **Idempotency**: same mutating request twice - duplicate created? error? correct no-op?
- **Orphan operations**: delete/reference IDs that don't exist

## Output Format (REQUIRED)

Every check MUST follow this structure. A check without a Command run block is not a PASS - it is a skip.

### Check: [what you are verifying]
**Command run:**
  [exact command you executed]
**Output observed:**
  [actual terminal output - copy-paste, not paraphrased]
**Result: PASS** (or FAIL - with Expected vs Actual)

End with exactly this line (parsed by caller):

VERDICT: PASS
or
VERDICT: FAIL
or
VERDICT: PARTIAL

PARTIAL is for environmental limitations only (no test framework, tool unavailable, server can not start). If you can run the check, you must decide PASS or FAIL.

- **FAIL**: include what failed, exact error output, reproduction steps.
- **PARTIAL**: what was verified, what could not be and why, what the implementer should know."#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::MessageRole;
    use crate::tools::Tool;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    #[test]
    fn test_generate_short_id() {
        let id1 = generate_short_id();
        let id2 = generate_short_id();
        assert_ne!(id1, id2);
        assert!(id1.len() == 6);
    }

    #[test]
    fn test_global_disallowed_tools() {
        let disallowed = global_disallowed_tools();
        assert!(disallowed.contains("agent"));
        // Task delegation tools are blocked for sub-agents
        assert!(disallowed.contains("task_create"));
        assert!(disallowed.contains("task_update"));
        assert!(disallowed.contains("task_list"));
        assert!(disallowed.contains("task_get"));
        assert!(disallowed.contains("task_stop"));
        assert!(disallowed.contains("task_output"));
        assert!(disallowed.contains("send_message"));
        assert!(disallowed.contains("plan_approval"));
    }

    #[test]
    fn test_build_child_config_auto_permission_mode() {
        let parent_config = Config::default();
        let child_config = build_child_config(&parent_config, "", 0);
        // Sub-agents always use AUTO mode
        assert_eq!(child_config.permission_mode, crate::permissions::PermissionMode::Auto);
    }

    #[test]
    fn test_build_child_config_avoid_permission_prompts() {
        let parent_config = Config::default();
        let child_config = build_child_config(&parent_config, "", 0);
        // Sub-agents should avoid permission prompts
        assert!(child_config.should_avoid_permission_prompts);
    }

    #[test]
    fn test_build_child_config_default_turns_ceiling() {
        let parent_config = Config::default();
        let child_config = build_child_config(&parent_config, "", 0);
        // Default max turns is the safety ceiling (200)
        assert_eq!(child_config.max_turns, 200);
    }

    #[test]
    fn test_get_agent_type_config() {
        assert!(get_agent_type_config("explore").is_some());
        assert!(get_agent_type_config("plan").is_some());
        assert!(get_agent_type_config("verify").is_some());
        assert!(get_agent_type_config("nonexistent").is_none());
    }

    #[test]
    fn test_explore_deny_tools() {
        let config = get_agent_type_config("explore").unwrap();
        assert!(config.deny_tools.contains(&"write_file"));
        assert!(config.deny_tools.contains(&"edit_file"));
    }

    #[test]
    fn test_build_child_registry_removes_agent() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        // The agent tool is NOT in builtin tools, but let's verify the filtering
        let child = build_child_registry(&registry, "", &[], &[], false);
        // All non-agent tools should be present
        assert!(child.get("read_file").is_some());
        assert!(child.get("exec").is_some());
        // Task delegation tools should also be absent (even though they aren't registered here)
        assert!(child.get("task_create").is_none());
        assert!(child.get("task_output").is_none());
        assert!(child.get("send_message").is_none());
        assert!(child.get("plan_approval").is_none());
    }

    #[test]
    fn test_build_child_registry_explore_type() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        let child = build_child_registry(&registry, "explore", &[], &[], false);
        // Explore type should not have write tools
        assert!(child.get("write_file").is_none());
        assert!(child.get("edit_file").is_none());
        assert!(child.get("exec").is_none());
        // But should have read tools
        assert!(child.get("read_file").is_some());
        assert!(child.get("grep").is_some());
    }

    #[test]
    fn test_build_child_registry_with_allowed_tools() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        let allowed = vec!["read_file".to_string(), "grep".to_string()];
        let child = build_child_registry(&registry, "", &allowed, &[], false);
        assert!(child.get("read_file").is_some());
        assert!(child.get("grep").is_some());
        assert!(child.get("exec").is_none());
    }

    #[test]
    fn test_build_child_registry_with_wildcard() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        let allowed = vec!["*".to_string()];
        let disallowed = vec!["exec".to_string()];
        let child = build_child_registry(&registry, "", &allowed, &disallowed, false);
        // All tools except exec (explicitly disallowed) and agent (globally disallowed)
        assert!(child.get("read_file").is_some());
        assert!(child.get("exec").is_none());
    }

    // ─── Feature test: Custom model parameter passing ────────────────────────

    #[test]
    fn test_custom_model_param_passing() {
        let parent_config = Config::default();
        let child_config = build_child_config(&parent_config, "claude-3-5-haiku-20241022", 0);
        // Model should be overridden
        assert_eq!(child_config.model, "claude-3-5-haiku-20241022");
    }

    #[test]
    fn test_custom_model_param_empty_falls_back_to_parent() {
        let mut parent_config = Config::default();
        parent_config.model = "claude-3-5-sonnet-20241022".to_string();
        let child_config = build_child_config(&parent_config, "", 0);
        // Empty override should keep parent's model
        assert_eq!(child_config.model, "claude-3-5-sonnet-20241022");
    }

    #[test]
    fn test_subagent_type_inherits_parent_model_if_no_override() {
        let mut parent_config = Config::default();
        parent_config.model = "claude-3-opus-20240229".to_string();
        let child_config = build_child_config(&parent_config, "", 0);
        assert_eq!(child_config.model, "claude-3-opus-20240229");
    }

    // ─── Feature test: fork mode inherit_context ─────────────────────────────

    #[test]
    fn test_clone_context_for_fork_basic() {
        let parent_ctx = Arc::new(tokio::sync::RwLock::new(ConversationContext::new(Config::default())));
        let child_ctx = Arc::new(tokio::sync::RwLock::new(ConversationContext::new(Config::default())));

        // Add some messages to parent
        {
            let mut parent = parent_ctx.blocking_write();
            parent.add_user_message("Hello from parent".to_string());
            parent.add_assistant_text("Response from parent".to_string());
        }

        clone_context_for_fork(&parent_ctx, &child_ctx);

        // Verify child got the messages
        {
            let child = child_ctx.blocking_read();
            let entries = child.entries();
            assert_eq!(entries.len(), 2, "Child should have 2 entries from parent");
            assert!(matches!(&entries[0].content, MessageContent::Text(t) if t == "Hello from parent"));
            assert!(matches!(&entries[1].content, MessageContent::Text(t) if t == "Response from parent"));
        }
    }

    #[test]
    fn test_clone_context_for_fork_skips_last_tool_use() {
        use crate::context::{Message, MessageContent, ToolUseBlock};

        let parent_ctx = Arc::new(tokio::sync::RwLock::new(ConversationContext::new(Config::default())));
        let child_ctx = Arc::new(tokio::sync::RwLock::new(ConversationContext::new(Config::default())));

        // Add user message, assistant text, tool use block (the agent tool call)
        {
            let mut parent = parent_ctx.blocking_write();
            parent.add_user_message("User message".to_string());
            parent.add_assistant_text("Thinking...".to_string());
            // Add a ToolUseBlocks entry (simulates the agent tool that spawned this child)
            let tool_use_msg = Message::new(
                MessageRole::Assistant,
                MessageContent::ToolUseBlocks(vec![ToolUseBlock {
                    id: "toolu_agent_123".to_string(),
                    name: "agent".to_string(),
                    input: std::collections::HashMap::from([
                        ("prompt".to_string(), serde_json::json!("test")),
                    ]),
                }]),
            );
            parent.add_message(tool_use_msg);
        }

        clone_context_for_fork(&parent_ctx, &child_ctx);

        // Verify child got user message + assistant text but NOT the ToolUseBlocks entry
        {
            let child = child_ctx.blocking_read();
            let entries = child.entries();
            assert_eq!(entries.len(), 2, "Child should have 2 entries (agent ToolUseBlocks skipped)");
            assert!(matches!(&entries[0].content, MessageContent::Text(t) if t == "User message"));
            assert!(matches!(&entries[1].content, MessageContent::Text(t) if t == "Thinking..."));
        }
    }

    // ─── Feature test: allowed_tools with wildcard ["*"] ─────────────────────

    #[test]
    fn test_wildcard_allowed_tools_includes_all_non_disallowed() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        let allowed = vec!["*".to_string()];
        let child = build_child_registry(&registry, "", &allowed, &[], false);
        // With wildcard, all tools should be present
        assert!(child.get("read_file").is_some());
        assert!(child.get("write_file").is_some());
        assert!(child.get("edit_file").is_some());
        assert!(child.get("exec").is_some());
        assert!(child.get("grep").is_some());
        assert!(child.get("glob").is_some());
    }

    #[test]
    fn test_wildcard_with_disallowed_excludes_specific() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        let allowed = vec!["*".to_string()];
        let disallowed = vec!["exec".to_string(), "write_file".to_string(), "edit_file".to_string()];
        let child = build_child_registry(&registry, "", &allowed, &disallowed, false);
        // Wildcard allows all, but disallowed still blocks specific tools
        assert!(child.get("read_file").is_some());
        assert!(child.get("grep").is_some());
        assert!(child.get("exec").is_none());
        assert!(child.get("write_file").is_none());
        assert!(child.get("edit_file").is_none());
    }

    #[test]
    fn test_wildcard_with_explore_type_excludes_type_specific() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        let allowed = vec!["*".to_string()];
        let child = build_child_registry(&registry, "explore", &allowed, &[], false);
        // Wildcard + explore type: type-specific deny tools should still be excluded
        assert!(child.get("read_file").is_some());
        assert!(child.get("grep").is_some());
        assert!(child.get("write_file").is_none());
        assert!(child.get("edit_file").is_none());
        assert!(child.get("exec").is_none());
    }

    // ─── Feature test: AgentTool input schema validation ─────────────────────

    #[test]
    fn test_agent_tool_missing_prompt() {
        use crate::tools::agent_tool::AgentTool;
        let tool = AgentTool::with_spawn_func(|_, _, _, _, _, _, _, _, _| {
            ("agent-test".to_string(), "ok".to_string(), String::new(), String::new(), 0, 0)
        });
        let params = HashMap::new(); // empty params - missing required "prompt"
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("prompt is required"));
    }

    #[test]
    fn test_agent_tool_empty_prompt() {
        use crate::tools::agent_tool::AgentTool;
        let tool = AgentTool::with_spawn_func(|_, _, _, _, _, _, _, _, _| {
            ("agent-test".to_string(), "ok".to_string(), String::new(), String::new(), 0, 0)
        });
        let mut params = HashMap::new();
        params.insert("prompt".to_string(), serde_json::json!(""));
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("prompt is required"));
    }

    #[test]
    fn test_agent_tool_no_spawn_func() {
        use crate::tools::agent_tool::AgentTool;
        let tool = AgentTool::new(); // no spawn_func set
        let mut params = HashMap::new();
        params.insert("prompt".to_string(), serde_json::json!("hello"));
        let result = tool.execute(params);
        assert!(result.is_error);
        assert!(result.output.contains("agent system not initialized"));
    }

    #[test]
    fn test_agent_tool_input_schema_requires_description_and_prompt() {
        use crate::tools::agent_tool::AgentTool;
        use crate::tools::Tool;
        let tool = AgentTool::new();
        let schema = tool.input_schema();
        let required = schema.get("required").and_then(|v| v.as_array()).unwrap();
        assert!(required.contains(&serde_json::json!("description")));
        assert!(required.contains(&serde_json::json!("prompt")));
    }

    // ─── Feature test: Multiple sequential sub-agents ────────────────────────

    #[test]
    fn test_multiple_sequential_registry_builds_no_state_corruption() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        // Build multiple child registries in sequence with different configs
        let allowed1 = vec!["read_file".to_string()];
        let child1 = build_child_registry(&registry, "", &allowed1, &[], false);
        assert!(child1.get("read_file").is_some());
        assert!(child1.get("exec").is_none());

        let allowed2 = vec!["exec".to_string()];
        let child2 = build_child_registry(&registry, "", &allowed2, &[], false);
        assert!(child2.get("exec").is_some());
        assert!(child2.get("read_file").is_none());

        let allowed3 = vec!["*".to_string()];
        let child3 = build_child_registry(&registry, "", &allowed3, &[], false);
        assert!(child3.get("read_file").is_some());
        assert!(child3.get("exec").is_some());
        assert!(child3.get("write_file").is_some());
    }

    #[test]
    fn test_sequential_registry_builds_different_agent_types() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        let explore = build_child_registry(&registry, "explore", &[], &[], false);
        let plan = build_child_registry(&registry, "plan", &[], &[], false);
        let verify = build_child_registry(&registry, "verify", &[], &[], false);
        let general = build_child_registry(&registry, "", &[], &[], false);

        // All should have read_file
        assert!(explore.get("read_file").is_some());
        assert!(plan.get("read_file").is_some());
        assert!(verify.get("read_file").is_some());
        assert!(general.get("read_file").is_some());

        // Explore and plan should NOT have write_file
        assert!(explore.get("write_file").is_none());
        assert!(plan.get("write_file").is_none());
        // Verify should NOT have write_file (type deny list)
        assert!(verify.get("write_file").is_none());
        // General should have write_file
        assert!(general.get("write_file").is_some());
    }

    #[test]
    fn test_sequential_registry_builds_with_disallowed() {
        let registry = Registry::new();
        crate::tools::register_builtin_tools(&registry);

        // First build with exec disallowed
        let disallowed1 = vec!["exec".to_string()];
        let child1 = build_child_registry(&registry, "", &[], &disallowed1, false);
        assert!(child1.get("exec").is_none());

        // Second build without exec disallowed (verify no cross-contamination)
        let child2 = build_child_registry(&registry, "", &[], &[], false);
        assert!(child2.get("exec").is_some());
    }
}

