//! Sub-agent spawning system — creates child AgentLoops with filtered tools and isolated context.

use crate::agent_loop::AgentLoop;
use crate::config::Config;
use crate::tools::Registry;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    set.insert("agent"); // no recursive agent spawning
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

    // Permission mode (sub-agents use ASK mode by default)
    sb.push_str("## Permission Mode: ASK\n\n");

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
    sb.push_str("- Agent threads always have their cwd reset between bash calls -- only use absolute file paths.\n");
    sb.push_str("- Be thorough but efficient -- avoid redundant reads or searches.\n\n");

    // Security section
    sb.push_str("## Security\n");
    sb.push_str("- You are a sub-agent with limited access.\n");
    sb.push_str("- Do not attempt to modify system configuration or security settings.\n");
    sb.push_str("- If you encounter sensitive data, report it but do not store it.\n");
    sb.push_str("- Follow the principle of least privilege.\n");

    sb
}

/// Build a child Config from the parent config with sub-agent overrides.
pub fn build_child_config(parent_config: &Config, model_override: &str) -> Config {
    let mut child_config = parent_config.clone();
    if !model_override.is_empty() {
        child_config.model = model_override.to_string();
    }
    // Limit child agent turns
    let max_turns = if child_config.max_turns > 0 {
        child_config.max_turns.min(50) // Cap sub-agents at 50 turns
    } else {
        50 // sensible default for sub-agents
    };
    child_config.max_turns = max_turns;
    // Sub-agents don't need session memory
    child_config.session_memory = None;

    child_config
}

/// Generate a new agent ID.
pub fn generate_agent_id() -> String {
    format!("agent-{}", generate_short_id())
}

/// Spawn a sub-agent and return its result.
///
/// This is the synchronous path — it blocks until the child agent completes.
/// Designed to be called from within a tool's execute() method.
pub fn spawn_sub_agent_sync(
    parent_config: &Config,
    parent_registry: &Registry,
    prompt: &str,
    subagent_type: &str,
    model: &str,
    run_in_background: bool,
    allowed_tools: &[String],
    disallowed_tools: &[String],
    _inherit_context: bool,
) -> (String, String, String, usize, u64) {
    let start = std::time::Instant::now();

    let agent_id = generate_agent_id();

    // Build child config and registry
    let child_config = build_child_config(parent_config, model);
    let child_registry = build_child_registry(
        parent_registry,
        subagent_type,
        allowed_tools,
        disallowed_tools,
        run_in_background,
    );

    let child_sys_prompt = build_sub_agent_system_prompt(
        &child_registry,
        &child_config.model,
        subagent_type,
    );

    // Async path: spawn on a separate thread and return immediately
    if run_in_background {
        let config = child_config.clone();
        let registry = child_registry.clone_for_spawn();
        let prompt_owned = prompt.to_string();
        let sys_prompt_owned = child_sys_prompt;

        std::thread::spawn(move || {
            match AgentLoop::new_for_sub_agent(config, registry, &sys_prompt_owned) {
                Ok(child_loop) => {
                    let _result = child_loop.run(&prompt_owned);
                }
                Err(e) => {
                    eprintln!("[agent] Failed to spawn background agent: {}", e);
                }
            }
        });

        return (
            agent_id.clone(),
            format!("Agent launched in background.\n\nagentId: {}\nStatus: async_launched", agent_id),
            String::new(),
            0,
            start.elapsed().as_millis() as u64,
        );
    }

    // Synchronous path: run the child agent loop
    match AgentLoop::new_for_sub_agent(child_config, child_registry, &child_sys_prompt) {
        Ok(child_loop) => {
            let result = child_loop.run(prompt);

            // Recover partial results if Run returned empty
            let final_result = if result.is_empty() {
                child_loop.get_partial_result()
            } else {
                result
            };

            let tools_used = child_loop.tools_used_count();
            let duration_ms = start.elapsed().as_millis() as u64;
            (agent_id, final_result, String::new(), tools_used, duration_ms)
        }
        Err(e) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            (agent_id, String::new(), format!("failed to create sub-agent: {e}"), 0, duration_ms)
        }
    }
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

Guidelines:
- Use glob for broad file pattern matching (e.g., "src/**/*.rs", "**/*.json")
- Use grep for searching file contents with regex
- Use read_file when you know the specific file path you need to read
- Use Bash ONLY for read-only operations (ls, git status, git log, git diff, find, grep, cat, head, tail)
- NEVER use Bash for: mkdir, touch, rm, cp, mv, git add, git commit, npm install, pip install, or any file creation/modification
- Adapt your search approach based on the thoroughness level specified by the caller
- Make efficient use of tools: be smart about how you search - spawn multiple parallel tool calls where possible
- Communicate your final report directly as a regular message - do NOT attempt to create files

Be thorough but efficient. In order to achieve speed:
- Make parallel tool calls wherever possible
- Start with broad searches (glob) to narrow down, then read specific files
- Avoid redundant reads or searches

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
}
