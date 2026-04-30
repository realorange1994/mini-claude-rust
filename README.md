# miniClaudeCode-rust

A lightweight implementation of Claude Code's agent loop framework written in Rust.

## Overview

miniClaudeCode-rust is a minimal AI agent framework that implements the core agentic loop pattern similar to Claude Code. It provides a tool-use paradigm where an LLM can execute various tools to accomplish complex tasks, with error handling, context management, and crash recovery.

## Features

- **Agent Loop**: Turn-based conversation with tool execution and max turn limits
- **Streaming Support**: Real-time streaming output with ThinkFilter state machine (filters `<thinking>` and Anthropic extended thinking blocks), StreamProgress tracking (TTFB, throughput), and retry strategies
- **Context Compaction**: LLM-driven compaction with 4-phase fallback degradation (RoundBased → TurnBased → SelectiveClear → Aggressive) keeps conversations productive in limited context windows
- **@ Context References**: Inject file content, folder listings, git diffs, and URLs into prompts with `@file:path`, `@folder:path`, `@staged`, `@diff`, `@git:N`, `@url:URL`
- **Tool System**: 17+ built-in tools with argument type coercion and required parameter validation:
  - `exec` -- Shell command execution with safety patterns
  - `read_file` / `write_file` / `edit_file` / `multi_edit` -- File operations
  - `glob` / `grep` / `list_dir` -- File system search and navigation
  - `web_search` / `web_fetch` -- Web search and content fetching (built-in scraper + Exa)
  - `fileops` -- File operations (copy, move, delete, chmod, symlink)
  - `process` -- Process management (list, kill, pgrep, top, pstree)
  - `git` -- Full git operations (clone, commit, push, pull, branch, merge, rebase, stash, worktree, and more)
  - `system` -- System info (uname, df, free, uptime, hostname, arch)
  - `terminal` -- tmux/screen session management
  - `runtime_info` -- Rust runtime and system information
- **File History**: Snapshot, diff, rewind, restore, checkout, tag, annotate, search, timeline, and batch operations -- 13 dedicated file history tools
- **Permission Modes**: Three permission modes for different use cases (auto, ask, plan)
- **MCP Support**: Model Context Protocol client for external tool integration (stdio transport)
- **Skills System**: Extensible skill loader with read_skill, list_skills, and search_skills, plus a SkillTracker for progressive disclosure across turns
- **Error Classification**: Structured error taxonomy (transient, context overflow, tool pairing, max output tokens, model confusion, auth, rate limit, fatal) with retry logic and recovery strategies
- **Crash Recovery**: Per-call transcript flush, truncated line handling, tool pairing validation, and role alternation repair on resume
- **API Message Normalization**: JSON key sorting and whitespace normalization for KV cache reuse (prefix caching)
- **Prompt Caching**: Anthropic-style prompt caching with cache control markers (system + 3 breakpoints)
- **Rate Limiting**: Response-header-based rate limit tracking with retry delay estimation
- **System Prompt Caching**: CachedSystemPrompt with dirty flag, avoids rebuilding on every API call

## Installation

```bash
cargo build --release
```

## Usage

```bash
# Interactive mode
./target/release/miniclaudecode-rust

# With streaming
./target/release/miniclaudecode-rust --stream

# Specify permission mode
./target/release/miniclaudecode-rust --mode ask

# Specify model
./target/release/miniclaudecode-rust --model claude-sonnet-4-6

# Specify project directory
./target/release/miniclaudecode-rust --dir /path/to/project

# Resume a previous session
./target/release/miniclaudecode-rust --resume last
```

### Slash Commands (in interactive mode)

- `/help` -- Show available commands
- `/resume [session]` -- Resume a previous conversation session
- `/compact` -- Force context compaction
- `/clear` -- Clear conversation history
- `/mode [auto|ask|plan]` -- Switch permission mode
- `/quit` -- Exit

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
- `@git:N` -- Git commit diff (N = commit count or hash)
- `@url:URL` -- Web page content

## Configuration

Configuration is stored in `.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_API_KEY": "your-api-key",
    "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
    "ANTHROPIC_MODEL": "claude-sonnet-4-6"
  }
}
```

Or use environment variables:

```bash
export ANTHROPIC_API_KEY="your-api-key"
export ANTHROPIC_BASE_URL="https://api.anthropic.com"
export ANTHROPIC_MODEL="claude-sonnet-4-6"
```

## Architecture

```
miniClaudeCode-rust/
├── src/
│   ├── main.rs              # Entry point and REPL
│   ├── agent_loop.rs        # Core agent loop with turn limits
│   ├── streaming.rs         # Streaming with ThinkFilter state machine and StreamProgress
│   ├── context.rs           # Conversation context with tool pairing and role alternation
│   ├── context_references.rs # @ reference expansion (file, folder, git, url)
│   ├── compact.rs           # LLM-driven compaction with 4-phase fallback
│   ├── error_types.rs       # Structured error classification
│   ├── normalize.rs         # API message normalization for KV cache reuse
│   ├── permissions.rs       # Permission gate implementation
│   ├── config.rs            # Configuration loading and CachedSystemPrompt
│   ├── prompt_caching.rs    # Anthropic prompt caching support
│   ├── rate_limit.rs        # Rate limit tracking with retry delay estimation
│   ├── retry_utils.rs       # Retry utilities with exponential backoff
│   ├── filehistory.rs       # File version history and snapshots
│   ├── skills/              # Skill loading and tracking system
│   ├── tools/               # Built-in tool implementations
│   │   ├── coercion.rs      # Argument type coercion
│   │   ├── mod.rs           # ToolResultMetadata and parameter validation
│   │   └── ...              # 17+ tool implementations
│   ├── mcp/                 # MCP client support
│   └── transcript/          # Crash-safe JSONL conversation logging
└── Cargo.toml
```

## Compatibility

Works with Anthropic API and compatible endpoints. Tested with:
- Anthropic Claude models (sonnet-4-6, opus-4-6, haiku-4-5)
- OpenAI-compatible proxies
- MiniMax models (via compatible proxy)

## License

MIT
