# miniClaudeCode-rust

A lightweight, distilled implementation of Claude Code's agent loop framework written in Rust.

## Overview

miniClaudeCode-rust is a minimal AI agent framework that implements the core agentic loop pattern similar to Claude Code. It provides a tool-use paradigm where an LLM can execute various tools to accomplish complex tasks.

## Features

- **Agent Loop**: Implements the core agentic loop with turn-based conversation, tool execution, and context management
- **Streaming Support**: Real-time streaming output with thinking block handling for various LLM providers
- **Intelligent Context Compaction**: 4-phase automatic context degradation keeps conversations productive in limited context windows
- **Tool System**: 17 built-in tools including exec, file operations, git, web search, system info, and more
- **Permission Modes**: Three permission modes for different use cases (auto, ask, plan)
- **MCP Support**: Model Context Protocol client for external tool integration
- **Skills System**: Extensible skill loader for custom agent behaviors

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
./target/release/miniclaudecode-rust --model claude-sonnet-4-20250514

# Specify project directory
./target/release/miniclaudecode-rust --dir /path/to/project
```

## Configuration

Configuration is stored in `.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "your-api-key",
    "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
    "ANTHROPIC_MODEL": "claude-sonnet-4-20250514"
  }
}
```

Or use environment variables:

```bash
export ANTHROPIC_API_KEY="your-api-key"
export ANTHROPIC_BASE_URL="https://api.anthropic.com"
export ANTHROPIC_MODEL="claude-sonnet-4-20250514"
```

## Architecture

```
miniClaudeCode-rust/
├── src/
│   ├── main.rs          # Entry point and REPL
│   ├── agent_loop.rs    # Core agent loop implementation
│   ├── streaming.rs     # Streaming event handling
│   ├── context.rs       # Conversation context management
│   ├── compact.rs        # 4-phase intelligent context compaction
│   ├── permissions.rs    # Permission gate implementation
│   ├── config.rs        # Configuration loading
│   ├── tools/           # Built-in tool implementations
│   ├── mcp/             # MCP client support
│   ├── skills/          # Skill loading system
│   └── transcript/      # Conversation logging
└── Cargo.toml
```

## License

MIT
