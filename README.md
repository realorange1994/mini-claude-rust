# miniClaudeCode

A lightweight CLI coding assistant written in Rust, inspired by Anthropic's Claude Code. Implements the core agentic loop pattern with tool use, streaming output, multi-turn conversation, and robust context management.

## Overview

miniClaudeCode is a terminal-based AI coding assistant that connects to Anthropic-compatible APIs and provides a tool-use paradigm for software engineering tasks. It features an interactive REPL and one-shot mode, real-time streaming with thinking block support, multi-layered context compaction, a comprehensive permission system with an LLM-based security classifier, and extensive tooling for file operations, git, process management, web search, sub-agents, and more.

## Features

### Agent Loop

- Turn-based conversation loop with configurable max turn limits (default: 90)
- Iteration budget with grace-call support for final answers
- Continue reason tracking (NextTurn, PromptTooLong, MaxOutputTokens, ModelConfused)
- Both interactive REPL and one-shot command-line modes

### Streaming Output

- Real-time SSE streaming enabled by default; disable with `--no-stream`
- ThinkFilter state machine that handles `<thinking>`, `<think>`, and Anthropic extended thinking blocks
- Thinking content displayed in dim/gray styling
- Tool call display with argument accumulation
- StreamProgress tracking (TTFB, throughput)
- Transient error recovery with stream retry (up to 2 retries)
- Stall detection for hanging streams

### Context Compaction

Multi-layered context management with five compaction strategies:

- **Micro-compact**: Time-based tool result clearing -- replaces old tool outputs with placeholders while keeping recent results. Configurable via `micro_compact_keep_recent` (default: 5).
- **SM-compact**: Session memory compaction -- uses stored session memory as the compaction summary, skipping the LLM API call entirely.
- **LLM-driven compaction**: AI-powered summarization with structured 9-field summaries (Primary Request, Key Concepts, Files/Code, Errors/Fixes, Problem Solving, User Messages, Pending Tasks, Current Work, Next Steps).
- **Partial compact**: Directional compaction around a pivot index. Two modes: `UpTo` (compact everything before the pivot) and `From` (compact everything after the pivot).
- **Reactive compact**: Triggers compaction when token count spikes between turns (configurable threshold, default: 5000 token delta).
- 4-phase fallback degradation: RoundBased, TurnBased, SelectiveClear, Aggressive truncation when LLM compaction fails.
- Anti-thrashing and cooldown protection.
- 3-pass pre-pruning before LLM compaction: deduplicate tool results, summarize old tool results, truncate large tool arguments.
- Sensitive information redaction during compaction (API keys, passwords, tokens).
- Post-compact context restoration: re-injects recently read file contents.

### Session Memory

- Persistent structured notes across conversation turns
- Four categories: preference, decision, state, reference
- CRUD operations via `memory_add` and `memory_search` tools
- Disk persistence in `.claude/session_memory.md` with background flush loop (30s interval)
- Automatic deduplication (same category + content updates timestamp)
- Max 100 entries with oldest eviction
- Injected into system prompt for context continuity
- Used by SM-compact to skip LLM compaction API calls

### Context References

Inject external context directly into prompts:

- `@file:path[:start-end]` -- File content with optional line range
- `@folder:path` -- Directory listing (default depth: 3)
- `@staged` -- Git staged diff
- `@diff` -- Git unstaged diff
- `@git:N` -- Git commit diff (N = commit count or hash)
- `@url:URL` -- Web page content
- Token budget guardrails: 25% soft warning, 50% hard block
- File content cache with LRU eviction (max 100 entries)
- Sensitive directory protection (`.ssh`, `.aws`, `.gnupg`, `.kube`, etc.)

### Tool System

30+ built-in tools with argument type coercion and required parameter validation:

| Tool | Description |
|------|-------------|
| `exec` | Shell command execution with safety patterns and allowed/denied lists |
| `read_file` | Read file contents |
| `write_file` | Write/create files with CRLF handling |
| `edit_file` | Single-edit file operations with stale-file detection |
| `multi_edit` | Multiple edits in a single operation |
| `fileops` | File operations (copy, move, delete, chmod, symlink) |
| `glob` | File pattern matching/search |
| `grep` | Text search in files |
| `list_dir` | Directory listing |
| `git` | Full git operations with built-in dangerous operation detection (see below) |
| `system` | System info (uname, df, free, uptime, hostname, arch) |
| `process` | Process management (list, kill, pgrep, top, pstree) |
| `terminal` | tmux/screen session management |
| `runtime_info` | Rust runtime and system information |
| `brief` | Communication guidance for clear, concise responses |
| `web_search` | Web search via DuckDuckGo HTML scraping |
| `web_fetch` | Web page content fetching with built-in scraper |
| `exa_search` | Web search via Exa API |
| `agent` | Spawn sub-agents for complex multi-step tasks |
| `task_create` | Create structured tasks with dependency tracking |
| `task_list` | List all tasks with status, owner, and blocked-by info |
| `task_get` | Get detailed info for a specific task |
| `task_update` | Update task status, subject, metadata, or dependencies |
| `task_stop` | Kill a running background bash task by ID |
| `task_output` | Read output file from a background bash task (supports blocking wait) |
| `tool_search` | Search and discover available tools (deferred tool loading) |
| `memory_add` | Add a structured memory note (category + content) |
| `memory_search` | Search stored memory entries |
| `read_skill` | Load a skill's full SKILL.md instructions |
| `list_skills` | List all available skills |
| `search_skills` | Search skills by topic/tag |
| `list_mcp_tools` | List tools from connected MCP servers |
| `mcp_tool_call` | Call a tool from an MCP server |
| `mcp_server_status` | Check MCP server status |

**File History Tools** (13 dedicated tools with disk-persisted snapshots):

| Tool | Description |
|------|-------------|
| `file_history` | Snapshot, diff, rewind, restore, checkout, tag, annotate, search, timeline, batch operations |
| `file_history_read` | Read a file's snapshot at a specific version |
| `file_history_grep` | Search across file history |
| `file_history_diff` | Diff between file versions |
| `file_history_search` | Search file history by content |
| `file_history_summary` | Summarize file history |
| `file_history_timeline` | Timeline of file changes |
| `file_history_tag` | Tag a specific version |
| `file_history_annotate` | Annotate a version with notes |
| `file_history_batch` | Batch operations on file history |
| `file_history_checkout` | Checkout a file version |
| `file_restore` | Restore a file to a previous version |
| `file_rewind` | Rewind a file to a previous state |

All tools accept an optional `timeout` parameter (1-600 seconds, default 600).

### Git Tool: Dangerous Operation Detection

The `git` tool has built-in security checks that block destructive operations unconditionally, regardless of permission mode:

- **Force push**: `git push --force`, `git push -f`, `git push --force-with-lease`
- **Hard reset**: `git reset --hard`
- **Forced clean**: `git clean -f` / `git clean -fd`
- **History rewrite**: `git commit --amend`, `git rebase --interactive`
- **gh CLI**: `pr_merge`, `pr_close`, `issue_close`, `repo_delete`, `release_delete`, and other write/destructive `gh` subcommands

Safe operations (status, log, diff, show, branch, etc.) pass through without restriction.

### Sub-Agent System

The `agent` tool spawns sub-agents that run independently with their own filtered tool registries. Three built-in agent types:

- **`explore`** -- Read-only agent specialized in code search. Write tools (`write_file`, `edit_file`, `multi_edit`, `fileops`, `exec`, `terminal`, `git`) are denied.
- **`plan`** -- Read-only agent specialized in architecture planning. Same tool restrictions as `explore`.
- **`verify`** -- Adversarial verification agent. Can run commands but cannot write files.

Sub-agents can be spawned synchronously (blocking, result returned inline) or asynchronously (non-blocking, task ID returned for later retrieval). Key implementation details:

- 4-layer tool filtering: (1) global disallowed (`agent` always denied to prevent recursion), (2) async-specific disallowed, (3) agent type deny list, (4) caller-specified disallowed tools. Optional `allowed_tools` whitelist with `*` wildcard support.
- Sub-agents inherit the parent's model but can override with a `model` parameter. Max turns capped at 50.
- Sub-agents build their own system prompts with environment info, tool list, and agent-type-specific behavioral instructions.
- Sub-agents do not inherit session memory to avoid cross-agent interference.

### Permission Modes

Three permission modes for different use cases:

- **ASK** (default): Potentially dangerous operations (`exec`, `write_file`, `edit_file`, `multi_edit`, `fileops`) require user confirmation via single-key prompt. Safe commands from the allowed list bypass confirmation. Denied patterns are always blocked.
- **AUTO**: Operations are evaluated by an LLM-based security classifier (see below). Read-only tools are auto-allowed. If the classifier is unavailable, all operations are auto-approved (legacy behavior).
- **PLAN**: Only read-only operations allowed. Write operations are blocked. Read-only tools include: `read_file`, `grep`, `glob`, `list_dir`, `git`, `system`, `process`, `terminal`, `web_search`, `web_fetch`, `runtime_info`, etc.

Permission system features:
- Allowed commands list with prefix matching (e.g., `git status` allows `git status --short`)
- Denied patterns list (`rm -rf /`, `git push --force`, etc.)
- Shell metacharacter detection for command injection prevention
- Path safety checks (operations must stay within project directory)
- Stale file detection (file must be read before editing; edits blocked if file modified externally)

### Auto Mode Classifier

In AUTO mode, an LLM-based security classifier evaluates non-whitelisted tool calls before execution. This is modeled after Claude Code's upstream yolo-classifier.

**Auto-allowlisted tools** (always allowed without classifier evaluation):

- Read-only tools: `read_file`, `glob`, `grep`, `list_dir`, `tool_search`, `brief`, `runtime_info`
- Memory tools: `memory_add`, `memory_search`
- Task management: `task_create`, `task_list`, `task_get`, `task_update`
- MCP introspection: `list_mcp_tools`, `list_skills`, `search_skills`, `read_skill`, `mcp_server_status`

**Git operations** -- operation-level granularity:
- Read-only (auto-allowed): `info`, `status`, `log`, `diff`, `show`, `reflog`, `blame`, `describe`, `shortlog`, `ls-tree`, `rev-parse`, `rev-list`
- Write operations (`push`, `commit`, `merge`, `rebase`, etc.) go through the classifier

**Exec commands** -- command-level granularity with prefix matching:
- Safe prefixes (auto-allowed): `ls`, `cat`, `head`, `tail`, `wc`, `find`, `grep`, `rg`, `tree`, `stat`, `diff`, `go version`, `go env`, `go build`, `go test`, `go vet`, `go run`, `cargo build`, `cargo test`, `cargo check`, `cargo clippy`, `cargo run`, `npm test`, `npm run`, `make`, `cmake`, `ping`, `traceroute`, `ps`, `top`, `env`, `whoami`, `hostname`, `uname`, `date`, and more
- Dangerous patterns (always blocked): `rm`, `sudo`, `chmod`, `chown`, `mkfs`, `dd if=`, `curl ... | bash`, `wget ... | sh`, redirects to system directories
- Unknown commands go through the LLM classifier

**Process operations** -- operation-level granularity:
- Read-only (auto-allowed): `list`, `pgrep`, `top`, `pstree`, `ps`
- Destructive operations (`kill`, `pkill`) go through the classifier

**Classifier behavior**:
- Uses structured output via Anthropic's tool_use feature for reliable JSON responses
- Results are cached with a 5-minute TTL to avoid redundant LLM calls
- After consecutive denials exceeding the limit (default: 3), falls back to interactive manual approval
- Fail-open on API errors or unparseable responses (technical issues are not treated as security rejections)
- Fail-closed when the classifier is unavailable (no API key or model configured): blocks non-whitelisted tools

### Task Management

Two distinct task management systems:

**WorkTaskStore** -- LLM task tracking:
- Structured tasks with subject, description, active_form, status, owner, and arbitrary metadata
- Bidirectional dependency tracking (`blocks` / `blocked_by` edges maintained automatically)
- Cycle detection via BFS before adding new dependency edges
- Hash prefix normalization (`#1` to `1`) and integer-to-string coercion for LLM-friendly IDs
- Deleted tasks automatically clean up their references from all other tasks

**TaskStore** -- Background bash task lifecycle:
- Task states: `pending` to `running` to `completed` / `failed` / `killed`
- OS process tracking with PID for kill support (Unix: `kill -9`, Windows: `taskkill /F /T`)
- Automatic eviction after 30 seconds for completed/failed/killed tasks
- Background commands run via `exec` tool with `run_in_background=true`

### Skills System

Pluggable capability definitions stored as `SKILL.md` files in the `skills/` directory.

**Skill structure**:
```
skills/
  my-skill/
    SKILL.md
```

**SKILL.md format**:
```markdown
---
name: my-skill
description: What this skill does
always: false
version: 1.0
requires: [binary-name]
tags: [category, topic]
when_to_use: When the user wants to do X
---

Full skill instructions and guidance...
```

Features:
- Builtin skills (from exe directory) and workspace skills (from project `skills/` directory)
- Frontmatter-parsed metadata: name, description, always, version, requires, tags, when_to_use
- Dependency checking: binary requirements, environment variables, workspace files
- SkillTracker for progressive disclosure: tracks shown/read/used skills across turns
- Always-on skills injected into system prompt every turn
- Newly available skills announced per-turn with character budget limits
- Hot-reloading of modified skill files via `refresh_if_changed`

### MCP (Model Context Protocol) Support

Connect to MCP servers for extended tool capabilities. Two transport types:

- **stdio** (JSON-RPC 2.0 over stdin/stdout) for local servers
- **HTTP+SSE** for remote servers

Both transports are auto-detected from configuration: entries with `command` use stdio, entries with `url` use HTTP+SSE.

### File History Tracking

13 dedicated tools provide disk-persisted file versioning with snapshots stored in `.claude/snapshots/`. Supports snapshot, diff, rewind, restore, checkout, tag, annotate, search, timeline, and batch operations.

### Error Classification

15-category error taxonomy with precise retry logic:
- Transient (network, 5xx), ContextOverflow, ToolPairing, RateLimit, Billing, ModelNotFound, PayloadTooLarge, Overloaded, Timeout, FormatError, Auth, ThinkingSig, LongContextTier, MaxOutputTokens, ModelConfusion, Fatal
- Retry strategies with exponential backoff
- Partial response handling for max-output-tokens errors
- Model confusion detection (repeating patterns, empty responses)

### Crash Recovery and Transcripts

- Per-call transcript flush to JSONL files in `.claude/transcripts/`
- Session resume via `--resume` flag (by filename, number, or `last`)
- Role alternation repair on resume
- Tool pairing validation (orphaned tool_use/tool_result detection)
- Interactive transcript listing and selection via `/resume`

### API Optimizations

- **Prompt caching**: Anthropic-style cache control markers (system + 3 breakpoints) for KV cache reuse
- **CachedSystemPrompt**: Avoids rebuilding system prompt on every API call; dirty flag invalidates after compaction
- **Message normalization**: JSON key sorting and whitespace normalization for prefix caching
- **Rate limiting**: Response-header-based rate limit tracking with retry delay estimation

### Additional Features

- **CLAUDE.md support**: Project instructions from `CLAUDE.md` in the working directory are automatically loaded into the system prompt
- **Ctrl+C handling**: Single press interrupts current operation; double press (within 2s) exits with resume hint. Windows console stdin recovery after Ctrl+C breaks the handle.

## Installation and Build

### Prerequisites

- Rust toolchain (edition 2021)

### Build

```bash
cargo build --release
```

The binary will be at `target/release/miniclaudecode-rust`.

### Cross-Platform

Works on Windows, Linux, and macOS. The release profile enables LTO for optimized binaries:

```toml
[profile.release]
opt-level = 3
lto = true
```

## Usage

### Command-Line Options

```
Usage: miniclaudecode [OPTIONS] [MESSAGE]...

Arguments:
  [MESSAGE]...  Message to process (one-shot mode)

Options:
      --model <MODEL>            Anthropic model to use
      --api-key <API_KEY>        API key (overrides ANTHROPIC_API_KEY env and config file)
      --base-url <BASE_URL>      Custom API base URL
      --mode <MODE>              Permission mode (ask|auto|plan) [default: ask]
      --max-turns <MAX_TURNS>    Max agent loop turns per message [default: 90]
  -s, --stream                   Enable streaming output (default: true)
      --no-stream                Disable streaming output
      --dir <DIR>                Project directory
      --resume <RESUME>          Resume from a previous session (path, number, or 'last')
```

### One-Shot Mode

Process a single message and exit:

```bash
./target/release/miniclaudecode-rust "Explain the project structure"
```

### Interactive REPL

Start an interactive session:

```bash
./target/release/miniclaudecode-rust
```

### Permission Modes

```bash
# Ask mode (default) -- dangerous operations require confirmation
./target/release/miniclaudecode-rust --mode ask

# Auto mode -- LLM classifier evaluates non-whitelisted operations
./target/release/miniclaudecode-rust --mode auto

# Plan mode -- read-only operations only
./target/release/miniclaudecode-rust --mode plan
```

### Other Examples

```bash
# Specify model
./target/release/miniclaudecode-rust --model claude-sonnet-4-20250514

# Specify project directory
./target/release/miniclaudecode-rust --dir /path/to/project

# Resume a previous session
./target/release/miniclaudecode-rust --resume last

# Disable streaming output
./target/release/miniclaudecode-rust --no-stream

# Override API key
./target/release/miniclaudecode-rust --api-key your-key

# Override base URL
./target/release/miniclaudecode-rust --base-url https://your-proxy.com

# Set max turns
./target/release/miniclaudecode-rust --max-turns 50
```

### Slash Commands (Interactive Mode)

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/compact` | Force context compaction (LLM-driven) |
| `/partialcompact [up_to\|from] [pivot]` | Partial compact with optional direction and pivot index |
| `/clear` | Clear conversation history and read-file tracking |
| `/mode [ask\|auto\|plan]` | Switch permission mode |
| `/resume [session]` | Resume a previous session (lists transcripts if no argument) |
| `/tools` | List all available tools with descriptions |
| `/quit` (or `/exit`, `/q`) | Exit the REPL |

Unknown `/xxx` commands are passed through as regular prompt text.

### Context References

Inject external context directly into your prompt:

```
Read the main module @file:src/main.rs and check the staged changes @staged
```

## Configuration

### Priority Order

Configuration is resolved in this priority order (highest to lowest):

1. Command-line flags (`--model`, `--api-key`, `--base-url`, `--mode`)
2. Environment variables
3. Project `.claude/settings.json`
4. Home `~/.claude/settings.json`
5. Default values

### Environment Variables

```bash
export ANTHROPIC_API_KEY="your-api-key"       # or ANTHROPIC_AUTH_TOKEN
export ANTHROPIC_BASE_URL="https://api.anthropic.com"
export ANTHROPIC_MODEL="claude-sonnet-4-20250514"
```

The `agent` tool accepts an optional `model` parameter to override the model for a specific sub-agent.

### Settings File

Configuration is stored in `.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "your-api-key",
    "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
    "ANTHROPIC_MODEL": "claude-sonnet-4-20250514"
  },
  "mcp": {
    "servers": {
      "my-server": {
        "command": "npx",
        "args": ["-y", "@some-mcp-server"]
      }
    }
  }
}
```

### MCP Configuration

MCP servers can also be configured via `.mcp.json`:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
    },
    "remote-server": {
      "url": "https://your-mcp-server.com/mcp"
    }
  }
}
```

### CLAUDE.md

Place a `CLAUDE.md` file in your project root. Its contents will be automatically loaded and injected into the system prompt as project-specific instructions.

### Compaction Configuration

| Setting | Default | Description |
|---------|---------|-------------|
| `auto_compact_enabled` | `true` | Enable automatic compaction |
| `auto_compact_threshold` | `0.75` | Trigger at 75% of effective context window |
| `auto_compact_buffer` | `13,000` | Reserved buffer tokens |
| `max_compact_output_tokens` | `20,000` | Max tokens for compact API response |
| `micro_compact_enabled` | `true` | Enable micro-compaction |
| `micro_compact_keep_recent` | `5` | Number of recent tool results to keep |
| `reactive_compact_threshold` | `5,000` | Token delta spike to trigger reactive compact |
| `post_compact_recover_files` | `true` | Re-inject recently read files after compaction |
| `post_compact_max_files` | `5` | Max files to restore post-compact |
| `post_compact_max_file_chars` | `50,000` | Max chars per file for restoration |

### Allowed Commands and Denied Patterns

Default allowed commands (bypass confirmation in ASK mode):
`ls`, `cat`, `head`, `tail`, `wc`, `find`, `grep`, `rg`, `git status`, `git diff`, `git log`, `git branch`, `python`, `python3`, `pip`, `npm`, `node`, `echo`, `pwd`, `which`, `env`, `date`

Default denied patterns (always blocked):
`rm -rf /`, `rm -rf ~`, `sudo rm`, `git push --force`, `git reset --hard`, `> /dev/sda`, `mkfs`, `dd if=`

### Auto Mode Classifier Configuration

| Setting | Default | Description |
|---------|---------|-------------|
| `auto_classifier_enabled` | `true` | Enable LLM classifier in auto mode |
| `auto_classifier_model` | (same as main model) | Model used for classification |
| `auto_classifier_max_tokens` | `128` | Max tokens for classifier response |
| `auto_denial_limit` | `3` | Consecutive denials before fallback to manual approval |

## Architecture

```
miniClaudeCode-rust/
  src/
    main.rs                  # Entry point, CLI args, REPL loop
    lib.rs                   # Library root / module declarations
    agent_loop.rs            # Core agent loop with turn limits, transcript logging
    agent_sub.rs             # Sub-agent spawning (agent types, filtered registries, prompt builders)
    streaming.rs             # SSE parsing, ThinkFilter, CollectHandler, TerminalHandler, stall detection
    context.rs               # Conversation context, message types, tool pairing, role alternation
    context_references.rs    # @ reference expansion (file, folder, git, url)
    compact.rs               # Multi-layered compaction (micro, SM, LLM, partial, reactive)
    config.rs                # Configuration loading, CachedSystemPrompt, build_system_prompt
    permissions.rs           # PermissionMode (Ask/Auto/Plan), PermissionGate, denied patterns
    auto_classifier.rs       # LLM-based security classifier with operation-level allowlist
    error_types.rs           # 15-category error taxonomy with retry strategies
    session_memory.rs        # Persistent session memory with disk persistence
    filehistory.rs           # File version history and snapshots
    prompt_caching.rs        # Anthropic prompt caching with cache control markers
    normalize.rs             # API message normalization for KV cache reuse
    rate_limit.rs            # Rate limit tracking from response headers
    retry_utils.rs           # Retry utilities with exponential backoff
    transcript_builder.rs    # Compact transcript builder for classifier context
    task_store.rs            # TaskStore for background bash tasks (registration, kill, eviction)
    work_task.rs             # WorkTaskStore for LLM task management (dependencies, cycle detection)
    tools/
      mod.rs                 # ToolResult, ToolResultMetadata, Registry, path safety
      coercion.rs            # Argument type coercion
      exec_tool.rs           # Shell command execution + background bash task system
      file_read.rs           # File reading
      file_write.rs          # File writing
      file_edit.rs           # Single-file editing
      multi_edit.rs          # Multi-file editing
      fileops.rs             # File operations (copy, move, delete, etc.)
      glob_tool.rs           # File pattern matching
      grep_tool.rs           # Text search
      list_dir.rs            # Directory listing
      git_tool.rs            # Git operations with dangerous operation detection
      system_tool.rs         # System information
      process.rs             # Process management
      terminal_tool.rs       # tmux/screen management
      runtime_info.rs        # Rust runtime info
      brief_tool.rs          # Communication guidance tool
      web_search.rs          # Web search (DuckDuckGo)
      web_fetch.rs           # Web page fetching
      exa_search.rs          # Web search (Exa API)
      mcp_tools.rs           # MCP tool integration
      skill_tools.rs         # Skill tools (read, list, search)
      file_history_tools.rs  # 13 file history tools
      memory_tool.rs         # Session memory tools
      task_tool.rs           # Task management tools (create, list, get, update, stop)
      agent_tool.rs          # Sub-agent spawning tool
      tool_search_tool.rs    # Tool discovery / deferred tool loading
    skills/
      mod.rs                 # Loader, SkillMeta, SkillInfo, frontmatter parsing
      tracker.rs             # SkillTracker (shown/read/used tracking)
    mcp/
      mod.rs                 # Manager (register, start, list servers)
      client.rs              # stdio + HTTP+SSE transport client
    transcript/
      mod.rs                 # Transcript, Entry types, resume support
  Cargo.toml
```

## Compatibility

Works with Anthropic API and compatible endpoints. Tested with:

- Anthropic Claude models (sonnet-4-20250514, opus-4-20250514, haiku-4-5, etc.)
- OpenAI-compatible proxies
- MiniMax models (via compatible proxy)

## License

MIT
