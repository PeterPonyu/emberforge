# Emberforge next steps plan and working routine

Last updated: 2026-04-04

## Current verified baseline

The repository is already materially beyond the earlier baseline layout.

- workspace was restructured to a top-level Rust workspace under `crates/`
- CLI, runtime, tools, telemetry, plugins, server, and docs have all been expanded
- terminal UI work is already present: banner modes, intro animation, HUD, markdown rendering, thinking preview/section output
- background task runtime work has already advanced in the current branch: task manifests, `/tasks list`, `/tasks show`, `/tasks logs`, `/tasks attach`, `/tasks stop`, session/task linkage in the HUD, better log tailing, and stop-aware subagent execution
- heartbeat-backed supervision is now surfaced explicitly in task reports (`/tasks list`, `/tasks show`, `/tasks logs`, and attach/watch transitions)
- task lifecycle activity history is now persisted in manifests and surfaced in `/tasks show` plus attach/watch updates
- `/tasks show` now includes concise restart/recovery hints for stalled and interrupted tasks without polluting healthy task reports
- interrupted local subagent tasks can now spawn a safe replacement via `/tasks restart <id>` when recoverable prompt/state metadata is available
- restart lineage is now surfaced in both `/tasks list` (concise ↳/→ tags) and `/tasks show` (Predecessor/Successor fields) using a pre-computed lineage map

### Latest verification snapshot

- `cargo test --workspace`
- result: 423 passed, 0 failed, 4 ignored

## GitHub publication status

Repository publication is already in place.

Observed status:

- GitHub repository exists: `PeterPonyu/emberforge`
- `main` is pushed and tracking `origin/main`
- future slices can be committed and pushed incrementally

## Recommended implementation priorities

These priorities follow `docs/REFINEMENT_PLAN.md` and the comparison audit against:

- `claude-code-src`
- `oh-my-claudecode-main`
- `oh-my-codex-main`
- `claw-code-parity-main`

### Priority 1 — Continue Phase 3 background-task runtime

Goal: move from improved task inspection toward basic worker supervision.

Recommended slice order:

1. decide whether local worker restart should remain operator-triggered or gain an automatic mode
2. decide whether recovery should reuse the same task id or continue forking successor tasks
3. clarify restart limits/backoff rules if repeated interruption occurs

Primary reference:

- `oh-my-claudecode-main/src/team/heartbeat.ts`
- `oh-my-claudecode-main/src/team/activity-log.ts`
- `oh-my-claudecode-main/src/team/worker-health.ts`

### Priority 2 — Deepen thinking/transcript UX

Goal: keep Emberforge's existing terminal thinking support, but improve transcript-grade visibility.

Recommended slice order:

1. enrich thinking sections with better structured replay/export behavior
2. add stronger session transcript organization and searchable history
3. add lightweight session-memory extraction or summary notes between turns

Primary reference:

- `claude-code-src/components/messages/AssistantThinkingMessage.tsx`
- `claude-code-src/services/sessionTranscript/sessionTranscript.ts`
- `claude-code-src/services/SessionMemory/sessionMemory.ts`

### Priority 3 — Strengthen model/provider capability handling

Goal: move from strong core provider plumbing to richer capability-aware routing.

Recommended slice order:

1. expand model profile coverage beyond Ollama-only context discovery
2. add unified provider capability metadata (tools, thinking, context, recommended output budget)
3. improve user-facing context-budget advisories before overflow

Primary reference:

- `claude-code-src/utils/context.ts`
- `claude-code-src/cost-tracker.ts`

### Priority 4 — Begin Phase 4 remote/bridge foundation

Goal: go beyond local/runtime-only flows toward a stronger remote session story.

Recommended slice order:

1. define transport abstraction for session ingress/egress
2. align `server` endpoints with future attach/stream clients
3. add clearer remote auth/session lifecycle boundaries

Primary reference:

- `claude-code-src/bridge/bridgeMain.ts`
- `claude-code-src/cli/transports/HybridTransport.ts`

## Working routine for future iterations

Use this as the default delivery routine for each new slice.

### 1. Choose one narrow slice

Pick exactly one verifiable improvement from the active priority phase.

Examples:

- heartbeat freshness in task manifests
- task activity log rendering
- transcript search/report command
- provider capability warning before context overflow

### 2. Confirm current behavior first

- inspect the relevant implementation and tests
- identify whether the gap is fully missing or only partial
- prefer changes that improve user-visible behavior and test coverage together

### 3. Implement incrementally

- keep edits small and localized
- preserve public APIs unless the slice explicitly needs expansion
- avoid mixing unrelated cleanup into feature work

### 4. Verify twice

Minimum verification routine:

1. run the focused crate tests first
2. run `cargo test --workspace` after the focused suite passes

### 5. Update docs as part of the slice

For every meaningful runtime feature change:

- update `docs/REFINEMENT_PLAN.md` only if priorities or phase meaning changed
- update this file when the recommended next slice changes
- note any newly closed gap or newly discovered blocker

### 6. Commit discipline

Recommended commit shape:

- one commit per meaningful slice
- message format:
  - `feat: ...` for visible functionality
  - `fix: ...` for correctness bugs
  - `docs: ...` for docs-only updates
  - `refactor: ...` for structural cleanup without behavior changes

### 7. Push discipline

Before every push:

- make sure session/telemetry artifacts are ignored
- confirm no temporary scratch files are staged
- verify the workspace tests are still green

## Suggested immediate next slice

If continuing immediately after the current branch state, the best next implementation target is:

### Decide whether local worker restart should remain operator-triggered or gain an automatic mode

Why this next:

- restart flow and lineage visibility are both in place
- the current design requires `/tasks restart <id>` — an explicit operator action
- deciding between operator-triggered and automatic restart shapes the supervision architecture
- automatic mode (with backoff/limits) would reduce operator burden for transient failures

Suggested acceptance criteria:

- document the design decision and rationale clearly
- if automatic mode is chosen, implement configurable retry policy (max retries, backoff)
- restart limits should prevent infinite restart loops
- focused tests and full workspace tests both pass

## GitHub publication checklist

For future slices, keep using this publish routine:

1. implement one narrow verified slice
2. run focused crate tests first
3. run `cargo test --workspace`
4. commit the slice cleanly
5. push `main`
