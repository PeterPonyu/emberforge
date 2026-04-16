# Claude Code Coverage Audit

Last updated: 2026-04-04

Scope: compare the current Rust `emberforge` branch with the attached TypeScript
`claude-code-src` tree.

Method:

- inspected concrete entrypoints, crates, commands, tools, runtime plumbing, UI
  surfaces, and support services
- weighted real implementation files more heavily than auto-generated TS stubs
- did **not** copy upstream code; this is an architectural audit only

See also: [`CROSS_PORT_AUDIT.md`](CROSS_PORT_AUDIT.md) — hook / lifecycle /
plugin-manifest gap matrix across Rust, TS, Go, and C++ ports (2026-04-16).

## Bottom line

Emberforge is **not** based on the full Claude Code technique stack.

It is now a **substantial Rust reimplementation of the Claude-style core**:

- local terminal REPL + prompt mode
- provider routing and streaming conversation loop
- slash commands, sessions, export/resume, diagnostics
- tool registry/execution, MCP, LSP, hooks, plugins, telemetry
- terminal rendering, startup UI, HUD, markdown/code/thinking presentation

But it still does **not** cover the full outer stack that exists in Claude Code:

- Ink/React application shell
- bridge / remote-control runtime and hybrid transports
- daemon worker model and rich background session lifecycle
- assistant/Kairos/coordinator/swarm/team subsystems
- buddy / voice / notifier / managed-settings / GrowthBook-style service layers
- output-style registry and richer transcript/message surface system

So the accurate description is:

> Emberforge covers much of the Claude Code **core CLI/runtime/tooling stack**, but
> not the full Claude Code **platform stack**.

## Coverage matrix

| Subsystem | Coverage | Notes |
| --- | --- | --- |
| CLI / REPL / prompt mode | Mostly covered | Strong local CLI with slash commands, prompt mode, doctor, sessions, export, review, tasks surface |
| Runtime conversation loop | Mostly covered | Streaming assistant/tool loop, compaction, usage, permissions, hook integration |
| Providers / auth / OAuth | Mostly covered | Ollama, Anthropic, xAI-compatible support and OAuth foundations |
| Tool registry / execution | Mostly covered | Large built-in surface, including MCP/LSP/resource tools |
| Sessions / persistence | Mostly covered | Local JSON sessions, managed session commands, telemetry traces |
| Hooks | Mostly covered | Config + execution pipeline exist in Rust now |
| Plugins | Mostly covered | Real plugin manager exists, with install/enable/disable/update + bundled sync |
| MCP | Mostly covered | Config, stdio bootstrap, discovery, tool/resource plumbing present |
| LSP | Partial | Practical LSP manager/tooling exists, but UX breadth is thinner than Claude Code |
| Renderer / terminal UI | Partial | Native Rust renderer is strong, but not the same layered message/UI system as Ink |
| Skills / agents | Partial | Local discovery/listing exists, but no Claude-scale bundled workflow ecosystem |
| Background tasks | Partial | `.ember-agents` manifests and task listing exist, but not a daemon-grade runtime |
| Remote / bridge / transport | Partial to missing | Upstream proxy bootstrap exists, but not Claude Code bridge/remote-control parity |
| Server / attach / remote session APIs | Partial | Simple HTTP/SSE server exists, but not full supervisor/transport stack |
| Analytics / policy / settings sync | Missing | Claude Code has many service layers here; Emberforge does not |
| Buddy / voice / assistant mode / coordinator / team | Missing | No parity yet |

## What Emberforge already covers well

### 1. Terminal-first CLI core

Evidence in Emberforge:

- `crates/ember-cli/src/main.rs`
- `crates/commands/src/parse.rs`
- `crates/commands/src/spec.rs`

Evidence in Claude Code:

- `/home/zeyufu/Desktop/claude-code-src/main.tsx`
- `/home/zeyufu/Desktop/claude-code-src/replLauncher.tsx`

Current status:

- Emberforge has a real interactive REPL and one-shot prompt mode.
- Slash command breadth is no longer “minimal only”; it now includes
  `agents`, `skills`, `hooks`, `mcp`, `plan`, `tasks`, `review`, `plugins`,
  session controls, git helpers, doctor, model switching, and export.
- This means the old claim that these command families were broadly absent is
  stale on this branch.

### 2. Conversation runtime and tool loop

Evidence in Emberforge:

- `crates/runtime/src/conversation.rs`
- `crates/runtime/src/session.rs`
- `crates/runtime/src/usage.rs`

Evidence in Claude Code:

- `/home/zeyufu/Desktop/claude-code-src/QueryEngine.ts`
- `/home/zeyufu/Desktop/claude-code-src/Tool.ts`

Current status:

- Emberforge has a credible Claude-style core loop: assistant stream → tool uses
  → tool results → continued assistant response.
- Permission checks, usage tracking, and compaction are in place.
- This is one of the strongest covered parts of the stack.

### 3. Hooks now exist at runtime

Evidence in Emberforge:

- `crates/runtime/src/hooks.rs`
- `crates/runtime/src/conversation.rs`

Current status:

- The older parity note saying hooks were only parsed is no longer correct.
- `PreToolUse` and `PostToolUse` are now executed in the Rust runtime.
- Hook feedback can modify the effective tool-result text path, and denial is
  enforced in the turn loop.

### 4. Plugins now exist as a real subsystem

Evidence in Emberforge:

- `crates/plugins/src/manager.rs`
- `crates/plugins/src/hooks.rs`
- `crates/commands/src/handlers.rs`

Evidence in Claude Code:

- `/home/zeyufu/Desktop/claude-code-src/plugins/builtinPlugins.ts`

Current status:

- The older parity note saying plugins were absent is now stale.
- Emberforge has a plugin manager, registry, bundled sync, external plugin
  discovery, install/update/enable/disable/uninstall flow, and command surface.
- What is still missing is **ecosystem depth**, not total absence.

### 5. MCP and LSP core support

Evidence in Emberforge:

- `crates/runtime/src/mcp.rs`
- `crates/runtime/src/mcp_client.rs`
- `crates/runtime/src/mcp_stdio.rs`
- `crates/lsp/src/manager.rs`
- `crates/tools/src/specs.rs`
- `crates/tools/src/executor.rs`

Current status:

- MCP server config, stdio bootstrapping, tool discovery, and resource access are present.
- LSP has a real manager and CLI-facing tool support.
- Claude Code still has a larger surrounding service/UI layer, but Emberforge
  clearly covers the core protocol side.

### 6. Terminal presentation stack

Evidence in Emberforge:

- `crates/ember-cli/src/render.rs`
- `crates/ember-cli/src/ui/banner.rs`
- `crates/ember-cli/src/ui/animation.rs`
- `crates/ember-cli/src/ui/hud.rs`

Evidence in Claude Code:

- `/home/zeyufu/Desktop/claude-code-src/components/Markdown.tsx`
- `/home/zeyufu/Desktop/claude-code-src/components/MarkdownTable.tsx`
- `/home/zeyufu/Desktop/claude-code-src/components/messages/AssistantThinkingMessage.tsx`

Current status:

- Emberforge does not use Ink/React, but it now has a meaningful terminal UI stack
  of its own: startup banner, intro animation, HUD, markdown renderer, code block
  framing, table framing, and thinking-preview handling.
- This is coverage by **different implementation strategy**, not parity by framework.

## What is only partially covered

### Skills and agents

Evidence in Emberforge:

- `crates/commands/src/handlers.rs`
- `crates/tools/src/specs.rs`

Current status:

- Emberforge can list skills, load local skills, and launch agents.
- What it lacks is Claude Code’s richer bundled skill ecosystem, hot-reload style
  discovery, and broader workflow/task ecosystem around those abstractions.

### Background tasks and long-running orchestration

Evidence in Emberforge:

- `crates/ember-cli/src/main.rs` (`.ember-agents` task listing and stop flow)
- `crates/server/src/lib.rs`

Evidence in Claude Code:

- `/home/zeyufu/Desktop/claude-code-src/tasks.ts`
- `/home/zeyufu/Desktop/claude-code-src/bridge/bridgeMain.ts`
- daemon and bg surfaces referenced from `main.tsx`

Current status:

- Emberforge has the beginnings of background-task management.
- It does **not** yet have Claude Code’s broader worker/attach/logs/kill/remote-run
  style orchestration stack.

### Remote support

Evidence in Emberforge:

- `crates/runtime/src/remote.rs`
- `crates/server/src/lib.rs`

Evidence in Claude Code:

- `/home/zeyufu/Desktop/claude-code-src/bridge/bridgeMain.ts`
- `/home/zeyufu/Desktop/claude-code-src/cli/transports/HybridTransport.ts`

Current status:

- Emberforge has remote bootstrap/proxy concepts and a simple server surface.
- It does not yet have parity with Claude Code’s bridge runtime, hybrid transport,
  remote-control flows, or session ingress lifecycle.

## What is materially missing

### Ink / React application shell

Claude Code is a much larger interactive application with:

- React component trees
- virtualized message surfaces
- richer per-message UI components
- settings dialogs and flow-specific screens

Emberforge deliberately uses a simpler Rust terminal renderer instead.

### Daemon / supervisor / remote-control platform

Claude Code’s `main.tsx` references fast-paths for:

- daemon workers
- remote-control / bridge mode
- attach/logs/kill / background session surfaces
- multiple service-layer bootstraps and analytics/policy gates

Emberforge does not currently implement an equivalent platform runtime.

### Buddy / voice / assistant mode / coordinator / swarm / team

Claude Code has source-tree areas for:

- `buddy/`
- `voice/`
- `assistant/`
- `coordinator/`
- richer task families and remote agents

Emberforge does not currently cover those higher-order subsystems.

### Managed settings / analytics / policy stack

Claude Code’s `main.tsx` loads a large platform layer around:

- GrowthBook / experimentation
- managed settings
- remote settings sync
- policy limits
- analytics/event sinks
- richer plugin bootstrap and install flows

Emberforge has telemetry, but not that full service platform.

## Important doc correction

The previous `docs/PARITY.md` in this repository had become stale in several important ways.

These older statements were no longer accurate on the current branch:

- plugins are absent
- hooks are config-only
- `/agents`, `/hooks`, `/mcp`, `/plugin`, `/skills`, `/plan`, `/review`, `/tasks` are missing
- MCP/LSP parity tools are missing outright

Those claims matched an earlier port snapshot, not the current tree.

## Final assessment

If the question is:

> “Is Emberforge already based on the full Claude Code technique stack?”

The answer is:

**No.**

If the question is instead:

> “Does Emberforge already cover a serious amount of the Claude Code core architecture?”

The answer is:

**Yes.**

Today Emberforge covers a meaningful Claude-style core:

- terminal CLI/repl
- provider/runtime/tool loop
- sessions, permissions, hooks, plugins
- MCP/LSP/tooling
- rendering/UI foundations

The main remaining gap is not the core agent loop anymore; it is the broader
surrounding platform: remote bridge/daemon/task orchestration, advanced message
UI surfaces, managed service layers, and optional higher-order systems like
voice/buddy/assistant/team mode.
