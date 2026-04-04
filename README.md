# Emberforge

![Emberforge pixel logo](assets/emberforge-pixel-logo.svg)

**A local-first coding forge for serious developers.**

Emberforge is an interactive coding assistant that runs in your terminal, powered by local LLMs via Ollama. It provides a rich REPL with tool execution, session management, plugins, and multi-provider support.

## Quick Start

```bash
# Build from source
cargo build --release

# Start the REPL (auto-detects Ollama)
./target/release/ember

# Or with a specific model
./target/release/ember --model qwen3:8b

# Run a cached health check against your local setup
./target/release/ember doctor

# One-shot prompt
./target/release/ember prompt "explain this codebase"
```

## Features

- **Local-first**: Runs with Ollama — no API keys needed for local models
- **51 local models**: Supports all Ollama model families (qwen, llama, gemma, mistral, deepseek, phi, and more)
- **Cloud fallback**: Anthropic Claude, xAI Grok when API keys are configured
- **Smart routing**: `/model auto` selects models by task complexity
- **Built-in diagnostics**: `ember doctor` runs real setup checks and caches slower family audits
- **Rich slash commands**: `/help`, `/status`, `/doctor`, `/model`, `/compact`, `/review`, `/commit`, `/pr`, and more
- **Built-in tools**: bash, file ops, search, web, notebooks, agents, skills, and more
- **Session persistence**: Save, resume, export conversations
- **Plugin system**: Extend with custom tools and hooks
- **MCP integration**: Connect to Model Context Protocol servers
- **Telemetry**: Session tracing and usage analytics
- **Prompt caching**: FNV-1a request fingerprinting with TTL

## Architecture

```text
crates/
├── api/            API client — Anthropic, OpenAI-compat, Ollama provider routing
├── ember-cli/      Interactive REPL, streaming renderer, slash commands
├── commands/       Shared slash command definitions and help text
├── compat-harness/ Compatibility layer
├── lsp/            Language Server Protocol integration
├── plugins/        Plugin system with pre/post tool hooks
├── runtime/        Session state, config, MCP, compaction, model profiles
├── server/         HTTP/SSE server infrastructure
├── telemetry/      Session tracing, analytics events, JSONL sink
└── tools/          Built-in tool specs with execution dispatch
```

## Model Support

| Provider | Models | Auth |
| --- | --- | --- |
| **Ollama** (local) | qwen3, llama3, gemma3, mistral, deepseek-r1, phi4, plus many more local families | None needed |
| **Anthropic** | Claude Opus 4.6, Sonnet 4.6, Haiku 4.5 | `ANTHROPIC_API_KEY` |
| **xAI** | Grok 3, Grok 3 Mini | `XAI_API_KEY` |

## Configuration

Emberforge reads configuration from (in order of priority):

1. `.ember.json` (project config)
2. `.ember/settings.json` (project settings)
3. `~/.ember/settings.json` (user settings)
4. Legacy `.claw.json` / `.claw/` paths (backward compatible)

Environment variables:

- `EMBER_CONFIG_HOME` — override config directory
- `OLLAMA_BASE_URL` — custom Ollama endpoint (default: `http://localhost:11434/v1`)
- `ANTHROPIC_API_KEY` — Anthropic API credentials
- `XAI_API_KEY` — xAI API credentials

## Project Instructions

Create an `EMBER.md` file in your project root to provide persistent guidance:

```bash
ember /init    # Scaffolds EMBER.md, .ember.json, and .gitignore entries
```

## Diagnostics

Use the built-in diagnostics command for real, user-selectable health checks:

```bash
# Quick check for the current model and local Ollama setup
./target/release/ember doctor

# Slower one-per-family audit, cached after the first run
./target/release/ember doctor full

# Show or reset cached diagnostic state
./target/release/ember doctor status
./target/release/ember doctor reset
```

Inside the REPL, the same flow is available through `/doctor`.

For terminal rendering regressions, there is also an optional live smoke pass that
uses small local Ollama models for tool output, code blocks, and thinking-preview
behavior, then caches the result after the first successful run:

```bash
# Verify tool output, code blocks, and thinking preview behavior
python3 tests/test_terminal_startup.py --live-render

# Rerun the live-model pass and ignore the cached result
python3 tests/test_terminal_startup.py --live-render --refresh-live-render
```

## Development

```bash
# Build
cargo build --release

# Run Rust tests
cargo test --workspace

# Run the real PTY startup smoke test
python3 tests/test_terminal_startup.py

# Optional: run the cached live-model terminal rendering smoke test
python3 tests/test_terminal_startup.py --live-render

# Run with Ollama
./target/release/ember

```

## License

MIT
