# Emberforge

![Emberforge](assets/badge.svg)

**A local-first terminal coding tool for language-model workflows.**

Emberforge is a terminal coding tool for local models through Ollama. When you need hosted models, it can use those too. The project includes an interactive REPL, one-shot prompt mode, built-in tools, session management, plugins, and support for multiple model backends.

## Quick Start

```bash
# Build from source
cargo build --release

# Start the REPL
./target/release/ember

# Or with a specific model
./target/release/ember --model qwen3:8b

# Run a cached health check against your local setup
./target/release/ember doctor

# One-shot prompt
./target/release/ember prompt "explain this codebase"
```

## What You Get

- Runs against local Ollama models, so you can work without API keys for local-only setups
- Supports a wide range of Ollama model families, including qwen, llama, gemma, mistral, deepseek, and phi
- Connects to Anthropic and xAI when credentials are configured
- Includes `/model auto` for task-based model selection
- Ships with `ember doctor` for setup checks, including cached family audits for slower scans
- Supports slash commands such as `/help`, `/status`, `/doctor`, `/model`, `/compact`, `/review`, `/commit`, and `/pr`
- Includes built-in tools for shell work, file operations, search, web access, notebooks, agents, and skills
- Lets you save, resume, and export sessions
- Supports plugins, MCP servers, telemetry, and prompt caching

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

Emberforge reads configuration in this order:

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

If you want project-specific instructions to persist across sessions, add an `EMBER.md` file at the project root. From inside the REPL, run:

```bash
/init    # Scaffolds EMBER.md, .ember.json, and .gitignore entries
```

## Diagnostics

Use the built-in diagnostics command to check your setup:

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

For terminal rendering regressions, there is also an optional live smoke test. It
uses small local Ollama models to exercise tool output, code blocks, and
thinking-preview behavior, then caches the result after the first successful run:

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
