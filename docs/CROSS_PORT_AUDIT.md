# Cross-Port Hook / Lifecycle / API Contract Audit

Status: draft (worker-3, 2026-04-16)
Scope: `emberforge` (Rust reference) vs. `emberforge-ts`, `emberforge-go`,
`emberforge-cpp` sibling ports under `/home/zeyufu/Desktop/emberforge-translations/`.
Authoritative contract: [`CROSS_PORT_CONTRACT.md`](CROSS_PORT_CONTRACT.md).

## 1. CI gate snapshot

Branch: `feature/buddy-reference-parity` (workspace root: `/home/zeyufu/Desktop/emberforge`).
The workflow in `.github/workflows/ci.yml` runs three gates on `ubuntu-latest`
and `macos-latest`:

| Gate | Command | Result on this branch |
| --- | --- | --- |
| Compile | `cargo check --workspace` | PASS (exit 0) |
| Tests | `cargo test --workspace` | PASS (exit 0, all suites green) |
| Release | `cargo build --release` | PASS (exit 0, 6 `dead_code` warnings only) |

No CI-blocking failures are present on the reference implementation. The 6
warnings originate in `crates/ember-cli/src/vim.rs` (unused public/private
methods on `VimState`); they are non-fatal but are the only visible
maintainability debt the release build surfaces today. Because they live in
files already modified by worker-1, they are deliberately **out of scope for
worker-3**; flag them for a follow-up rather than touch a shared file.

## 2. Hook / lifecycle contract gap matrix

`CROSS_PORT_CONTRACT.md` §4 defines 17 hook events, two backends
(`command`, `http`), a match-rule grammar, and exit-code deny/warn semantics.
It also defines plugin lifecycle (`Init` / `Shutdown`) in §5.

| Capability (contract) | Rust (`crates/runtime/src/hooks.rs`, `crates/plugins/src/manager.rs`) | TS (`emberforge-ts/packages/*`) | Go (`emberforge-go/pkg/*`) | C++ (`emberforge-cpp/include+src/*`) |
| --- | --- | --- | --- | --- |
| `HookEvent` enum (17 variants) | Present, snake-case serialization | **Missing** | **Missing** | **Missing** |
| `HookBackend::Command` w/ exit-code semantics (0 allow, 2 deny, other warn) | Present | **Missing** | **Missing** | **Missing** |
| `HookBackend::Http` (curl POST, JSON payload) | Present (curl shell-out) | **Missing** | **Missing** | **Missing** |
| `HookMatchRule` (`tool_names`, `commands` globs) | Present | **Missing** | **Missing** | **Missing** |
| Lifecycle dispatch (`fire_event`, `fire_event_with_context`) | Present | **Missing** | **Missing** | **Missing** |
| `PluginLifecycle { init, shutdown }` manifest fields | Present (`manager.rs:544..648`) | **Missing** — `Plugin` is `{metadata, validate}` only | **Missing** — same skeletal interface | **Missing** — same skeletal interface |
| `PluginToolManifest` / `PluginCommandManifest` | Present | **Missing** | **Missing** | **Missing** |
| Conformance checklist items §11 (hook events, hook backends, plugin manifest) | Partially satisfied — live runtime dispatch is narrower than the full declared contract | Unsatisfied | Unsatisfied | Unsatisfied |

### Evidence pointers

- Rust hooks: `crates/runtime/src/hooks.rs:11-60` (events) and
  `:190-354` (runner + exit-code semantics).
- Rust plugin lifecycle: `crates/plugins/src/manager.rs:521-648` and
  `:981-1020` (`run_lifecycle_commands`).
- TS plugin surface: `emberforge-translations/emberforge-ts/packages/plugins/src/types.ts:1-12` — only `PluginMetadata` + `Plugin.validate()`.
- Go plugin surface: `emberforge-translations/emberforge-go/pkg/plugins/types.go:1-14` — same shape.
- C++ plugin surface: `emberforge-translations/emberforge-cpp/include/emberforge/plugins/plugin.hpp:5-30` — same shape, `ExamplePlugin` stub only.
- Cross-port search for `Hook*`, `PreToolUse`, `SessionStart`, … under
  `emberforge-translations/` returns **zero matches in source files**
  (README hits only).

## 3. Observations about the existing contract

1. **Conformance checklist is aspirational and partially ahead of Rust
   runtime wiring.** §11 marks hook backends and plugin manifests as
   required; the three translation ports would all fail those checks,
   and even the Rust reference currently dispatches only the pre/post
   tool subset in production flow. The checklist should either be scoped
   per port or a status column added so reviewers can distinguish "not
   yet done" from "divergent on purpose".
2. **Port Divergence Log (§9) does not record the hook gap.** Every row
   explains a difference in *how* each port satisfies a feature; none
   explain that three ports simply do not implement hooks or
   plugin lifecycle yet. Future reviewers will correctly (and noisily)
   flag that as drift.
3. **HTTP backend in Rust shells out to `curl`.** That is fine as an
   iter-2 implementation, but it is a real portability footgun (Windows
   without curl, sandboxed CI) that should be noted rather than left
   implicit. The `run_http_hook` function is currently gated with
   `#[allow(dead_code)]`, which is itself a signal: the HTTP backend is
   declared on the wire but not yet wired into the dispatcher.
4. **`UserPromptSubmit`, `Notification`, `PluginLoad`/`PluginUnload`,
   `CwdChanged`, `FileChanged`** are listed as first-class events but
   do not currently have production dispatch call sites in the Rust
   runtime. The enum variants are still useful for wire compatibility,
   but the absence of dispatchers means the Rust reference does not yet
   satisfy the full hook contract it declares.

## 4. Bounded maintainability recommendations

Ranked by risk/reward, smallest first. Each item is contained to a
single contract-owner file or doc; none cross another worker's lane.

| # | Change | Owner file(s) | Why |
| --- | --- | --- | --- |
| R1 | Add a **"Hook / Lifecycle Coverage"** subsection to §9 (Port Divergence Log) recording that TS/Go/C++ intentionally ship without hooks or plugin lifecycle in iter-2, with a link to this audit. | `docs/CROSS_PORT_CONTRACT.md` | Reviewers stop re-flagging the gap; the drift is documented not denied. |
| R2 | In §11 conformance checklist, split the hooks and plugin manifest bullets into "reference port (Rust)" vs. "translation ports" so the boxes accurately reflect scope. | `docs/CROSS_PORT_CONTRACT.md` | Currently every translation port fails the checklist as written. |
| R3 | In the Rust hook runner, either delete `run_http_hook` or flip it behind a feature flag with a `TODO` that matches the iter plan. Leaving `#[allow(dead_code)]` on wire-level code silently lets the HTTP contract rot. | `crates/runtime/src/hooks.rs` (owner: worker-1) | Out of scope for this worker — logged here as a follow-up item. |
| R4 | Add a one-paragraph **"How events are dispatched today"** note in `docs/CROSS_PORT_CONTRACT.md` near §4, listing which of the 17 events currently have in-tree call sites vs. which are reserved for future use. | `docs/CROSS_PORT_CONTRACT.md` | Prevents port authors from assuming every event is wired. |
| R5 | Publish this audit (`docs/CROSS_PORT_AUDIT.md`) and reference it from `docs/PARITY.md`. | `docs/CROSS_PORT_AUDIT.md` (new), `docs/PARITY.md` | Makes the gap tracker discoverable from the existing audit index. |

Items R1, R2, R4, R5 are documentation-only and are the bounded
improvements this worker can safely implement. R3 is flagged for
worker-1 (implementation lane) to avoid conflicting edits on
`crates/runtime/src/hooks.rs`.

## 5. Verification evidence

- `cargo check --workspace` — PASS (exit 0), log at
  `/tmp/claude-1000/-home-zeyufu-Desktop/c4837e66-3514-4cee-b775-04198738bec3/tasks/by4aro1p1.output`.
- `cargo test --workspace` — PASS (exit 0), log at `…/b4e9r8obu.output`.
- `cargo build --release` — PASS (exit 0, 6 non-fatal `dead_code`
  warnings in `crates/ember-cli/src/vim.rs`), log at `…/bmdfjjrgs.output`.
- Cross-port hook/lifecycle searches produce **zero source-file hits**
  under `emberforge-translations/` (see §2 evidence pointers), which
  the matrix faithfully reflects.

## 6. Out-of-scope notes

Worker-1 (implementation) owns source edits under `crates/**`; worker-2
(test) owns new test scaffolding. This audit intentionally leaves
those lanes alone. The only code-level follow-up surfaced here (R3) is
logged for worker-1 rather than applied.
