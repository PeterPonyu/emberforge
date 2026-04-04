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
- automatic restart is now supported via `EMBER_AUTO_RESTART` env var with configurable chain-depth limits, exponential backoff via `AutoRestartPolicy`, and integration into the attach/supervision loop

### Latest verification snapshot

- `cargo test --workspace`
- result: 428 passed, 0 failed, 4 ignored

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

### Priority 1 — Complete Phase 3 background-task runtime

Goal: wrap up the remaining Phase 3 design decisions and close out worker
supervision before moving to platform-level features.

#### Slice 1.1 — Fork-vs-reuse design decision and chain-collapse view

**Background:** Both manual `/tasks restart` and `EMBER_AUTO_RESTART` currently
fork a new successor task.  This preserves audit history but creates visual
clutter when restart chains grow.

**Recommendation:** Keep the fork model (safer, preserves full audit trail).
Add a collapse mode to `/tasks list` that groups restart chains under the
latest successor.

**Implementation plan:**

1. Add `--collapse` flag to `/tasks list` (default off)
2. In `render_task_list_report`, when collapse is active:
   - Group tasks by chain root (walk `predecessors` to find root)
   - Only render the latest successor; show `(+N restarts)` badge
3. Update `push_task_section` to accept a `collapsed: bool` parameter
4. Add 2 tests: collapsed view groups chains, uncollapsed is unchanged
5. Document the design decision in a new `docs/DESIGN_DECISIONS.md`

**Files to touch:**
- `crates/ember-cli/src/task_mgmt.rs` — collapse logic + tests
- `crates/ember-cli/src/main.rs` — parse `--collapse` flag from `/tasks list`
- `docs/DESIGN_DECISIONS.md` — new file

**Estimated test delta:** +2 tests

#### Slice 1.2 — Restart limits and backoff rules

**Background:** `AutoRestartPolicy` has `max_restarts` but no backoff delay.
Rapid successive restarts could thrash the system.

**Implementation plan:**

1. Add `restart_delay_secs` field to `AutoRestartPolicy` (default: 5s)
2. Read `EMBER_AUTO_RESTART_DELAY` env var (seconds, default 5, max 60)
3. In `try_auto_restart`, check the timestamp of the predecessor's interruption;
   skip restart if less than `restart_delay_secs` have elapsed
4. Log the skip as an `auto-restart-delayed` activity entry
5. Add 2 tests: delay is respected, delay expires and restart proceeds

**Files to touch:**
- `crates/ember-cli/src/task_mgmt.rs` — delay logic + tests

**Estimated test delta:** +2 tests

#### Slice 1.3 — Supervision health summary command

**Background:** No single command shows the overall health of all supervised
tasks.  Operators need to check tasks one by one.

**Implementation plan:**

1. Add `/tasks health` subcommand
2. Render a concise table: task id, status, heartbeat age, chain depth,
   auto-restart eligibility, last activity timestamp
3. Summary footer: N healthy, N delayed, N stalled, N interrupted, N chains
4. Add 1 test: health report covers various task states

**Files to touch:**
- `crates/ember-cli/src/task_mgmt.rs` — `render_task_health_report()`
- `crates/ember-cli/src/main.rs` — `/tasks health` handler

**Estimated test delta:** +1 test

---

### Priority 2 — Deepen thinking/transcript UX

Goal: keep Emberforge's existing terminal thinking support, but improve
transcript-grade visibility.

#### Slice 2.1 — Structured thinking replay and export

**Background:** Thinking sections are rendered inline but cannot be replayed
or exported independently.

**Implementation plan:**

1. Add `ThinkingBlock` struct that captures timestamp, content, model
2. Collect thinking blocks during streaming into a `Vec<ThinkingBlock>`
3. Add `/thinking export <path>` command that writes blocks as JSON
4. Add `/thinking replay` that re-renders thinking blocks in order
5. Add 1 test: export produces valid JSON with expected structure

**Files to touch:**
- `crates/ember-cli/src/main.rs` — command handlers
- `crates/ember-cli/src/render.rs` — `ThinkingBlock` struct + export/replay

**Estimated test delta:** +1 test

#### Slice 2.2 — Session transcript organization and search

**Background:** Session history exists but lacks structured search or
transcript-level organization.

**Implementation plan:**

1. Add `TranscriptEntry` struct (role, content, timestamp, tool calls)
2. Append entries to a session-scoped transcript file during conversation
3. Add `/transcript search <query>` command with simple substring matching
4. Add `/transcript export <path>` for full transcript as Markdown or JSON
5. Add 2 tests: search finds matching entries, export produces valid output

**Files to touch:**
- `crates/ember-cli/src/main.rs` — command handlers
- New `crates/ember-cli/src/transcript.rs` module

**Estimated test delta:** +2 tests

#### Slice 2.3 — Session memory extraction

**Background:** No mechanism to extract and persist key insights between
turns within a session.

**Implementation plan:**

1. Add `/memory note <text>` command to persist a note to session memory
2. Store notes in `.ember-sessions/<id>/notes.json`
3. Add `/memory list` to show notes for the current session
4. Render a memory summary hint at session resume if notes exist
5. Add 1 test: note round-trips through write and read

**Estimated test delta:** +1 test

---

### Priority 3 — Strengthen model/provider capability handling

Goal: move from strong core provider plumbing toward richer capability-aware
routing and user-facing advisories.

#### Slice 3.1 — Expand model profile coverage

**Implementation plan:**

1. Add model capability profiles for major providers (Anthropic, OpenAI,
   Gemini, Mistral) beyond the current Ollama-only discovery
2. Store profiles as static data keyed by model name prefix
3. Add `resolve_model_profile(model_name)` that returns capabilities
4. Add 2 tests: known models resolve correctly, unknown returns default

**Files to touch:**
- `crates/api/src/providers/` — profile registry
- `crates/api/src/lib.rs` — public API

**Estimated test delta:** +2 tests

#### Slice 3.2 — Unified capability metadata

**Implementation plan:**

1. Define `ModelCapabilities` struct: tools, thinking, context_window,
   recommended_output_budget, streaming
2. Populate from resolved model profile
3. Thread `ModelCapabilities` into the conversation runtime
4. Add capability checks before tool dispatch (e.g. skip tools if unsupported)

#### Slice 3.3 — Context budget advisories

**Implementation plan:**

1. Before sending a request, check if input tokens approach context budget
2. Emit a warning line in the terminal: "Context is 85% full (N/M tokens)"
3. Add a compaction suggestion when over threshold
4. Add 1 test: advisory fires when context exceeds threshold

---

### Priority 4 — Begin Phase 4 remote/bridge foundation

Goal: go beyond local/runtime-only flows toward a stronger remote session story.

#### Slice 4.1 — Transport abstraction

**Implementation plan:**

1. Define `SessionTransport` trait: `send_event()`, `recv_event()`, `close()`
2. Implement `LocalTransport` (the current stdin/stdout path)
3. Add `TransportEvent` enum covering conversation, tool, and control events
4. Wire `LocalTransport` into the existing conversation loop as a proof of concept

**Files to touch:**
- New `crates/runtime/src/transport.rs`
- `crates/runtime/src/lib.rs`

#### Slice 4.2 — Server endpoint alignment

**Implementation plan:**

1. Add `/api/v1/sessions/:id/events` SSE endpoint to the server crate
2. Bridge server events to `TransportEvent` format
3. Allow `/tasks attach` to connect via HTTP transport instead of local file polling

#### Slice 4.3 — Remote auth and session lifecycle

**Implementation plan:**

1. Add session token generation and validation
2. Add auth middleware for server transport endpoints
3. Define session lifecycle: create, attach, detach, destroy

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

### Slice 1.1 — Fork-vs-reuse design decision and chain-collapse view

Why this next:

- the restart infrastructure (manual + automatic, lineage visibility) is complete
- the remaining design question — fork vs reuse — is the last blocker before Phase 3 can be considered largely done
- implementing chain-collapse view directly addresses the UX issue of restart chain clutter
- this is a focused, testable slice that closes out the core task-supervision story

Concrete steps:

1. Add `docs/DESIGN_DECISIONS.md` documenting the fork-over-reuse decision
2. Add `--collapse` flag to `/tasks list` command parsing in `main.rs`
3. In `render_task_list_report`, group chain members under latest successor when collapse is active
4. Show `(+N restarts)` badge on the collapsed line
5. Add 2 tests: collapsed grouping, uncollapsed remains unchanged
6. Update this plan and push

After this slice, Priority 1 remaining work is slices 1.2 (backoff delay) and
1.3 (health summary), which are incremental improvements rather than design
decisions.

## GitHub publication checklist

For future slices, keep using this publish routine:

1. implement one narrow verified slice
2. run focused crate tests first
3. run `cargo test --workspace`
4. commit the slice cleanly
5. push `main`
