# miniClaudeCode-rust

A lightweight implementation of Claude Code's agentic loop framework written in Rust.

## Overview

miniClaudeCode-rust is a minimal AI coding assistant that implements the core agentic loop pattern similar to Anthropic's Claude Code. It provides a tool-use paradigm where an LLM can execute various tools to accomplish software engineering tasks, with robust error handling, context management, crash recovery, and crash-resumable sessions.

## Features

### Agent Loop
- Turn-based conversation loop with configurable max turn limits (default: 90)
- Iteration budget with grace-call support for final answers
- Continue reason tracking (NextTurn, PromptTooLong, MaxOutputTokens, ModelConfused)
- Both interactive REPL and one-shot modes

### Streaming Support
- Real-time SSE streaming output enabled by default; disable with `--no-stream` flag
- ThinkFilter state machine that filters `<thinking>`/`</thinking>`, `<think>`/`</think>`, and Anthropic extended thinking blocks
- Streaming output displayed in dim/gray styling for thinking content
- Tool call display with argument accumulation
- StreamProgress tracking (TTFB, throughput)
- Transient error recovery with stream retry (up to 2 retries)
- Stall detection for hanging streams
- Partial tool call cleanup on retry

### Context Compaction
Multi-layered context management system with five compaction strategies:

- **Micro-compact**: Time-based tool result clearing -- replaces old tool outputs with placeholders while keeping recent results intact. Configurable via `micro_compact_keep_recent` (default: 5).
- **SM-compact**: Session memory compaction -- uses stored session memory as the compaction summary, skipping the expensive LLM API call entirely. Follows the official Claude Code approach.
- **LLM-driven compaction**: AI-powered summarization via API call with structured 9-field summaries (Primary Request, Key Concepts, Files/Code, Errors/Fixes, Problem Solving, User Messages, Pending Tasks, Current Work, Next Steps). Supports iterative updates with previous summaries.
- **Partial compact**: Directional compaction around a pivot index. Two modes:
  - `UpTo`: Compact everything before the pivot (keeps recent context)
  - `From`: Compact everything after the pivot (keeps early context + recent tail)
- **Reactive compact**: Proactively triggers compaction when token count spikes between turns (configurable threshold, default: 5000 token delta).
- **4-phase fallback degradation**: RoundBased -> TurnBased -> SelectiveClear -> Aggressive truncation when LLM compaction fails.
- Anti-thrashing protection: Skips compaction if recent savings were <10%.
- Cooldown protection: Prevents immediate re-compaction until 25% token growth.
- 3-pass pre-pruning before LLM compaction: deduplicate tool results, summarize old tool results, truncate large tool arguments.
- Sensitive information redaction during compaction (API keys, passwords, tokens, etc.).
- Post-compact context restoration: re-injects recently read file contents.

### Session Memory
- Persistent structured notes across conversation turns
- Four categories: preference, decision, state, reference
- CRUD operations: add notes, get all notes, search notes
- Disk persistence in `.claude/session_memory.md` with background flush loop (30s interval)
- Automatic deduplication (same category+content updates timestamp)
- Max entries limit (100) with oldest eviction
- Injected into system prompt for context continuity
- SM-compact leverages session memory to skip LLM compaction API calls

### @ Context References
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
| `git` | Full git operations (clone, commit, push, pull, branch, merge, rebase, stash, worktree, etc.) |
| `system` | System info (uname, df, free, uptime, hostname, arch) |
| `process` | Process management (list, kill, pgrep, top, pstree) |
| `terminal` | tmux/screen session management |
| `runtime_info` | Rust runtime and system information |
| `web_search` | Web search via DuckDuckGo HTML scraping |
| `web_fetch` | Web page content fetching with built-in scraper |
| `exa_search` | Web search via Exa API |
| `agent` | Spawn sub-agents for complex multi-step tasks (sync or async). Supports typed agents: `explore` (read-only code search), `plan` (read-only architecture planning), `verify` (adversarial verification). Filtered tool registries per agent type. |
| `task_create` | Create structured tasks with dependency tracking |
| `task_list` | List all tasks with status, owner, and blocked-by info |
| `task_get` | Get detailed info for a specific task |
| `task_update` | Update task status, subject, metadata, or dependencies (bidirectional blocks/blocked_by with cycle detection) |
| `task_stop` | Kill a running background bash task by ID (OS-level process kill). Use `task_update` with `status: "deleted"` to delete work tasks. |
| `task_output` | Read output file from a background bash task (supports blocking wait) |

**File History Tools** (13 dedicated tools with disk-persisted snapshots):
| Tool | Description |
|------|-------------|
| `file_history` | Snapshot, diff, rewind, restore, checkout, tag, annotate, search, timeline, batch operations |
| `file_history_read` | Read a file's snapshot at a specific version |
| `file_history_grep` | Search across file history |
| `file_restore` | Restore a file to a previous version |
| `file_rewind` | Rewind a file to a previous state |
| `file_history_diff` | Diff between file versions |
| `file_history_search` | Search file history by content |
| `file_history_summary` | Summarize file history |
| `file_history_timeline` | Timeline of file changes |
| `file_history_tag` | Tag a specific version |
| `file_history_annotate` | Annotate a version with notes |
| `file_history_batch` | Batch operations on file history |
| `file_history_checkout` | Checkout a file version |

**Session Memory Tools**:
| Tool | Description |
|------|-------------|
| `memory_add` | Add a structured memory note (category + content) |
| `memory_search` | Search stored memory entries |

**Skill Tools**:
| Tool | Description |
|------|-------------|
| `read_skill` | Load a skill's full SKILL.md instructions |
| `list_skills` | List all available skills |
| `search_skills` | Search skills by topic/tag |

**MCP Tools** (when MCP servers configured):
| Tool | Description |
|------|-------------|
| `list_mcp_tools` | List tools from connected MCP servers |
| `mcp_tool_call` | Call a tool from an MCP server |
| `mcp_server_status` | Check MCP server status |

All tools accept an optional `timeout` parameter (1-300 seconds, default 30).

### Skills System
- Extensible skill loader reading `SKILL.md` files from `skills/` directory
- Builtin skills (from exe directory) and workspace skills (from project `skills/` directory)
- Frontmatter-parsed skill metadata: name, description, always, version, requires, tags, when_to_use
- Dependency checking: binary requirements, environment variables, workspace files
- SkillTracker for progressive disclosure across turns: tracks shown/read/used skills
- Always-on skills injected into system prompt every turn
- Newly available skills announced per-turn with character budget limits
- `refresh_if_changed` for hot-reloading modified skill files
- Discovery reminder when skills remain unread

### Permission Modes
Three permission modes for different use cases:

- **ASK** (default): Potentially dangerous operations (exec, write_file, edit_file, multi_edit, fileops) require user confirmation via single-key prompt. Safe commands from the allowed list bypass confirmation.
- **AUTO**: All operations are auto-approved (use with caution). Denied patterns are still enforced.
- **PLAN**: Only read-only operations allowed. Write operations are blocked. Read-only tools include: read_file, grep, glob, list_dir, git, system, process, terminal, web_search, web_fetch, runtime_info, etc.

Permission system features:
- Allowed commands list with prefix matching (e.g., `git status` allows `git status --short`)
- Denied patterns list (`rm -rf /`, `git push --force`, etc.)
- Shell metacharacter detection for command injection prevention
- Path safety checks (operations must stay within project directory)
- Stale file detection (file must be read before editing; edits blocked if file modified externally)

### Error Classification
15-category error taxonomy with precise retry logic:
- Transient (network, 5xx), ContextOverflow, ToolPairing, RateLimit, Billing, ModelNotFound, PayloadTooLarge, Overloaded, Timeout, FormatError, Auth, ThinkingSig, LongContextTier, MaxOutputTokens, ModelConfusion, Fatal
- Retry strategies with exponential backoff
- Partial response handling for max-output-tokens errors
- Model confusion detection (repeating patterns, empty responses)

### Crash Recovery & Transcripts
- Per-call transcript flush to JSONL files in `.claude/transcripts/`
- Session resume via `--resume` flag (by filename, number, or `last`)
- Role alternation repair on resume
- Tool pairing validation (orphaned tool_use/tool_result detection)
- Truncated line handling
- Interactive transcript listing and selection via `/resume`

### API Optimizations
- **Prompt caching**: Anthropic-style cache control markers (system + 3 breakpoints) for KV cache reuse
- **CachedSystemPrompt**: Avoids rebuilding system prompt on every API call; dirty flag invalidates after compaction
- **Message normalization**: JSON key sorting and whitespace normalization for prefix caching
- **Rate limiting**: Response-header-based rate limit tracking with retry delay estimation

### CLAUDE.md Support
- Project instructions from `CLAUDE.md` in the working directory are automatically loaded and injected into the system prompt

### Ctrl+C Handling
- Single press: Interrupts current operation with option to continue working
- Double press (within 2s): Exits immediately with resume hint
- Console stdin recovery on Windows after Ctrl+C breaks the handle

## Installation & Build

### Prerequisites

- Rust toolchain (edition 2021)

### Build

```bash
cargo build --release
```

The binary will be at `target/release/miniclaudecode-rust`.

## Usage

### Command-Line Options

```bash
# Interactive mode (streaming enabled by default)
./target/release/miniclaudecode-rust

# Disable streaming output
./target/release/miniclaudecode-rust --no-stream

# Specify permission mode
./target/release/miniclaudecode-rust --mode ask

# Specify model
./target/release/miniclaudecode-rust --model claude-sonnet-4-20250514

# Specify project directory
./target/release/miniclaudecode-rust --dir /path/to/project

# Resume a previous session
./target/release/miniclaudecode-rust --resume last

# One-shot mode (process a single message and exit)
./target/release/miniclaudecode-rust "Explain the project structure"

# Override API key
./target/release/miniclaudecode-rust --api-key your-key

# Override base URL
./target/release/miniclaudecode-rust --base-url https://your-proxy.com

# Set max turns
./target/release/miniclaudecode-rust --max-turns 50
```

### Slash Commands (in interactive mode)

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

### @ Context References

Inject external context directly into your prompt:

```
Read the main module @file:src/main.rs and check the staged changes @staged
```

Supported references:
- `@file:path[:start-end]` -- File content with optional line range
- `@folder:path` -- Directory listing
- `@staged` -- Git staged diff
- `@diff` -- Git unstaged diff
- `@git:N` -- Git commit diff
- `@url:URL` -- Web page content

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
export ANTHROPIC_API_KEY="your-api-key"     # or ANTHROPIC_AUTH_TOKEN
export ANTHROPIC_BASE_URL="https://api.anthropic.com"
export ANTHROPIC_MODEL="claude-sonnet-4-20250514"  # Default model
```

The `agent` tool accepts an optional `model` parameter to override the model for a specific sub-agent.

### Settings File

Configuration is stored in `.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "your-api-key",
    "ANTHROPIC_BASE_URL": "https://api.anthropics.com",
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

MCP transport: stdio (JSON-RPC 2.0 over stdin/stdout) for local servers, HTTP+SSE for remote servers. Both transports are auto-detected from configuration -- entries with `command` use stdio, entries with `url` use HTTP+SSE.

### CLAUDE.md

Place a `CLAUDE.md` file in your project root. Its contents will be automatically loaded and injected into the system prompt as project-specific instructions.

### Compaction Configuration (defaults)

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

### Allowed Commands & Denied Patterns

Default allowed commands (bypass confirmation in ASK mode):
`ls`, `cat`, `head`, `tail`, `wc`, `find`, `grep`, `rg`, `git status`, `git diff`, `git log`, `git branch`, `python`, `python3`, `pip`, `npm`, `node`, `echo`, `pwd`, `which`, `env`, `date`

Default denied patterns (always blocked):
`rm -rf /`, `rm -rf ~`, `sudo rm`, `git push --force`, `git reset --hard`, `> /dev/sda`, `mkfs`, `dd if=`

## Session Memory

Session Memory provides persistent structured notes across conversation turns, stored in `.claude/session_memory.md`.

### Memory Categories
- **preference**: User preferences and settings
- **decision**: Architectural or implementation decisions made
- **state**: Current state of the project or task
- **reference**: Reference information to remember

### Memory Tools
The model can use `memory_add` and `memory_search` tools to manage session memory. Notes are automatically included in the system prompt and can be used as compaction summaries (SM-compact), avoiding expensive LLM compaction API calls.

### Persistence
- Memory is flushed to disk every 30 seconds via a background thread
- Final flush occurs on shutdown
- Memory survives session restart via `--resume`

## Skills System

Skills are pluggable capability definitions stored as `SKILL.md` files in the `skills/` directory.

### Skill Structure
```
skills/
  my-skill/
    SKILL.md
```

### SKILL.md Format
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

### Skill Discovery
The model is instructed to use `search_skills` to find relevant skills before attempting alternative approaches. Skills are progressively disclosed:
- Always-on skills appear in system prompt every turn
- New skills are announced per-turn (with character budget limits)
- Once shown, skills are not re-announced
- Full instructions loaded via `read_skill` tool

## Architecture

```
miniClaudeCode-rust/
├── src/
│   ├── main.rs              # Entry point, CLI args, and REPL
│   ├── agent_loop.rs        # Core agent loop with turn limits, transcript logging
│   ├── agent_sub.rs         # Sub-agent spawning system (agent types, filtered registries, prompt builders)
│   ├── streaming.rs         # SSE parsing, ThinkFilter, CollectHandler, TerminalHandler, stall detection
│   ├── context.rs           # Conversation context, message types, tool pairing, role alternation
│   ├── context_references.rs # @ reference expansion (file, folder, git, url)
│   ├── compact.rs           # Multi-layered compaction (micro, SM, LLM, partial, reactive, legacy)
│   ├── config.rs            # Configuration loading, CachedSystemPrompt, build_system_prompt
│   ├── permissions.rs       # PermissionMode (Ask/Auto/Plan), PermissionGate, denied patterns
│   ├── error_types.rs       # 15-category error taxonomy with retry strategies
│   ├── session_memory.rs    # Persistent session memory with disk persistence
│   ├── filehistory.rs       # File version history and snapshots
│   ├── prompt_caching.rs    # Anthropic prompt caching with cache control markers
│   ├── normalize.rs         # API message normalization for KV cache reuse
│   ├── rate_limit.rs        # Rate limit tracking from response headers
│   ├── retry_utils.rs       # Retry utilities with exponential backoff
│   ├── task_store.rs        # TaskStore for background bash tasks (registration, kill, eviction)
│   ├── work_task.rs         # WorkTaskStore for LLM task management (dependencies, cycle detection)
│   ├── tools/               # Built-in tool implementations
│   │   ├── coercion.rs      # Argument type coercion
│   │   ├── mod.rs           # ToolResult, ToolResultMetadata, Registry, path safety
│   │   ├── exec_tool.rs     # Shell command execution + background bash task system
│   │   ├── file_read.rs     # File reading
│   │   ├── file_write.rs    # File writing
│   │   ├── file_edit.rs     # Single-file editing
│   │   ├── multi_edit.rs    # Multi-file editing
│   │   ├── fileops.rs       # File operations (copy, move, delete, etc.)
│   │   ├── glob_tool.rs     # File pattern matching
│   │   ├── grep_tool.rs     # Text search
│   │   ├── list_dir.rs      # Directory listing
│   │   ├── git_tool.rs      # Git operations
│   │   ├── system_tool.rs   # System information
│   │   ├── process.rs       # Process management
│   │   ├── terminal_tool.rs # tmux/screen management
│   │   ├── runtime_info.rs  # Rust runtime info
│   │   ├── web_search.rs    # Web search (DuckDuckGo)
│   │   ├── web_fetch.rs     # Web page fetching
│   │   ├── exa_search.rs    # Web search (Exa)
│   │   ├── mcp_tools.rs     # MCP tool integration
│   │   ├── skill_tools.rs   # Skill tools (read, list, search)
│   │   ├── file_history_tools.rs  # 13 file history tools
│   │   ├── memory_tool.rs   # Session memory tools
│   │   ├── task_tool.rs     # Task management tools (create, list, get, update, stop)
│   │   └── agent_tool.rs    # Sub-agent spawning tool
│   ├── skills/              # Skill loading and tracking
│   │   ├── mod.rs           # Loader, SkillMeta, SkillInfo, frontmatter parsing
│   │   └── tracker.rs       # SkillTracker (shown/read/used tracking)
│   ├── mcp/                 # MCP client support
│   │   ├── mod.rs           # Manager (register, start, list servers)
│   │   └── client.rs        # stdio + HTTP+SSE transport client
│   └── transcript/          # JSONL conversation logging
│       └── mod.rs           # Transcript, Entry types, resume support
└── Cargo.toml
```

### Sub-Agent System (AgentTool)

The `agent` tool enables spawning sub-agents that run independently with their own filtered tool registries. Three built-in agent types are supported:

- **`explore`** -- Read-only agent specialized in code search. Tools denied: `write_file`, `edit_file`, `multi_edit`, `fileops`, `exec`, `terminal`, `git`.
- **`plan`** -- Read-only agent specialized in architecture planning. Same tool restrictions as `explore`.
- **`verify`** -- Adversarial verification agent. Tools denied: `write_file`, `edit_file`, `multi_edit`, `fileops`. Can run commands but cannot write files.

Sub-agents can be spawned **synchronously** (blocking, result returned inline) or **asynchronously** (non-blocking, task ID returned for later retrieval). Key implementation details:

- 4-layer tool filtering: (1) global disallowed (`agent` tool always denied to prevent recursion), (2) async-specific disallowed, (3) agent type deny list, (4) caller-specified disallowed tools. Optional `allowed_tools` whitelist with `*` wildcard support.
- Sub-agents inherit the parent's model configuration but can override with a `model` parameter. Max turns capped at 50.
- Sub-agents build their own system prompts with environment info, tool list, and agent-type-specific behavioral instructions.
- Sub-agents do not inherit session memory to avoid cross-agent interference.

### Task Management System (TaskTool + WorkTask)

Two distinct task management systems exist for different purposes:

**WorkTaskStore** (`work_task.rs`) -- LLM task tracking:
- Structured tasks with subject, description, active_form, status, owner, and arbitrary metadata.
- Bidirectional dependency tracking: `blocks`/`blocked_by` edges maintained automatically.
- Cycle detection via BFS before adding new dependency edges (prevents circular chains).
- Hash prefix normalization (`#1` -> `1`) and integer-to-string coercion for LLM-friendly dependency IDs.
- Deleted tasks automatically clean up their references from all other tasks.

**TaskStore** (`task_store.rs`) -- Background bash task lifecycle:
- Task states: `pending` -> `running` -> `completed`/`failed`/`killed`.
- OS process tracking with PID for kill support (platform-specific: `kill -9` on Unix, `taskkill /F /T` on Windows).
- Automatic eviction after 30 seconds for completed/failed/killed tasks, with output file cleanup.
- Canonical task ID registration: IDs are generated by the TaskStore at registration time, ensuring display IDs and internal IDs match (fixing the earlier ID mismatch bug).

### Background Bash Task System

Background bash commands (via `exec` tool with `run_in_background=true`) are integrated into `exec_tool.rs`:

- Commands run in dedicated background threads with child processes.
- Task IDs are registered in `TaskStore` before process spawn, with output files named after the canonical task ID.
- The `task_output` tool reads output files and supports blocking wait for completion.
- The `task_stop` tool kills the background process via OS signal and marks the task as killed.
- Task completion/failure notifications are sent via an unbounded MPSC channel for agent loop integration.

## Compatibility

Works with Anthropic API and compatible endpoints. Tested with:
- Anthropic Claude models (sonnet-4-20250514, opus-4-20250514, haiku-4-5, etc.)
- OpenAI-compatible proxies
- MiniMax models (via compatible proxy)

## License

MIT
