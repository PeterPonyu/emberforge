# Emberforge next steps plan and working routine

Last updated: 2026-04-04

## Current verified baseline

The repository is already materially beyond the earlier baseline layout.

- workspace was restructured to a top-level Rust workspace under `crates/`
- CLI, runtime, tools, telemetry, plugins, server, and docs have all been expanded
- terminal UI work is already present: banner modes, intro animation, HUD, markdown rendering, thinking preview/section output
- background task runtime work has already advanced in the current branch: task manifests, `/tasks list`, `/tasks show`, `/tasks logs`, `/tasks attach`, `/tasks stop`, session/task linkage in the HUD, better log tailing, and stop-aware subagent execution
- heartbeat-backed supervision is now surfaced explicitly in task reports (`/tasks list`, `/tasks show`, `/tasks logs`, and attach/watch transitions)

### Latest verification snapshot

- `cargo test --workspace`
- result: 414 passed, 0 failed, 4 ignored

## GitHub publication status

Local repository state can be committed now.

Actual GitHub repository creation/push is currently blocked by invalid GitHub CLI authentication on this machine.

Observed status:

- `gh` is installed
- active GitHub account: `PeterPonyu`
- current token is invalid and needs re-authentication before `gh repo create` / push can succeed

## Recommended implementation priorities

These priorities follow `docs/REFINEMENT_PLAN.md` and the comparison audit against:

- `claude-code-src`
- `oh-my-claudecode-main`
- `oh-my-codex-main`
- `claw-code-parity-main`

### Priority 1 — Continue Phase 3 background-task runtime

Goal: move from improved task inspection toward basic worker supervision.

Recommended slice order:

1. add task activity/audit log entries for lifecycle changes
2. add recovery/restart policy for interrupted local workers where safe
3. surface restart/recovery hints more explicitly in `/tasks show`
4. decide whether local worker restart should be automatic or operator-triggered

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

### Add a task activity/audit timeline for lifecycle changes

Why this next:

- it builds directly on the new supervision layer
- it improves observability without forcing automatic restarts yet
- it prepares the task runtime for richer attach/recovery and future remote orchestration

Suggested acceptance criteria:

- task lifecycle changes produce concise activity records
- `/tasks show` or a related surface can display recent activity entries
- attach/watch output stays readable and does not duplicate unchanged events
- focused tests and full workspace tests both pass

## GitHub publication checklist

Once GitHub CLI authentication is fixed, do this next:

1. create the GitHub repository for `emberforge`
2. set `origin`
3. push the current `main` branch
4. continue pushing one verified feature slice at a time

If repository visibility needs a decision, prefer making that explicit before creation:

- public if this is intended as the new canonical open baseline
- private if the branch still contains work that should be curated before release
