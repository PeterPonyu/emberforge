# Emberforge Roadmap

## Current State (2026-04-03)

### Completed
- Rust workspace: 11 crates (api, claw-cli, commands, compat-harness, lsp, plugins, runtime, server, telemetry, tools, plus workspace root)
- 330+ tests passing, zero warnings
- Ollama provider: auto-detect + 51 local models supported
- Prompt caching (FNV-1a fingerprinting, 30s completion TTL)
- Telemetry (JSONL session tracing, turn timing, analytics events)
- MCP/LSP tool wiring (MCPTool -> McpServerManager, LSPTool -> diagnostics)
- 35 slash commands, 26 tool specs
- .env loading with placeholder detection
- Auto model selection: Anthropic > xAI > Ollama fallback
- Animated dragon spinner, verbose/thinking toggle
- Session save/load/export/resume
- Plugin system with hooks (pre/post tool use)
- keep_alive=60m for Ollama model persistence

### Known Issues
- skill_loads_local_skill_prompt test depends on machine-local fixture
- Thinking models (qwen3) need /no_think suffix or native API for content
- Stream-safe markdown sometimes buffers too aggressively

---

## Phase 1: Polish & Stability (1-2 weeks)

### 1.1 Fix Thinking Model Output
- **Problem**: qwen3:8b puts all output in `<think>` tags via OpenAI-compat, leaving content empty
- **Fix**: For thinking-capable Ollama models, use native `/api/chat` with `think: true` and merge `thinking` + `content` fields
- **Scope**: `crates/api/src/providers/openai_compat.rs` — add Ollama native API path
- **Test**: Verify qwen3, deepseek-r1 produce visible content

### 1.2 Improve Streaming UX
- **Problem**: Text streams but doesn't show newlines properly in some terminals
- **Fix**: Bypass markdown renderer for plain text streaming; only render code blocks with syntect
- **Scope**: `crates/claw-cli/src/render.rs` + `main.rs` streaming loop

### 1.3 Fix Skill Path Inconsistency
- **Problem**: `/skills` searches project-local roots but `Skill` tool only searches user-global roots
- **Fix**: Unify search paths in both commands/discovery.rs and tools/implementations.rs
- **Test**: Add test with temp project-local skill

### 1.4 Clean Up Session Files
- **Problem**: `.claw/sessions/` accumulates stale session JSON files
- **Fix**: Add `/session clean` subcommand to prune sessions older than 7 days
- **Scope**: `crates/claw-cli/src/main.rs` session handler

---

## Phase 2: Rebrand to Emberforge (2-3 weeks)

### 2.1 User-Facing Rebrand (Week 1)
Rename all user-visible surfaces while keeping internal crate names:

| Old | New |
|-----|-----|
| `claw` binary | `ember` binary (keep `claw` as alias) |
| `CLAW.md` | `EMBER.md` (read both, prefer new) |
| `.claw/` | `.ember/` (support both) |
| `.claw.json` | `.ember.json` (support both) |
| `.claw-agents/` | `.ember-agents/` (support both) |
| `CLAW_*` env vars | `EMBER_*` env vars (read both) |
| `Emberforge` in banner | `Emberforge` |
| `claw-cli` crate | `ember-cli` (later) |

### 2.2 Product Vocabulary (Week 1)

| Concept | Current | Proposed |
|---------|---------|----------|
| Background agents | agents | Flights |
| Skills/capabilities | skills | Runes |
| Sessions | sessions | Roosts |
| Permission modes | read-only/workspace-write/danger | Shield/Forge/Dragon |
| Task queue | tasks | Forge Queue |
| Model routing | provider detection | Flight Path |
| Memory system | CLAW.md/memory | Atlas |

### 2.3 Compatibility Layer (Week 2)
- Config loader reads both `.claw.json` and `.ember.json`
- Instruction loader reads both `CLAW.md` and `EMBER.md`
- State directory supports both `.claw/` and `.ember/`
- `claw` binary becomes a shim that prints deprecation notice then delegates to `ember`
- ENV reader checks both `CLAW_*` and `EMBER_*` prefixes

### 2.4 Visual Identity (Week 2-3)
- Dragon logo: animated startup with fire gradient (208/214/220 ANSI palette)
- Prompt: `ember>` or fire emoji
- Color theme: warm orange/amber for Emberforge identity
- Tool display: ember-themed box drawing

---

## Phase 3: Runtime Sophistication (3-4 weeks)

### 3.1 Context Window Management
- Auto-detect model context window from Ollama API (`/api/show`)
- Track token count per turn using prompt cache stats
- Trigger automatic compaction when approaching 80% of context window
- Show token budget in `/status`
- **Reference**: parity's `compact.rs` already has `should_compact()` logic

### 3.2 Reactive Compaction
- When API returns 413/context overflow, automatically compact and retry
- Preserve recent 3 messages verbatim, summarize older
- Show compaction event to user: "context compacted: 8K -> 2K tokens"
- **Scope**: `crates/runtime/src/conversation.rs` — add retry loop

### 3.3 Token Budget Continuation
- When model hits max_tokens mid-response, automatically continue
- Detect `stop_reason: "max_tokens"` and send continuation prompt
- Cap at 3 continuations to prevent infinite loops
- **Scope**: `crates/claw-cli/src/main.rs` streaming loop

### 3.4 Model Profile System
- Per-family configs: context window, tool support, thinking mode, max_tokens
- Auto-configure based on Ollama model metadata
- Disable tools for models that don't support them (already done via retry)
- Set appropriate max_tokens per model family
- **Scope**: New `crates/runtime/src/model_profiles.rs`

---

## Phase 4: MCP & LSP Full Integration (2-3 weeks)

### 4.1 MCP Tool Execution
- Wire `MCPTool` through to `McpServerManager::call_tool()` (done)
- Add MCP server lifecycle management (connect/disconnect/reconnect)
- Implement `ListMcpResources` → MCP resource listing API
- Implement `ReadMcpResource` → MCP resource content retrieval
- Test with a real MCP server (e.g., filesystem MCP)

### 4.2 LSP Tool Execution
- Wire `LSPTool` through to `LspManager` methods
- Actions: diagnostics, go-to-definition, find-references
- Auto-start LSP server when workspace has matching files
- Show LSP diagnostics in `/status`

### 4.3 MCP Server Management
- `/mcp connect <server>` — start and initialize server
- `/mcp disconnect <server>` — gracefully shutdown
- `/mcp list` — show connected servers with tool counts
- Persist MCP server configs across sessions

---

## Phase 5: Agent & Skill Ecosystem (3-4 weeks)

### 5.1 Agent Improvements
- Background agent with progress reporting
- Agent result streaming to parent session
- Agent isolation via git worktrees
- `/agents status` — show running agents
- Agent timeout and cancellation

### 5.2 Skill System
- Project-local skills (`.ember/skills/`)
- User-global skills (`~/.ember/skills/`)
- Skill marketplace (plugin registry with skill discovery)
- Skill invocation with `@skill-name` syntax
- Built-in skills: `/commit`, `/review`, `/test`, `/deploy`

### 5.3 Task Management
- Background task queue with priority
- `/tasks list` — show queued and running tasks
- `/tasks stop <id>` — cancel a running task
- Task result persistence and retrieval
- Task dependencies (task B waits for task A)

---

## Phase 6: Multi-Provider Strategy (2-3 weeks)

### 6.1 Provider Router
- Smart model selection based on task complexity
- Route simple queries to small/fast models (qwen2.5:0.5b)
- Route complex coding to large models (qwen3:32b, claude-opus)
- Cost-aware routing for cloud providers
- `/model auto` — enable automatic routing

### 6.2 Provider-Specific Optimizations
- Ollama: native API for thinking models, batch inference, GPU scheduling
- Anthropic: prompt caching, extended thinking, computer use
- OpenAI: structured outputs, function calling
- xAI: Grok-specific features

### 6.3 Hybrid Local+Cloud
- Use local model for tool execution (fast, free)
- Use cloud model for complex reasoning (accurate, costly)
- Automatic fallback: local -> cloud when local fails
- Token budget: spend local tokens freely, budget cloud tokens

---

## Phase 7: Distribution & Packaging (1-2 weeks)

### 7.1 Binary Distribution
- GitHub Releases with pre-built binaries (Linux x86_64, macOS arm64)
- Cargo install: `cargo install emberforge`
- Homebrew tap: `brew install emberforge/tap/ember`
- AUR package for Arch Linux

### 7.2 Configuration
- `ember init` — scaffold project config, detect stack, set defaults
- `ember login` — OAuth flow for Anthropic/xAI
- `ember config` — interactive configuration wizard
- Default configs for common stacks (Rust, Python, Node, Go)

### 7.3 Documentation
- README with quick start
- docs/ with architecture guide
- Man page generation from CLI help
- Example configs and workflows

---

## Milestone Summary

| Milestone | Duration | Key Deliverable |
|-----------|----------|----------------|
| Phase 1: Polish | 1-2 weeks | Thinking models work, streaming smooth |
| Phase 2: Rebrand | 2-3 weeks | Emberforge identity, compatibility aliases |
| Phase 3: Runtime | 3-4 weeks | Auto-compaction, token budget, model profiles |
| Phase 4: MCP/LSP | 2-3 weeks | Full MCP tool execution, LSP integration |
| Phase 5: Agents | 3-4 weeks | Background agents, skills, task queue |
| Phase 6: Providers | 2-3 weeks | Smart routing, hybrid local+cloud |
| Phase 7: Distribution | 1-2 weeks | Binary releases, packaging, docs |

**Total: ~16-22 weeks for full product**
