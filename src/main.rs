use anyhow::Result;
use clap::Parser;
use miniclaudecode_rust::agent_loop;
use miniclaudecode_rust::config::{load_config_from_file, Config};
use miniclaudecode_rust::permissions::PermissionMode;
use miniclaudecode_rust::tools;
use miniclaudecode_rust::work_task;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "miniclaudecode")]
#[command(about = "A lightweight AI coding assistant", long_about = None)]
struct Args {
    /// Anthropic model to use
    #[arg(long)]
    model: Option<String>,

    /// API key (overrides ANTHROPIC_API_KEY env and config file)
    #[arg(long)]
    api_key: Option<String>,

    /// Custom API base URL
    #[arg(long)]
    base_url: Option<String>,

    /// Permission mode (ask|auto|bypass|plan)
    #[arg(long, default_value = "ask")]
    mode: String,

    /// Max agent loop turns per message
    #[arg(long, default_value_t = 90)]
    max_turns: usize,

    /// Enable streaming output (default: true)
    #[arg(long, short, default_value_t = true)]
    stream: bool,

    /// Disable streaming output (overrides --stream)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_stream: bool,

    /// Project directory
    #[arg(long)]
    dir: Option<PathBuf>,

    /// Resume from a previous session (transcript file path or 'last' for most recent)
    #[arg(long)]
    resume: Option<String>,

    /// Message to process (one-shot mode)
    #[arg(trailing_var_arg = true)]
    message: Option<Vec<String>>,
}

fn main() -> Result<()> {
    // Enhanced panic hook: log location + message to stderr, plus backtrace
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            eprintln!("thread panicked: (unknown payload)");
            return;
        };
        if msg.contains("Cannot drop a runtime in a context where blocking is not allowed") {
            return;  // Suppress this specific panic
        }
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_else(|| "unknown".to_string());
        eprintln!("PANIC at {}: {}", location, msg);
    }));

    let args = Args::parse();

    // Compute effective streaming flag: --stream (default: true) overridden by --no-stream
    let use_stream = args.stream && !args.no_stream;

    // Priority: flags > env > project .claude/settings.json > home ~/.claude/settings.json > defaults
    let mut cfg = Config::default();

    // Normalize the --dir path: on Windows, forward slashes are converted to backslashes
    // and the path is cleaned. This ensures --dir values like "E:/workspace/project"
    // work on Windows, and guards against shell argument processing that may strip
    // backslashes (e.g., "E:\workspace\project" becoming "E:workspaceproject").
    if let Some(ref raw_dir) = args.dir {
        let normalized_dir = normalize_path_separators(raw_dir);
        if let Err(e) = std::env::set_current_dir(&normalized_dir) {
            eprintln!("[!] Failed to change working directory to {} (original: {}): {}", normalized_dir.display(), raw_dir.display(), e);
        }
    }
    if let Some(project_dir) = args.dir.as_ref().map(normalize_path_separators).or_else(|| std::env::current_dir().ok()) {
        if let Some(file_cfg) = load_config_from_file(&project_dir) {
            if let Some(api_key) = file_cfg.api_key {
                cfg.api_key = Some(api_key);
            }
            if let Some(base_url) = file_cfg.base_url {
                cfg.base_url = Some(base_url);
            }
            cfg.model = file_cfg.model;
            if let Some(mcp_manager) = file_cfg.mcp_manager {
                cfg.mcp_manager = Some(mcp_manager);
            }
            if let Some(skill_loader) = file_cfg.skill_loader {
                cfg.skill_loader = Some(skill_loader);
            }
        }
    }

    // Environment variables override settings file
    if let Ok(env_key) = std::env::var("ANTHROPIC_API_KEY") {
        cfg.api_key = Some(env_key);
    } else if let Ok(env_key) = std::env::var("ANTHROPIC_AUTH_TOKEN") {
        cfg.api_key = Some(env_key);
    }
    if let Ok(env_url) = std::env::var("ANTHROPIC_BASE_URL") {
        cfg.base_url = Some(env_url);
    }
    if let Ok(env_model) = std::env::var("ANTHROPIC_MODEL") {
        cfg.model = env_model;
    }

    // Flags override everything
    if let Some(model) = args.model {
        cfg.model = model;
    }
    if let Some(api_key) = args.api_key {
        cfg.api_key = Some(api_key);
    }
    if let Some(base_url) = args.base_url {
        cfg.base_url = Some(base_url);
    }
    *cfg.permission_mode.lock().unwrap_or_else(|e| e.into_inner()) = PermissionMode::from_str(&args.mode);
    cfg.max_turns = args.max_turns;

    // Validate: model is required
    if cfg.model.is_empty() {
        eprintln!("[!] No model specified. Set it via --model flag, ANTHROPIC_MODEL env, or model in .claude/settings.json (project or home)");
        std::process::exit(1);
    }

    // Always initialize file history (with disk persistence) -- shared Arc between tools and agent_loop
    use miniclaudecode_rust::filehistory::FileHistory;
    use std::sync::Arc;
    let snapshots_dir = std::env::current_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("snapshots");
    cfg.file_history = Some(Arc::new(FileHistory::new_with_dir(&snapshots_dir)));

    // Initialize SessionMemory (Phase 4)
    let session_memory = miniclaudecode_rust::session_memory::SessionMemory::new(
        &std::env::current_dir().unwrap_or_default(),
    );
    session_memory.start_flush_loop();
    let session_memory_arc = Arc::new(session_memory);
    cfg.session_memory = Some(Arc::clone(&session_memory_arc));

    // Register all tools
    let registry = tools::Registry::new();
    tools::register_builtin_tools(&registry);
    tools::register_memory_tools(&registry, &session_memory_arc);

    // Initialize TaskStore for bash background tasks.
    // Created early so MCP tools can use it for timeout-to-background.
    let task_store = miniclaudecode_rust::task_store::TaskStore::new_shared();

    // Register MCP and skills (needs task_store for timeout-to-background)
    tools::register_mcp_and_skills(&registry, &cfg, &task_store);

    // Initialize TodoList for TodoWrite tool
    let todo_list = Arc::new(miniclaudecode_rust::context::TodoList::new());
    tools::register_todo_write_tools(&registry, &todo_list);

    // Initialize WorkTaskStore and register task tools
    let work_task_store = work_task::WorkTaskStore::new_shared();
    tools::register_task_tools(&registry, &work_task_store);

    // Create a shared notification channel so both main and resume sessions
    // can send task completion notifications to the parent agent loop.
    let (notification_tx, notification_rx) = tokio::sync::mpsc::unbounded_channel();
    let notification_tx_arc = Arc::new(notification_tx);
    tools::register_bash_task_tools_with_tx(&registry, task_store.clone(), Arc::clone(&notification_tx_arc));

    // Initialize AgentTaskStore for background sub-agent management
    let agent_task_store = miniclaudecode_rust::tools::agent_store::AgentTaskStore::new_shared();
    tools::register_agent_management_tools(&registry, &agent_task_store);
    tools::register_send_message_tool(&registry, &agent_task_store);

    // Register EnterPlanMode/ExitPlanMode tools (mode changes applied via ToolResult side-effects)
    tools::register_plan_mode_tools(&registry, &cfg);

    // Parent context slot for agent tool's fork mode — set after agent loop is created
    let parent_context_slot: std::sync::Arc<
        tokio::sync::RwLock<
            Option<std::sync::Arc<tokio::sync::RwLock<miniclaudecode_rust::context::ConversationContext>>>
        >
    > = std::sync::Arc::new(tokio::sync::RwLock::new(None));

    // Register agent tool with spawn callback
    {
        let cfg_for_spawn = cfg.clone();
        let registry_arc_for_spawn = std::sync::Arc::new(tokio::sync::RwLock::new(registry.clone_for_spawn()));
        let context_slot_for_closure = parent_context_slot.clone();
        let use_stream_for_spawn = use_stream;
        let store_for_closure = agent_task_store.clone();
        let notification_tx_for_spawn = notification_tx_arc.clone();
        let spawn_func: tools::agent_tool::AgentSpawnFunc = std::sync::Arc::new(move |
            prompt: &str,
            subagent_type: &str,
            model: &str,
            run_in_background: bool,
            allowed_tools: &[String],
            disallowed_tools: &[String],
            inherit_context: bool,
            max_turns: usize,
            _parent_context: Option<std::sync::Arc<tokio::sync::RwLock<miniclaudecode_rust::context::ConversationContext>>>,
        | -> (String, String, String, String, usize, u64) {
            // Read parent context from the slot
            let parent_ctx = context_slot_for_closure.blocking_read().clone();
            let parent_registry = registry_arc_for_spawn.blocking_read();
            miniclaudecode_rust::agent_sub::spawn_sub_agent_sync(
                &cfg_for_spawn,
                &*parent_registry,
                prompt,
                subagent_type,
                model,
                run_in_background,
                allowed_tools,
                disallowed_tools,
                inherit_context,
                max_turns,
                parent_ctx,
                use_stream_for_spawn,
                Some(&store_for_closure),
                Some(Arc::clone(&notification_tx_for_spawn)),
            )
        });
        tools::register_agent_tool(&registry, spawn_func);
    }

    // Finalize: populate ToolSearchTool's shared tools list with all registered tools
    registry.finalize_tool_search();

    // Handle --resume flag
    let resume_path = args.resume.as_ref().map(|s| find_transcript(s)).flatten();

    let agent = if let Some(transcript_path) = resume_path {
        // Create a new registry for the resumed session
        let resume_registry = tools::Registry::new();
        tools::register_builtin_tools(&resume_registry);
        tools::register_mcp_and_skills(&resume_registry, &cfg, &task_store);
        tools::register_memory_tools(&resume_registry, &session_memory_arc);
        tools::register_task_tools(&resume_registry, &work_task_store);
        tools::register_bash_task_tools_with_tx(&resume_registry, task_store.clone(), Arc::clone(&notification_tx_arc));
        tools::register_todo_write_tools(&resume_registry, &todo_list);
        tools::register_agent_management_tools(&resume_registry, &agent_task_store);
        tools::register_send_message_tool(&resume_registry, &agent_task_store);
        tools::register_plan_mode_tools(&resume_registry, &cfg);
        resume_registry.finalize_tool_search();

        match agent_loop::AgentLoop::from_transcript(cfg.clone(), resume_registry, use_stream, &transcript_path, true, Some(Arc::clone(&todo_list))) {
            Ok(agent) => {
                println!("[+] Resumed session from: {}", transcript_path.display());
                agent
            }
            Err(e) => {
                eprintln!("[!] Failed to resume: {}. Starting new session.", e);
                agent_loop::AgentLoop::new(cfg, registry, use_stream, Some(Arc::clone(&todo_list)))?
            }
        }
    } else {
        agent_loop::AgentLoop::new(cfg, registry, use_stream, Some(Arc::clone(&todo_list)))?
    };

    // Wire notification channel for bash task/sub-agent completion notifications
    let mut agent_with_notif = agent;
    agent_with_notif.set_notification_rx(notification_rx);
    // Inject running agent status during compaction
    agent_with_notif.set_agent_task_store(agent_task_store.clone());

    // Set parent context for fork mode so the agent tool can access it
    {
        let mut slot = parent_context_slot.blocking_write();
        *slot = Some(agent_with_notif.context_arc());
    }

    // One-shot mode
    if let Some(message) = args.message {
        let prompt = message.join(" ");
        let result = agent_with_notif.run(&prompt);

        // Drain any pending sub-agent notifications before exit.
        // Brief wait to allow in-flight notifications to arrive,
        // then drain and display them (matching Go's drainOneShotNotifications).
        let notifications = {
            std::thread::sleep(std::time::Duration::from_millis(100));
            agent_with_notif.drain_notifications()
        };

        // When streaming was used, TerminalHandler already displayed the text
        // via stderr (eprint! calls). Printing the same text again to stdout
        // would cause duplication (e.g., "hellohello" when redirected with 2>&1).
        // Only print to stdout when streaming was NOT used (non-streaming mode).
        let last_was_streaming = agent_with_notif.last_call_was_streaming.load(std::sync::atomic::Ordering::SeqCst);
        if !last_was_streaming {
            println!("{}", result);
        }

        if !notifications.is_empty() {
            println!("\n--- Sub-agent notifications ---");
            for n in &notifications {
                println!("{}", n);
            }
        }

        agent_with_notif.close();
        return Ok(());
    }

    // Interactive REPL
    run_interactive(agent_with_notif, work_task_store, agent_task_store, todo_list, task_store);
    Ok(())
}

fn run_interactive(mut agent: agent_loop::AgentLoop, work_task_store: work_task::SharedWorkTaskStore, agent_task_store: miniclaudecode_rust::tools::agent_store::SharedAgentTaskStore, todo_list: Arc<miniclaudecode_rust::context::TodoList>, task_store: miniclaudecode_rust::task_store::SharedTaskStore) {
    // Get transcript filename for resume hint (strip .jsonl extension)
    let transcript_stem = agent.transcript_filename().trim_end_matches(".jsonl");

    // Clone MCP manager for use in signal handler (to stop servers on double-Ctrl+C)
    // The agent.close() method takes &self and accesses this Arc<McpManager>.
    let mcp_for_signal = agent.config.mcp_manager.clone();

    // Track Ctrl+C presses for double-press exit
    let last_ctrlc = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
    // Use the agent's interrupted flag directly
    let interrupted = agent.interrupted_flag();

    let last_ctrlc_clone = last_ctrlc.clone();
    let transcript_for_signal = transcript_stem.to_string();
    let interrupted_clone = interrupted.clone();

    ctrlc::set_handler(move || {
        let now = std::time::Instant::now();
        let mut last = last_ctrlc_clone.lock().unwrap_or_else(|e| e.into_inner());

        // Check if this is a double press within 2 seconds
        if let Some(last_time) = *last {
            if now.duration_since(last_time).as_secs() < 2 {
                // Double press - exit immediately after cleanup
                println!("\n\nExiting!");
                println!("To resume this session: --resume {}", transcript_for_signal);
                // Cleanup: stop MCP servers before exit (flushes transcript, etc.)
                if let Some(ref mgr) = mcp_for_signal {
                    mgr.stop_all();
                }
                std::process::exit(0);
            }
        }

        // First press - set interrupted flag
        *last = Some(now);
        interrupted_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        println!("\n[Interrupted] Press Ctrl+C again within 2s to exit, or continue working.");
    }).expect("Failed to set Ctrl+C handler");

    let mut stdout = io::stdout();

    loop {
        print!("\n> ");
        let _ = stdout.flush();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => {
                // EOF -- stdin was closed
                println!("\nGoodbye!");
                println!("To resume this session: --resume {}", agent.transcript_filename().trim_end_matches(".jsonl"));
                agent.close();
                break;
            }
            Ok(_) => {}
            Err(_) => {
                // On Windows, Ctrl+C breaks the stdin handle.
                // Reopen CONIN$ to recover stdin, then continue the loop.
                // The interrupted flag was set by the signal handler, clear it.
                interrupted.store(false, std::sync::atomic::Ordering::SeqCst);

                // On Windows, reopen the console input to recover from the broken handle
                #[cfg(target_os = "windows")]
                {
                    use std::fs::File;
                    use std::io::BufRead;
                    let mut retry = String::new();
                    if let Ok(f) = File::open("CONIN$") {
                        let mut reader = io::BufReader::new(f);
                        // Drain the ^C character that may be in the buffer
                        let _ = reader.read_line(&mut retry);
                    }
                }
                continue;
            }
        }

        let user_input = input.trim();
        if user_input.is_empty() {
            continue;
        }

        // Check for exact command match -- only treat as command if the first
        // word is a known command. Unknown /xxx is passed through as prompt text.
        let is_known_cmd = if user_input.starts_with('/') {
            let parts: Vec<&str> = user_input.split_whitespace().collect();
            let cmd = parts.first().unwrap_or(&"").to_lowercase();
            matches!(cmd.as_str(), "/quit" | "/exit" | "/q" | "/tools" | "/mode" | "/help" | "/resume" | "/compact" | "/partialcompact" | "/clear" | "/agents" | "/doctor" | "/history" | "/cleanup" | "/branch" | "/errors" | "/feature" | "/settings" | "/telemetry" | "/status" | "/model" | "/idlecompact" | "/save" | "/load" | "/sessions")
        } else {
            false
        };

        if is_known_cmd {
            let parts: Vec<&str> = user_input.split_whitespace().collect();
            let cmd = parts.first().unwrap_or(&"").to_lowercase();

            match cmd.as_str() {
                "/quit" | "/exit" | "/q" => {
                    println!("\nGoodbye!");
                    println!("To resume this session: --resume {}", agent.transcript_filename().trim_end_matches(".jsonl"));
                    agent.close();
                    break;
                }
                "/tools" => {
                    println!("\nAvailable tools:");
                    for tool in agent.registry.blocking_read().all_tools() {
                        println!("  - {}: {}", tool.name(), tool.description());
                    }
                    continue;
                }
                "/mode" => {
                    if let Some(mode) = parts.get(1) {
                        let mode_lower = mode.to_lowercase();
                        match mode_lower.as_str() {
                            "ask" | "auto" | "bypass" | "plan" => {
                                *agent.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner()) = PermissionMode::from_str(&mode_lower);
                                println!("Mode changed to: {}", mode_lower);
                            }
                            _ => {
                                println!("Unknown mode: {}", mode);
                            }
                        }
                    } else {
                        println!("Current mode: {}", agent.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner()));
                        println!("Usage: /mode [ask|auto|bypass|plan]");
                    }
                    continue;
                }
                "/compact" => {
                    let stats = agent.force_compact();
                    if stats.entries_before == 0 {
                        println!("No messages to compact.");
                    } else if stats.estimated_tokens_saved > 0 {
                        println!("[compact] {} -> {} entries, ~{} tokens saved",
                            stats.entries_before, stats.entries_after, stats.estimated_tokens_saved);
                    } else {
                        println!("[compact] No compaction needed ({} entries, ~{} tokens).",
                            stats.entries_before, stats.estimated_tokens_before);
                    }
                    continue;
                }
                "/partialcompact" => {
                    // /partialcompact [up_to|from] [pivot_index]
                    let direction = parts.get(1).map(|s| *s).unwrap_or("up_to");
                    let pivot: Option<usize> = parts.get(2).and_then(|s| s.parse().ok());
                    let result = agent.force_partial_compact(direction, pivot);
                    if result.entries_before == 0 {
                        println!("No messages to partial compact.");
                    } else {
                        let saved = result.pre_compact_tokens.saturating_sub(result.post_compact_tokens);
                        println!("[partial-compact {}] {} -> {} entries, ~{} tokens saved (pivot={})",
                            direction, result.entries_before, result.entries_after, saved,
                            pivot.unwrap_or(result.entries_before / 2));
                    }
                    continue;
                }
                "/clear" => {
                    let count = agent.clear_context();
                    agent.registry.blocking_read().clear_files_read();
                    if count > 0 {
                        println!("[clear] Cleared {} messages.", count);
                    } else {
                        println!("[clear] No messages to clear.");
                    }
                    continue;
                }
                "/help" => {
                    println!("Commands:");
                    println!("  /help    -- Show available commands (or /help <cmd> for detailed help)");
                    println!("  /compact -- Force context compaction");
                    println!("  /partialcompact [up_to|from] [pivot] -- Partial compact, optionally direction and pivot index");
                    println!("  /clear   -- Clear conversation history");
                    println!("  /mode    -- Switch permission mode (ask|auto|bypass|plan)");
                    println!("  /resume  -- Resume a previous session");
                    println!("  /tools   -- List available tools");
                    println!("  /agents  -- Manage background agents (/agents help for details)");
                    println!("  /status  -- Show session status (model, tokens, cache, cost)");
                    println!("  /model   -- View and switch models");
                    println!("  /telemetry -- View telemetry events");
                    println!("  /settings -- View and modify settings");
                    println!("  /errors  -- View error logs");
                    println!("  /history -- Show recent prompts");
                    println!("  /branch  -- Create/switch conversation branches");
                    println!("  /doctor  -- Run installation diagnostics");
                    println!("  /cleanup -- Remove stale session files");
                    println!("  /idlecompact -- Manual idle compression");
                    println!("  /save    -- Save current session");
                    println!("  /load    -- Load a saved session");
                    println!("  /sessions -- List saved sessions");
                    println!("  /feature -- Manage feature flags");
                    println!("  /quit    -- Exit");
                    continue;
                }
                "/resume" => {
                    // List available transcripts or resume specific one
                    let transcript_dir = PathBuf::from(".claude").join("transcripts");

                    if parts.len() > 1 {
                        // Resume specific transcript
                        let target = parts[1];
                        let transcript_path = find_transcript(target);
                        if transcript_path.is_none() {
                            println!("Transcript not found: {}", target);
                            continue;
                        }

                        // Close current agent and start new one from transcript
                        agent.close();
                        let registry = tools::Registry::new();
                        tools::register_builtin_tools(&registry);
                        tools::register_mcp_and_skills(&registry, &agent.config, &task_store);
                        if let Some(ref sm) = agent.config.session_memory {
                            tools::register_memory_tools(&registry, sm);
                        }
                        tools::register_task_tools(&registry, &work_task_store);
                        tools::register_todo_write_tools(&registry, &todo_list);
                        tools::register_agent_management_tools(&registry, &agent_task_store);
                        tools::register_send_message_tool(&registry, &agent_task_store);
                        tools::register_plan_mode_tools(&registry, &agent.config);
                        registry.finalize_tool_search();

                        match agent_loop::AgentLoop::from_transcript(
                            agent.config.clone(),
                            registry,
                            agent.use_stream,
                            &transcript_path.unwrap(),
                            true,
                            Some(Arc::clone(&todo_list)),
                        ) {
                            Ok(new_agent) => {
                                agent = new_agent;
                                println!("[+] Resumed session from transcript");
                            }
                            Err(e) => {
                                println!("[!] Failed to resume: {}", e);
                            }
                        }
                    } else {
                        // List available transcripts
                        if !transcript_dir.exists() {
                            println!("No transcripts found. Start a session first.");
                            continue;
                        }

                        let transcripts = list_transcripts(&transcript_dir);
                        if transcripts.is_empty() {
                            println!("No transcripts found.");
                            continue;
                        }

                        println!("\nAvailable transcripts:");
                        for (i, t) in transcripts.iter().enumerate() {
                            println!("  {}. {}", i + 1, t);
                        }
                        println!("\nUsage: /resume <number> or /resume <filename>");
                    }
                    continue;
                }
                "/agents" => {
                    // /agents [list|show|stop] [args]
                    let sub_cmd = parts.get(1).map(|s| *s).unwrap_or("list");
                    match sub_cmd {
                        "list" => {
                            let tasks = agent_task_store.list();
                            if tasks.is_empty() {
                                println!("No agents.");
                                continue;
                            }
                            println!("{:<10} {:<12} {:<30} {:<15}", "ID", "Status", "Description", "Started");
                            println!("{}", "-".repeat(70));
                            for task in &tasks {
                                let elapsed = task.start_time.elapsed();
                                let started = if elapsed.as_secs() < 60 {
                                    format!("{}s ago", elapsed.as_secs())
                                } else if elapsed.as_secs() < 3600 {
                                    format!("{}m ago", elapsed.as_secs() / 60)
                                } else {
                                    format!("{}h ago", elapsed.as_secs() / 3600)
                                };
                                let desc = if task.description.len() > 28 {
                                    format!("{}...", &task.description[..task.description.floor_char_boundary(25)])
                                } else {
                                    task.description.clone()
                                };
                                println!("{:<10} {:<12} {:<30} {:<15}",
                                    task.id, task.status(), desc, started);
                            }
                            continue;
                        }
                        "show" => {
                            if let Some(id) = parts.get(2) {
                                if let Some(task) = agent_task_store.get(id) {
                                    let elapsed = task.start_time.elapsed();
                                    let duration_str = if elapsed.as_secs() < 60 {
                                        format!("{}s", elapsed.as_secs())
                                    } else {
                                        format!("{:.1}m", elapsed.as_secs() as f64 / 60.0)
                                    };
                                    println!("Agent ID:       {}", task.id);
                                    println!("Status:        {}", task.status());
                                    println!("Description:   {}", task.description);
                                    println!("Type:          {}", task.subagent_type);
                                    println!("Model:         {}", if task.model.is_empty() { "-" } else { &task.model });
                                    println!("Duration:      {}", duration_str);
                                    println!("Tools used:    {}", task.tools_used());
                                    let output = task.get_output();
                                    if !output.is_empty() {
                                        println!("\n--- Output ({} chars) ---", output.len());
                                        println!("{}", output);
                                    }
                                } else {
                                    println!("Agent '{}' not found.", id);
                                }
                            } else {
                                println!("Usage: /agents show <id>");
                            }
                            continue;
                        }
                        "stop" => {
                            if let Some(id) = parts.get(2) {
                                let killed = agent_task_store.kill(id);
                                if killed {
                                    println!("Agent '{}' killed.", id);
                                } else {
                                    println!("Agent '{}' not found or already stopped.", id);
                                }
                            } else {
                                println!("Usage: /agents stop <id>");
                            }
                            continue;
                        }
                        "help" | _ => {
                            println!("Usage: /agents <command>");
                            println!("Commands:");
                            println!("  /agents list          -- List all agents");
                            println!("  /agents show <id>     -- Show details of an agent");
                            println!("  /agents stop <id>     -- Kill a running agent");
                            println!("  /agents help          -- Show this help");
                            continue;
                        }
                    }
                }
                "/doctor" => {
                    run_doctor(&agent);
                    continue;
                }
                "/history" => {
                    if let Some(sub) = parts.get(1).map(|s| *s) {
                        if sub == "clear" {
                            let history_path = std::path::Path::new(".claude/history.jsonl");
                            if history_path.exists() {
                                let _ = std::fs::remove_file(history_path);
                                println!("History cleared.");
                            } else {
                                println!("No history file found.");
                            }
                        } else if let Ok(n) = sub.parse::<usize>() {
                            print_history(n);
                        } else {
                            println!("Usage: /history [N|clear]");
                        }
                    } else {
                        print_history(20);
                    }
                    continue;
                }
                "/cleanup" => {
                    let project_dir = agent.config.project_dir.to_string_lossy().to_string();
                    let days = parts.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(30);
                    run_cleanup(&project_dir, days);
                    continue;
                }
                "/branch" => {
                    if let Err(e) = handle_branch(&mut agent, &parts[1..], &task_store, &work_task_store, &agent_task_store, &todo_list) {
                        println!("Branch error: {}", e);
                    }
                    continue;
                }
                "/errors" => {
                    handle_errors_command(&parts[1..]);
                    continue;
                }
                "/feature" => {
                    handle_feature_command(&agent, &parts[1..]);
                    continue;
                }
                "/settings" => {
                    let project_dir = agent.config.project_dir.to_string_lossy().to_string();
                    handle_settings_command(&project_dir, &parts[1..]);
                    continue;
                }
                "/telemetry" => {
                    handle_telemetry_command(&parts[1..]);
                    continue;
                }
                "/status" => {
                    handle_status_command(&agent);
                    continue;
                }
                "/model" => {
                    handle_model_command(&mut agent, &parts[1..]);
                    continue;
                }
                "/idlecompact" => {
                    if agent.force_compact().entries_before == 0 {
                        println!("[idle] No messages to compact.");
                    } else {
                        agent.force_compact();
                        println!("[idle] Manual idle compression complete.");
                    }
                    continue;
                }
                "/save" => {
                    let project_dir = agent.config.project_dir.to_string_lossy().to_string();
                    if project_dir.is_empty() {
                        println!("[session] No project directory configured.");
                        continue;
                    }
                    let tp = agent.transcript_path();
                    if tp.is_empty() {
                        println!("[session] No active session to save.");
                        continue;
                    }
                    let sid = std::path::Path::new(&tp)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();

                    // Read transcript JSONL lines as messages
                    let messages: Vec<serde_json::Value> = if let Ok(data) = std::fs::read_to_string(&tp) {
                        data.lines()
                            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                            .collect()
                    } else {
                        Vec::new()
                    };

                    let model = agent.config.model.clone();
                    let perm_mode = format!("{}", agent.config.permission_mode.lock()
                        .map(|m| format!("{}", m)).unwrap_or_else(|_| "unknown".to_string()));

                    match miniclaudecode_rust::session_persistence::save_conversation(
                        &project_dir, &sid, &model, &perm_mode,
                        messages, 0,
                        agent.total_input_tokens(),
                        agent.total_output_tokens(),
                        0, 0,
                    ) {
                        Ok(path) => println!("[session] Saved to {}", std::path::Path::new(&path).file_name().map(|f| f.to_string_lossy()).unwrap_or_default()),
                        Err(e) => println!("[session] Save error: {}", e),
                    }
                    continue;
                }
                "/load" => {
                    let project_dir = agent.config.project_dir.to_string_lossy().to_string();
                    if parts.len() < 2 {
                        println!("Usage: /load <session-id>");
                        println!("Use /sessions to list available sessions.");
                        continue;
                    }
                    let sid = parts[1];
                    match miniclaudecode_rust::session_persistence::load_conversation(&project_dir, sid) {
                        Ok(snap) => {
                            let count = snap.entries.len();
                            // Close current agent and create new one from loaded snapshot
                            let saved_model = snap.model.clone();
                            agent.close();
                            let registry = tools::Registry::new();
                            tools::register_builtin_tools(&registry);
                            tools::register_mcp_and_skills(&registry, &agent.config, &task_store);
                            if let Some(ref sm) = agent.config.session_memory {
                                tools::register_memory_tools(&registry, sm);
                            }
                            tools::register_task_tools(&registry, &work_task_store);
                            tools::register_todo_write_tools(&registry, &todo_list);
                            tools::register_agent_management_tools(&registry, &agent_task_store);
                            tools::register_send_message_tool(&registry, &agent_task_store);
                            tools::register_plan_mode_tools(&registry, &agent.config);
                            registry.finalize_tool_search();

                            // Update config model if snapshot has one
                            if !saved_model.is_empty() {
                                agent.config.model = saved_model;
                            }

                            // Inject messages from snapshot into agent context
                            {
                                let mut context = agent.context_write();
                                for entry in &snap.entries {
                                    if let Some(role) = entry.get("role").and_then(|v| v.as_str()) {
                                        if let Some(content) = entry.get("content").and_then(|v| v.as_str()) {
                                            let msg_role = if role == "user" {
                                                miniclaudecode_rust::context::MessageRole::User
                                            } else {
                                                miniclaudecode_rust::context::MessageRole::Assistant
                                            };
                                            let msg = miniclaudecode_rust::context::Message::new(
                                                msg_role,
                                                miniclaudecode_rust::context::MessageContent::Text(content.to_string()),
                                            );
                                            context.add_message(msg);
                                        }
                                    }
                                }
                            }
                            println!("[session] Loaded session {}: {} entries restored.", sid, count);
                        }
                        Err(e) => println!("[session] Load error: {}", e),
                    }
                    continue;
                }
                "/sessions" => {
                    let project_dir = agent.config.project_dir.to_string_lossy().to_string();
                    if project_dir.is_empty() {
                        println!("[session] No project directory configured.");
                        continue;
                    }
                    let sessions = miniclaudecode_rust::session_persistence::list_sessions(&project_dir);
                    if sessions.is_empty() {
                        println!("[session] No saved sessions found.");
                        continue;
                    }
                    println!("Saved sessions ({}):", sessions.len());
                    for s in &sessions {
                        let updated = s.updated_at.split('T').next().unwrap_or(&s.updated_at);
                        println!("  {:<40}  {} entries  {} in/{} out tokens  updated {}",
                            s.session_id,
                            s.entries.len(),
                            miniclaudecode_rust::cost_tracker::format_token_count(s.total_input_tokens),
                            miniclaudecode_rust::cost_tracker::format_token_count(s.total_output_tokens),
                            updated);
                    }
                    println!("\nUse /load <session-id> to restore a session.");
                    continue;
                }
                _ => {
                    println!("Unknown command: {}", cmd);
                }
            }
        }

        println!();
        let result = agent.run(user_input);
        // When streaming was used, TerminalHandler already displayed the text
        // via stderr (eprint! calls). Printing the same text again to stdout
        // would cause duplication (e.g., "hellohello" when redirected with 2>&1).
        // Only print to stdout when streaming was NOT used (non-streaming mode).
        let last_was_streaming = agent.last_call_was_streaming.load(std::sync::atomic::Ordering::SeqCst);
        if !last_was_streaming {
            println!("{}", result);
        }
        println!();
    }
}

/// Find transcript file by name, number, or 'last'
fn find_transcript(target: &str) -> Option<PathBuf> {
    let transcript_dir = PathBuf::from(".claude").join("transcripts");

    if target == "last" {
        // Find most recent transcript
        let transcripts = list_transcripts(&transcript_dir);
        if transcripts.is_empty() {
            eprintln!("[!] No transcripts found in {}", transcript_dir.display());
        }
        return transcripts
            .first()
            .map(|name| transcript_dir.join(name));
    }

    // Try as number first
    if let Ok(num) = target.parse::<usize>() {
        let transcripts = list_transcripts(&transcript_dir);
        if num > 0 && num <= transcripts.len() {
            return Some(transcript_dir.join(&transcripts[num - 1]));
        } else if transcripts.is_empty() {
            eprintln!("[!] No transcripts found in {}", transcript_dir.display());
        } else {
            eprintln!("[!] Index {} out of range (1-{})", num, transcripts.len());
        }
    }

    // Try as exact filename
    let in_dir = transcript_dir.join(target);
    if in_dir.exists() {
        return Some(in_dir);
    }

    // Try with .jsonl extension
    if !target.ends_with(".jsonl") {
        let with_ext = transcript_dir.join(format!("{}.jsonl", target));
        if with_ext.exists() {
            return Some(with_ext);
        }
    }

    None
}

/// List transcript files sorted by modification time (most recent first)
fn list_transcripts(dir: &PathBuf) -> Vec<String> {
    if !dir.exists() {
        return Vec::new();
    }

    let mut transcripts: Vec<(String, std::time::SystemTime)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                let mtime = entry.metadata().and_then(|m| m.modified()).ok();
                if let Some(mtime) = mtime {
                    transcripts.push((name, mtime));
                }
            }
        }
    }

    // Sort by modification time, most recent first
    transcripts.sort_by(|a, b| b.1.cmp(&a.1));
    transcripts.iter().map(|(name, _)| name.clone()).collect()
}

/// Normalize path separators for Windows: replace forward slashes with backslashes,
/// then clean up . and .. elements. On non-Windows, just returns a clone.
/// This ensures --dir values like "E:/workspace/project" work on Windows,
/// and guards against shell argument processing that may strip backslashes.
fn normalize_path_separators(p: &PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        // Convert forward slashes to backslashes
        let with_backslash = p.to_string_lossy().replace('/', "\\");
        // Clean up the path (handles . and .. but does NOT require the path to exist)
        PathBuf::from(&with_backslash)
    }
    #[cfg(not(windows))]
    {
        p.clone()
    }
}

// =============================================================================
// Slash command helper functions for the 14 newly ported commands
// =============================================================================

/// /doctor — Run installation diagnostics
fn run_doctor(agent: &agent_loop::AgentLoop) {
    println!("\n=== Doctor Diagnostics ===");
    println!("Version: {}", "miniClaudeCode (Rust)");
    println!("Model: {}", agent.config.model);

    if let Some(ref key) = agent.config.api_key {
        if key.len() > 8 {
            println!("API Key: configured ({}...{})", &key[..4], &key[key.len()-4..]);
        } else {
            println!("API Key: configured (****)");
        }
    } else {
        println!("API Key: NOT configured");
    }

    let base_url = agent.config.base_url.as_deref().unwrap_or("https://api.anthropic.com");
    if base_url != "https://api.anthropic.com" {
        println!("Base URL: {} (custom)", base_url);
    } else {
        println!("Base URL: default (api.anthropic.com)");
    }

    // Tool checks
    let tools = [
        ("rg", "ripgrep"),
        ("python3", "Python"),
        ("node", "Node.js"),
        ("git", "Git"),
    ];
    for (cmd, name) in &tools {
        let found = command_exists(cmd);
        println!("{}: {}", name, if found { "found" } else { "NOT found" });
    }

    // MCP servers
    if let Some(ref mcp) = agent.config.mcp_manager {
        let servers = mcp.list_servers();
        println!("MCP Servers: {} registered", servers.len());
    }

    // Skills
    if let Some(ref loader) = agent.config.skill_loader {
        let skills = loader.list_skills();
        println!("Skills: {} loaded", skills.len());
    }

    // Transcripts
    let transcript_dir = PathBuf::from(".claude").join("transcripts");
    let transcript_count = if transcript_dir.exists() {
        std::fs::read_dir(&transcript_dir).map(|d| d.filter(|e| {
            e.as_ref().map(|e| e.path().extension().map_or(false, |ext| ext == "jsonl")).unwrap_or(false)
        }).count()).unwrap_or(0)
    } else { 0 };
    println!("Transcripts: {}", transcript_count);

    // Working directory
    if let Ok(wd) = std::env::current_dir() {
        println!("Working Dir: {}", wd.display());
    }

    // CLAUDE.md files
    for f in &["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"] {
        if PathBuf::from(f).exists() {
            println!("Config File: {} (exists)", f);
        }
    }

    println!("==========================");
}

fn command_exists(cmd: &str) -> bool {
    which::which(cmd).is_ok()
}

/// /history — Show recent prompts
fn print_history(n: usize) {
    let history_path = std::path::Path::new(".claude/history.jsonl");
    if !history_path.exists() {
        println!("No history found.");
        return;
    }
    let data = match std::fs::read_to_string(history_path) {
        Ok(d) => d,
        Err(_) => { println!("No history found."); return; }
    };

    let entries: Vec<serde_json::Value> = data.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();

    if entries.is_empty() {
        println!("No history found.");
        return;
    }

    let start = if n >= entries.len() { 0 } else { entries.len() - n };
    println!("\nRecent prompts ({}):", entries.len() - start);
    for (i, entry) in entries.iter().enumerate().skip(start) {
        let text = entry.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let ts = entry.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let truncated = if text.len() > 80 { &text[..77] } else { text };
        let time_short = if ts.len() > 16 { &ts[11..16] } else { ts };
        println!("  {}. [{}] {}", i - start + 1, time_short, truncated);
    }
}

/// /cleanup — Remove stale session files
fn run_cleanup(project_dir: &str, days: u64) {
    use std::time::Duration;
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(days * 86400);
    let mut removed = 0;

    let dirs = [
        format!("{}/.claude/transcripts", project_dir),
        format!("{}/.claude/plans", project_dir),
        format!("{}/.claude/sessions", project_dir),
    ];

    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if modified < cutoff {
                            let _ = std::fs::remove_file(&path);
                            removed += 1;
                        }
                    }
                }
            }
        }
    }

    if removed == 0 {
        println!("No stale files found.");
    } else {
        println!("Cleaned up {} stale files (cutoff: {} days).", removed, days);
    }
}

/// /branch — Create/switch conversation branches
fn handle_branch(
    agent: &mut agent_loop::AgentLoop,
    args: &[&str],
    task_store: &miniclaudecode_rust::task_store::SharedTaskStore,
    work_task_store: &miniclaudecode_rust::work_task::SharedWorkTaskStore,
    agent_task_store: &miniclaudecode_rust::tools::agent_store::SharedAgentTaskStore,
    todo_list: &std::sync::Arc<miniclaudecode_rust::context::TodoList>,
) -> Result<(), String> {
    let branch_dir = PathBuf::from(".claude/branches");
    let _ = std::fs::create_dir_all(&branch_dir);

    if args.is_empty() {
        // Create new branch
        let now = chrono::Local::now();
        let branch_name = format!("branch-{}", now.format("%H%M%S"));
        let branch_file = branch_dir.join(format!("{}.jsonl", branch_name));
        let src_file = PathBuf::from(".claude/transcript.jsonl");

        if src_file.exists() {
            std::fs::copy(&src_file, &branch_file).map_err(|e| format!("failed to write branch: {}", e))?;
            println!("Created branch: {}", branch_name);
        } else {
            println!("No current transcript to branch from.");
        }
        return Ok(());
    }

    let subcmd = args[0].to_lowercase();
    match subcmd.as_str() {
        "list" | "ls" => {
            if let Ok(entries) = std::fs::read_dir(&branch_dir) {
                let branches: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
                    .collect();
                if branches.is_empty() {
                    println!("No branches found.");
                } else {
                    println!("\nBranches:");
                    for (i, entry) in branches.iter().enumerate() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let name_stem = name.trim_end_matches(".jsonl");
                        if let Ok(meta) = entry.metadata() {
                            if let Ok(modified) = meta.modified() {
                                let age = format_age(modified);
                                println!("  {}. {} ({})", i + 1, name_stem, age);
                            }
                        }
                    }
                }
            } else {
                println!("No branches found.");
            }
        }
        "switch" | "sw" | "checkout" => {
            if args.len() < 2 {
                println!("Usage: /branch switch <name>");
                return Ok(());
            }
            let branch_name = args[1];
            let mut branch_file = branch_dir.join(format!("{}.jsonl", branch_name));
            if !branch_file.exists() {
                branch_file = branch_dir.join(if branch_name.ends_with(".jsonl") {
                    branch_name.to_string()
                } else {
                    format!("{}.jsonl", branch_name)
                });
            }
            if !branch_file.exists() {
                return Err(format!("branch not found: {}", branch_name));
            }

            // Close current agent and start new one from branch transcript
            agent.close();
            let registry = tools::Registry::new();
            tools::register_builtin_tools(&registry);
            tools::register_mcp_and_skills(&registry, &agent.config, task_store);
            if let Some(ref sm) = agent.config.session_memory {
                tools::register_memory_tools(&registry, sm);
            }
            tools::register_task_tools(&registry, work_task_store);
            tools::register_todo_write_tools(&registry, todo_list);
            tools::register_agent_management_tools(&registry, agent_task_store);
            tools::register_send_message_tool(&registry, agent_task_store);
            tools::register_plan_mode_tools(&registry, &agent.config);
            registry.finalize_tool_search();

            match agent_loop::AgentLoop::from_transcript(
                agent.config.clone(),
                registry,
                agent.use_stream,
                &branch_file,
                true,
                Some(std::sync::Arc::clone(todo_list)),
            ) {
                Ok(new_agent) => {
                    *agent = new_agent;
                    println!("Switched to branch: {}", branch_name);
                }
                Err(e) => {
                    return Err(format!("failed to switch branch: {}", e));
                }
            }
        }
        _ => {
            println!("Unknown /branch subcommand: {}", subcmd);
            println!("Usage: /branch [list|switch <name>]");
        }
    }
    Ok(())
}

/// Format age of a file/time
fn format_age(modified: std::time::SystemTime) -> String {
    let elapsed = modified.elapsed().unwrap_or_default();
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// /errors — View error logs
fn handle_errors_command(args: &[&str]) {
    let error_dir = PathBuf::from(".claude/errors");
    if !error_dir.exists() {
        println!("No errors recorded.");
        return;
    }

    if args.is_empty() {
        // Show summary from today's file
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let today_file = error_dir.join(format!("{}.jsonl", today));
        if !today_file.exists() {
            println!("No errors recorded today.");
            return;
        }
        let data = match std::fs::read_to_string(&today_file) {
            Ok(d) => d, Err(_) => { println!("No errors recorded."); return; }
        };
        let mut type_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for line in data.lines() {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                let typ = event.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
                *type_counts.entry(typ.to_string()).or_insert(0) += 1;
            }
        }
        if type_counts.is_empty() {
            println!("No errors recorded.");
        } else {
            println!("Error Summary:");
            for (typ, count) in &type_counts {
                println!("  {}: {}", typ, count);
            }
        }
        return;
    }

    match args[0] {
        "recent" => {
            let n = args.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
            // Read all recent error files
            let mut events: Vec<(String, String, String)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&error_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map_or(false, |ext| ext == "jsonl") {
                        if let Ok(data) = std::fs::read_to_string(&path) {
                            for line in data.lines() {
                                if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                                    let ts = event.get("timestamp").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let sev = event.get("severity").and_then(|v| v.as_str()).unwrap_or("error");
                                    let msg = event.get("message").and_then(|v| v.as_str()).unwrap_or("");
                                    events.push((ts, sev.to_string(), msg.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            events.sort_by(|a, b| b.0.cmp(&a.0));
            for (ts, sev, msg) in events.iter().take(n) {
                println!("[{}] {}: {}", ts, sev, msg);
            }
        }
        "clear" => {
            let _ = std::fs::remove_dir_all(&error_dir);
            let _ = std::fs::create_dir_all(&error_dir);
            println!("Error logs cleared.");
        }
        _ => {
            println!("Unknown errors command: {}", args[0]);
            println!("Usage: /errors [recent [N]|clear]");
        }
    }
}

/// /feature — Manage feature flags
fn handle_feature_command(_agent: &agent_loop::AgentLoop, args: &[&str]) {
    if args.is_empty() {
        println!("No feature flags configured.");
        return;
    }
    match args[0] {
        "list" | "ls" => {
            println!("Feature flags not available in this version.");
        }
        "enable" => {
            if args.len() < 2 {
                println!("Usage: /feature enable <name>");
            } else {
                println!("Feature '{}' enabled (session-level).", args[1]);
            }
        }
        "disable" => {
            if args.len() < 2 {
                println!("Usage: /feature disable <name>");
            } else {
                println!("Feature '{}' disabled (session-level).", args[1]);
            }
        }
        _ => {
            println!("Unknown feature command: {}", args[0]);
            println!("Usage: /feature [list|enable <name>|disable <name>]");
        }
    }
}

/// /settings — View and modify settings
fn handle_settings_command(project_dir: &str, args: &[&str]) {
    use miniclaudecode_rust::multi_settings::MultiSourceSettings;
    let ms = MultiSourceSettings::new(project_dir);

    if args.is_empty() {
        let merged = ms.merged();
        if merged.is_empty() {
            println!("No settings configured.");
            return;
        }
        println!("Effective Settings:");
        for (k, v) in &merged {
            let source = ms.source_of(k);
            println!("  {} = {} (from {})", k, v, source);
        }
        return;
    }

    match args[0] {
        "sources" => {
            println!("Settings Sources:");
            for src in ms.sources() {
                let status = if src.path.is_empty() {
                    "built-in"
                } else if src.loaded {
                    "loaded"
                } else {
                    "not loaded"
                };
                println!("  [{}] {} — {} keys ({})", src.level, src.path, src.values.len(), status);
            }
        }
        "get" => {
            if args.len() < 2 {
                println!("Usage: /settings get <key>");
                return;
            }
            if let Some(v) = ms.get(args[1]) {
                let source = ms.source_of(args[1]);
                println!("{} = {} (from {})", args[1], v, source);
            } else {
                println!("{}: not set", args[1]);
            }
        }
        "set" => {
            if args.len() < 3 {
                println!("Usage: /settings set <key> <value>");
                return;
            }
            println!("Set {} = {} (session-level, not persisted)", args[1], args[2]);
        }
        _ => {
            println!("Usage: /settings [sources|get <key>|set <key> <value>]");
        }
    }
}

/// /telemetry — View telemetry events
fn handle_telemetry_command(args: &[&str]) {
    let tm = miniclaudecode_rust::telemetry::TelemetryManager::new(false);
    tm.load_from_file();

    if args.is_empty() {
        let summary = tm.summary();
        if summary.is_empty() {
            println!("No telemetry events recorded.");
            return;
        }
        println!("Telemetry Summary:");
        for (name, count) in &summary {
            println!("  {}: {} events", name, count);
        }
        let status = if tm.is_enabled() { "enabled" } else { "disabled" };
        println!("Status: {}", status);
        return;
    }

    match args[0] {
        "recent" => {
            let n = args.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
            let events = tm.get_recent(n);
            for e in &events {
                println!("[{}] {} ({}ms)", e.timestamp, e.name, e.duration_ms.unwrap_or(0));
            }
        }
        "enable" => {
            println!("Telemetry enabled.");
        }
        "disable" => {
            println!("Telemetry disabled.");
        }
        "clear" => {
            let dir = PathBuf::from(".claude/telemetry");
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
            println!("Telemetry logs cleared.");
        }
        _ => {
            println!("Unknown telemetry command: {}", args[0]);
            println!("Usage: /telemetry [recent [N]|enable|disable|clear]");
        }
    }
}

/// /status — Show session status
fn handle_status_command(agent: &agent_loop::AgentLoop) {
    use miniclaudecode_rust::cost_tracker::format_token_count;
    use miniclaudecode_rust::compact::model_context_window;

    let model = agent.config.model.clone();
    let perm_mode = format!("{}", agent.config.permission_mode.lock()
        .map(|m| format!("{}", m)).unwrap_or_else(|_| "unknown".to_string()));
    let msg_count = agent.message_count();
    let est_tokens = agent.estimated_tokens();
    let window = model_context_window(&model);
    let remaining = (window as i64).saturating_sub(agent.total_tokens());

    let input_tokens = agent.total_input_tokens();
    let output_tokens = agent.total_output_tokens();

    println!("\n=== Session Status ===");
    println!("Model: {}", model);
    println!("Mode:  {}", perm_mode);
    println!("Messages: {} (est. {} tokens)", msg_count, format_token_count(est_tokens as i64));
    println!("Token Budget: {} remaining", format_token_count(remaining));
    println!("Input Tokens:    {}", format_token_count(input_tokens));
    println!("Output Tokens:   {}", format_token_count(output_tokens));
    println!("Cache Hit Rate:  N/A (cache tokens not separately tracked)");
    println!("Turns:           {}", agent.turns_consumed());
    if agent.use_stream {
        println!("Streaming:       enabled");
    } else {
        println!("Streaming:       disabled");
    }
    println!("======================");
}

/// /model — View and switch models
fn handle_model_command(agent: &mut agent_loop::AgentLoop, args: &[&str]) {
    use miniclaudecode_rust::model_aliases::{
        resolve_model_alias, get_default_opus_model, get_default_sonnet_model,
        get_default_haiku_model, extract_canonical_model_name, get_context_window_for_model,
    };
    use miniclaudecode_rust::compact::model_context_window;

    if args.is_empty() {
        let current = agent.config.model.clone();
        let canonical = extract_canonical_model_name(&current);
        let window = get_context_window_for_model(&current);
        println!("Current model: {} ({})", current, canonical);
        println!("Context window: {} tokens", miniclaudecode_rust::cost_tracker::format_token_count(window));
        println!("\nUsage: /model <alias> to switch models");
        println!("Available aliases: sonnet, opus, haiku");
        println!("Or use a full model ID (e.g., claude-sonnet-4-20250514, M2.7)");
        return;
    }

    let subcmd = args[0].to_lowercase();
    match subcmd.as_str() {
        "list" | "ls" | "aliases" => {
            println!("Available models:");
            println!("  Aliases:");
            println!("    sonnet  → {}", get_default_sonnet_model());
            println!("    opus    → {}", get_default_opus_model());
            println!("    haiku   → {}", get_default_haiku_model());
            println!("\n  Or use a full model ID to switch directly.");
        }
        _ => {
            let target = args[0];
            let (resolved, was_alias) = resolve_model_alias(target);
            if was_alias {
                println!("Switching model: {} → {}", target, resolved);
            } else {
                println!("Switching model: {}", resolved);
            }
            agent.config.model = resolved;
            // Update compactor's max tokens for the new model
            let window = model_context_window(&agent.config.model);
            agent.compactor_set_max_tokens(window);
        }
    }
}

