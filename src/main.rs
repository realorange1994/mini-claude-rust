use anyhow::Result;
use clap::Parser;
use miniclaudecode_rust::agent_loop;
use miniclaudecode_rust::config::{load_config_from_file, Config};
use miniclaudecode_rust::permissions::PermissionMode;
use miniclaudecode_rust::tools;
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

    /// Permission mode (ask|auto|plan)
    #[arg(long, default_value = "ask")]
    mode: String,

    /// Max agent loop turns per message
    #[arg(long, default_value_t = 90)]
    max_turns: usize,

    /// Enable streaming output
    #[arg(long, short)]
    stream: bool,

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
    // Suppress tokio runtime shutdown panic message
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            return;
        };
        if msg.contains("Cannot drop a runtime in a context where blocking is not allowed") {
            return;  // Suppress this specific panic
        }
        eprintln!("thread panicked: {}", msg);
    }));

    let args = Args::parse();

    // Priority: flags > env > .claude/settings.json > defaults
    let mut cfg = Config::default();

    // Load from .claude/settings.json and .mcp.json
    if let Some(project_dir) = args.dir.clone().or_else(|| std::env::current_dir().ok()) {
        if let Err(e) = std::env::set_current_dir(&project_dir) {
            eprintln!("[!] Failed to change working directory to {}: {}", project_dir.display(), e);
        }
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
    cfg.permission_mode = PermissionMode::from_str(&args.mode);
    cfg.max_turns = args.max_turns;

    // Validate: model is required
    if cfg.model.is_empty() {
        eprintln!("[!] No model specified. Set it via --model flag, ANTHROPIC_MODEL env, or model in .claude/settings.json");
        std::process::exit(1);
    }

    // Always initialize file history (with disk persistence) — shared Arc between tools and agent_loop
    use miniclaudecode_rust::filehistory::FileHistory;
    use std::sync::Arc;
    let snapshots_dir = std::env::current_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("snapshots");
    cfg.file_history = Some(Arc::new(FileHistory::new_with_dir(&snapshots_dir)));

    // Register all tools
    let registry = tools::Registry::new();
    tools::register_builtin_tools(&registry);
    tools::register_mcp_and_skills(&registry, &cfg);

    // Handle --resume flag
    let resume_path = args.resume.as_ref().map(|s| find_transcript(s)).flatten();

    let agent = if let Some(transcript_path) = resume_path {
        // Create a new registry for the resumed session
        let resume_registry = tools::Registry::new();
        tools::register_builtin_tools(&resume_registry);
        tools::register_mcp_and_skills(&resume_registry, &cfg);

        match agent_loop::AgentLoop::from_transcript(cfg.clone(), resume_registry, args.stream, &transcript_path, true) {
            Ok(agent) => {
                println!("[+] Resumed session from: {}", transcript_path.display());
                agent
            }
            Err(e) => {
                eprintln!("[!] Failed to resume: {}. Starting new session.", e);
                agent_loop::AgentLoop::new(cfg, registry, args.stream)
            }
        }
    } else {
        agent_loop::AgentLoop::new(cfg, registry, args.stream)
    };

    // One-shot mode
    if let Some(message) = args.message {
        let prompt = message.join(" ");
        let result = agent.run(&prompt);
        println!("{}", result);
        agent.close();
        return Ok(());
    }

    // Interactive REPL
    run_interactive(agent);
    Ok(())
}

fn run_interactive(mut agent: agent_loop::AgentLoop) {
    // Get transcript filename for resume hint (strip .jsonl extension)
    let transcript_stem = agent.transcript_filename().trim_end_matches(".jsonl");

    // Track Ctrl+C presses for double-press exit
    let last_ctrlc = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
    // Use the agent's interrupted flag directly
    let interrupted = agent.interrupted_flag();

    let last_ctrlc_clone = last_ctrlc.clone();
    let transcript_for_signal = transcript_stem.to_string();
    let interrupted_clone = interrupted.clone();

    ctrlc::set_handler(move || {
        let now = std::time::Instant::now();
        let mut last = last_ctrlc_clone.lock().unwrap();

        // Check if this is a double press within 2 seconds
        if let Some(last_time) = *last {
            if now.duration_since(last_time).as_secs() < 2 {
                // Double press - exit immediately
                println!("\n\nExiting!");
                println!("To resume this session: --resume {}", transcript_for_signal);
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
        stdout.flush().unwrap();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => {
                // EOF — stdin was closed
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

        // Check for exact command match — only treat as command if the first
        // word is a known command. Unknown /xxx is passed through as prompt text.
        let is_known_cmd = if user_input.starts_with('/') {
            let parts: Vec<&str> = user_input.split_whitespace().collect();
            let cmd = parts.first().unwrap_or(&"").to_lowercase();
            matches!(cmd.as_str(), "/quit" | "/exit" | "/q" | "/tools" | "/mode" | "/help" | "/resume" | "/compact" | "/clear")
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
                            "ask" | "auto" | "plan" => {
                                agent.config.permission_mode = PermissionMode::from_str(&mode_lower);
                                println!("Mode changed to: {}", mode_lower);
                            }
                            _ => {
                                println!("Unknown mode: {}", mode);
                            }
                        }
                    } else {
                        println!("Current mode: {}", agent.config.permission_mode);
                        println!("Usage: /mode [ask|auto|plan]");
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
                    println!("  /help    — Show available commands");
                    println!("  /compact — Force context compaction");
                    println!("  /clear   — Clear conversation history");
                    println!("  /mode    — Switch permission mode (ask|auto|plan)");
                    println!("  /resume  — Resume a previous session");
                    println!("  /tools   — List available tools");
                    println!("  /quit    — Exit");
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
                        tools::register_mcp_and_skills(&registry, &agent.config);

                        match agent_loop::AgentLoop::from_transcript(
                            agent.config.clone(),
                            registry,
                            agent.use_stream,
                            &transcript_path.unwrap(),
                            true,
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
                _ => {
                    println!("Unknown command: {}", cmd);
                }
            }
        }

        println!();
        let result = agent.run(user_input);
        if !agent.use_stream {
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
