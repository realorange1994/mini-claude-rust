use anyhow::Result;
use clap::Parser;
use miniclaudecode_rust::agent_loop;
use miniclaudecode_rust::config::{load_config_from_file, Config};
use miniclaudecode_rust::permissions::PermissionMode;
use miniclaudecode_rust::tools;
use std::io::{self, Write};
use std::path::PathBuf;

const BANNER: &str = r#"
  ╔══════════════════════════════════════╗
  ║       miniClaudeCode v0.1.0         ║
  ║  Distilled Agent Loop Framework     ║
  ╚══════════════════════════════════════╝ 

  Type your message to start. Commands:
    /tools   -- list available tools
    /mode    -- show/change permission mode
    /help    -- show help
    /quit    -- exit
"#;

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
    #[arg(long, default_value_t = 30)]
    max_turns: usize,

    /// Enable streaming output
    #[arg(long, short)]
    stream: bool,

    /// Project directory
    #[arg(long)]
    dir: Option<PathBuf>,

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

    // Register all tools
    let registry = tools::Registry::new();
    tools::register_builtin_tools(&registry);
    tools::register_mcp_and_skills(&registry, &cfg);
    let agent = agent_loop::AgentLoop::new(cfg, registry, args.stream);

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
    println!("{}", BANNER);

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("\n> ");
        stdout.flush().unwrap();

        // Simple read_line - no BufReader. On Windows, stdin.lock() is reentrant,
        // so ask_user() can also read from stdin concurrently without conflict.
        let mut input = String::new();
        if stdin.read_line(&mut input).is_err() {
            println!("\nGoodbye!");
            agent.close();
            break;
        }

        let user_input = input.trim();
        if user_input.is_empty() {
            continue;
        }

        if user_input.starts_with('/') {
            let parts: Vec<&str> = user_input.split_whitespace().collect();
            let cmd = parts.first().unwrap_or(&"").to_lowercase();

            match cmd.as_str() {
                "/quit" | "/exit" | "/q" => {
                    println!("Goodbye!");
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
                        match *mode {
                            "ask" | "auto" | "plan" => {
                                agent.config.permission_mode = PermissionMode::from_str(mode);
                                println!("Mode changed to: {}", mode);
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
                "/help" => {
                    println!("{}", BANNER);
                    continue;
                }
                _ => {
                    println!("Unknown command: {}. Type /help for help.", cmd);
                    continue;
                }
            }
        }

        println!();
        let result = agent.run(user_input);
        println!("{}", result);
        println!();
    }
}
