# EMBER.md

This file provides guidance to Emberforge when working with code in this repository.

## Detected stack

- Languages: Rust.
- Frameworks: none detected from the supported starter markers.

## Verification

- Run Rust verification from the repo root: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- `crates/` and `tests/` are both present; update implementation and validation surfaces together when behavior changes.

## Repository shape

- `crates/` contains the active Rust workspace for the CLI, runtime, tools, telemetry, plugins, server, and support crates.
- `docs/` contains the current roadmap, refinement plan, release notes, and working-routine guidance.
- `tests/` contains end-to-end and smoke validation surfaces that should be reviewed alongside behavior changes.

## Working agreement

- Prefer small, reviewable changes and keep generated bootstrap files aligned with actual repo workflows.
- Keep shared defaults in `.ember.json`; reserve `.ember/settings.local.json` for machine-local overrides.
- Do not overwrite existing `EMBER.md` content automatically; update it intentionally when repo workflows change.
