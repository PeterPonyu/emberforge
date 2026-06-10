# AGENTS.md — Emberforge operating contract

This file is the onboarding contract for AI agents (and humans driving them) working in
the Emberforge repository. It is factual to **this** repo. Emberforge is a local-first
terminal coding tool written in Rust; the shipped binary is named `ember`.

If you only need one thing: build with `cargo build --release`, then run a single
non-interactive agent turn with `./target/release/ember prompt "<your task>"`.

## Build & install the `ember` binary

```bash
# Build the optimized binary (output: ./target/release/ember)
cargo build --release

# Optional: install onto your PATH from the workspace
cargo install --path crates/ember-cli   # installs the `ember` binary

# Verify
./target/release/ember --version
./target/release/ember --help
```

Requirements: a stable Rust toolchain (`rustup` recommended). The CI toolchain is
`dtolnay/rust-toolchain@stable` with `rustfmt`; `clippy` is also run.

## The direct loop (one-shot, non-interactive)

The "direct loop" runs exactly **one** agent turn through the conversation runtime
(model + tool dispatch), prints the result, and exits. This is the path agents should
use for scripted, non-interactive work.

```bash
# Explicit prompt subcommand
./target/release/ember prompt "explain crates/runtime/src/prompt.rs"

# Bare prompt (any non-subcommand argument is treated as a prompt)
./target/release/ember "summarize this repo"

# Compatibility flag form
./target/release/ember -p "list the crates and what each does"
```

Implementation: `crates/ember-cli/src/main.rs` parses `CliAction::Prompt` and calls
`LiveCli::run_turn_with_output(&prompt, output_format)`, which dispatches to the shared
runtime turn (`runtime::run_turn`). Do not build a separate agent engine — reuse this path.

### Output formats (prompt mode only)

| Format | Flag | Shape |
| --- | --- | --- |
| `text` (default) | _(none)_ | Human-readable streamed text |
| `json` | `--output-format json` | One JSON object: `{message, model, tool_uses, tool_results, usage, iterations}` |
| `ndjson` | `--output-format ndjson` | Newline-delimited events: `turn_started`, `assistant_text`, `usage`, `turn_completed` |

Structured formats are **only** valid in prompt mode. Using them with the REPL is rejected
with a clear error.

```bash
./target/release/ember prompt "status" --output-format json
./target/release/ember -p "status" --output-format ndjson
```

### Other prompt-mode flags

| Flag | Effect |
| --- | --- |
| `--model <name>` | Pick a model / alias (e.g. `qwen3:8b`, `opus`, `grok-3`) |
| `--permission-mode <mode>` | `read-only`, `workspace-write`, or `danger-full-access` |
| `--dangerously-skip-permissions` | Shortcut for `danger-full-access` |
| `--allowed-tools <list>` (alias `--allowedTools`) | Restrict the tool set for the turn |

Note: the default permission mode is `danger-full-access` unless overridden by
`EMBER_PERMISSION_MODE` or a flag. Set `--permission-mode read-only` for safe inspection.

## The interactive REPL

```bash
./target/release/ember                 # start the REPL with the default model
./target/release/ember --model qwen3:8b
```

Useful slash commands inside the REPL: `/help`, `/status`, `/doctor`, `/model`,
`/permissions`, `/compact`, `/commit`, `/pr`, `/init`. Press `Tab` to complete commands.

`/model` accepts `auto` (task-based routing), `hybrid` (split fast/slow tasks
across local and hosted models), a specific model name or alias
(e.g. `qwen3:8b`, `opus`, `grok-3`), or `list` to show all available models.

## Providers & required env vars

Emberforge is local-first (Ollama) and can also use hosted providers when credentials are
present. The default model is the Ollama default unless Anthropic or xAI auth is detected,
or `EMBER_MODEL` is set.

| Provider | Models | Required env |
| --- | --- | --- |
| **Ollama** (local) | qwen3, llama3, gemma3, mistral, deepseek-r1, phi4, and other local families | none (needs a running Ollama daemon) |
| **Anthropic** | Claude Opus / Sonnet / Haiku | `ANTHROPIC_API_KEY` (optional `ANTHROPIC_BASE_URL`) |
| **xAI** | Grok 3 / Grok 3 Mini | `XAI_API_KEY` (optional `XAI_BASE_URL`) |

Other environment variables:

- `OLLAMA_BASE_URL` — Ollama endpoint (default `http://localhost:11434/v1`)
- `EMBER_MODEL` — override the default model
- `EMBER_PERMISSION_MODE` — default permission mode
- `EMBER_CONFIG_HOME` — override the config directory
- `EMBER_TELEMETRY=off` (or `0`) — disable the local JSONL telemetry sink

Diagnostics: `./target/release/ember doctor` (add `full`, `status`, or `reset`) runs cached
setup/connectivity checks against the configured providers.

## Running tests (full CI suite)

CI (`.github/workflows/ci.yml`) runs, in order:

```bash
cargo fmt --all -- --check          # formatting
cargo check --workspace             # type/borrow check
cargo clippy --workspace --all-targets
cargo test --workspace              # unit + integration tests
cargo build --release               # release build
```

Optional real-PTY terminal smoke test (Python):

```bash
python3 tests/test_terminal_startup.py
python3 tests/test_terminal_startup.py --live-render   # uses small local Ollama models
```

## Repository layout

```text
crates/
├── api/                API client — Anthropic, OpenAI-compat, Ollama provider routing
├── ember-cli/          The `ember` binary: REPL, arg parsing, prompt mode, renderer
├── commands/           Shared slash-command definitions and help text
├── compat-harness/     Compatibility layer
├── integration-tests/  Cross-crate runtime / tool-pipeline integration tests
├── lsp/                Language Server Protocol integration
├── plugins/            Plugin system with pre/post tool hooks
├── runtime/            Conversation runtime, session/config, prompt builder, MCP, compaction
├── server/             HTTP/SSE server infrastructure
├── telemetry/          Session tracing, analytics events, JSONL sink
└── tools/              Built-in tool specs with execution dispatch
docs/                   Architecture, parity, and release notes
tests/                  Python PTY startup smoke test
```

Key direct-loop files: `crates/ember-cli/src/main.rs` (`CliAction::Prompt`,
`run_turn_with_output`), `crates/runtime/src/conversation.rs` (`run_turn`),
`crates/runtime/src/prompt.rs` (system prompt builder), `crates/runtime/src/bridge.rs`.

## Agent etiquette in this repo

- Prefer the smallest viable change; do not refactor adjacent code unprompted.
- Run `cargo fmt`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace`
  before claiming work is complete — show real output, not assumptions.
- Do not commit build artifacts (`/target`), local state (`.ember/`, `.env`), or the
  prebuilt `ember` binary. They are already covered by `.gitignore`.
- Project-specific persistent instructions live in `EMBER.md` (scaffold via `/init`).
