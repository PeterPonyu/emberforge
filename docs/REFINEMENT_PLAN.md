# Emberforge Refinement Plan

Last updated: 2026-04-04

This roadmap replaces the older OMX-oriented plan with a plan grounded in the
current `emberforge` branch and the attached `claude-code-src` audit.

## Current branch position

Emberforge already has a strong Claude-style core in Rust:

- multi-provider CLI and REPL
- streaming conversation runtime with tool execution
- managed local sessions, export, resume, compaction, doctor flow
- hooks, plugins, MCP, LSP, telemetry, server foundations
- terminal UI work completed recently:
  - capability-aware startup banner
  - intro animation
  - turn HUD
  - improved markdown / code / table rendering
  - cleaner thinking preview presentation

What it still lacks is the broader Claude Code **platform envelope**:

- richer structured / remote transport layers
- daemon / attach / logs / background-worker lifecycle
- broader task orchestration
- deeper skills / agents / plugin ecosystem
- advanced transcript/message UI surfaces
- assistant / buddy / voice / coordinator / team systems
- policy / managed-settings / analytics platform layers

## Recently completed UI work

Done on this branch:

1. startup UI moved from classic-only default to capability-aware auto/pixel mode
2. intro animation added for capable terminals
3. interactive turn HUD added with typed config presets
4. markdown renderer improved for code fences, tables, blockquotes, and thinking previews
5. prompt-mode output cleaned up so REPL-only HUD chrome does not leak into `-p` output

The next plan should therefore build **after** the terminal UI pass, not pretend
that HUD/statusline is still a future phase.

## Priority roadmap

## Phase 1 — Output and message-surface polish (1-2 weeks)

Highest-value near-term work after the UI update.

### Phase 1 goals

- improve tool call/result rendering so long operations are easier to scan
- stop human-readable tool chatter from polluting structured output modes
- make thinking presentation feel intentional, not incidental

### Phase 1 deliverables

- grouped tool progress rendering
- better tool result cards for bash/read/search/MCP/LSP outputs
- cleaner JSON / NDJSON / machine-readable modes
- explicit full-thinking section mode for verbose REPL sessions
- hyperlink-aware output and stronger markdown streaming behavior

### Why phase 1 now

The core renderer is good enough to build on, and this phase directly improves
every session without requiring new distributed infrastructure.

## Phase 2 — Structured transport parity (1 week)

### Phase 2 goals

- make non-interactive output transport-safe
- separate human-readable terminal chrome from machine-readable output

### Phase 2 deliverables

- deterministic JSON mode with no prelude leakage
- optional NDJSON / structured event stream output mode
- clearer transport boundaries between prompt, REPL, and server usage

### Gap addressed by phase 2

Claude Code has deeper structured/remote IO pathways; Emberforge still mixes
human and structured output too easily in some flows.

## Phase 3 — Background task runtime (1-2 weeks)

### Phase 3 goals

- evolve `.ember-agents` from simple manifest tracking into a stronger task runtime
- make long-running automation resumable and observable

### Phase 3 deliverables

- richer task manifests and status transitions
- attach/logs/stop flows that do not require manual digging
- better session/task linkage in HUD and `/tasks`
- basic persistent worker supervision for local long-running tasks

### Gap addressed by phase 3

This is the closest Rust analogue to Claude Code’s background/worker/task stack,
without jumping straight into full daemon complexity.

## Phase 4 — Remote and bridge foundation (2-3 weeks)

### Phase 4 goals

- move beyond simple remote bootstrap helpers toward real remote session plumbing

### Phase 4 deliverables

- stronger session-ingress transport abstraction
- websocket / HTTP transport layer for remote event flow
- clearer remote session lifecycle and auth handling
- align the server crate with future attach/stream use cases

### Gap addressed by phase 4

Claude Code has a large bridge/remote-control platform. Emberforge currently has
pieces of that story, but not the runtime shape.

## Phase 5 — Skills and plugin ecosystem depth (1-2 weeks)

### Phase 5 goals

- turn local skills/plugins into a richer workflow ecosystem

### Phase 5 deliverables

- bundled first-party skills
- better plugin-discovered tools/commands/hooks visibility
- skill metadata improvements and workflow packaging
- reload/discovery ergonomics closer to a real platform

### Gap addressed by phase 5

Emberforge has skills/plugins now, but not the breadth and richness of the
Claude Code ecosystem around them.

## Phase 6 — Higher-order orchestration (2-4 weeks)

Optional, but strategically important if parity ambition remains high.

### Candidate areas for phase 6

- team / multi-agent coordination
- coordinator-style orchestration
- assistant mode expansions
- richer session history and transcript UI

### Why phase 6 comes later

These are expensive layers. The platform foundation, output transport, and task
runtime should mature first.

## Phase 7 — Optional platform layers (longer tail)

These are clearly outside current parity and should be treated as explicit,
optional investments rather than assumed short-term work.

### Candidate areas for phase 7

- voice
- buddy/companion UX
- notifier integrations
- managed settings / policy sync
- analytics / experimentation / remote control gates
- browser / Chrome / computer-use integrations

## Priority summary

| Priority | Area | Why it matters now |
| --- | --- | --- |
| 1 | Output + message surfaces | Direct quality-of-life payoff after UI work |
| 2 | Structured transport cleanup | Needed for correctness and automation friendliness |
| 3 | Background task runtime | Bridges current local core to richer orchestration |
| 4 | Remote / bridge foundation | Biggest missing platform layer |
| 5 | Skills / plugin ecosystem depth | Raises workflow richness |
| 6 | Higher-order orchestration | Valuable, but depends on stronger foundations |
| 7 | Optional platform layers | Long-tail parity, not immediate core need |

## Practical conclusion

The right post-UI plan is **not** “keep building terminal chrome forever.”

The right plan is:

1. finish the output/message presentation layer
2. cleanly separate human vs structured transports
3. strengthen background-task and remote-session foundations
4. then expand the ecosystem and higher-order orchestration layers

That sequence reflects the real current state of Emberforge and the real remaining
distance to Claude Code’s broader stack.
