# Release process

Emberforge releases are intentionally small and evidence-backed.

1. Confirm the working tree is clean except for the release commit.
2. Run local gates:
   - `cargo check --workspace`
   - `cargo clippy --workspace --all-targets`
   - `cargo test --workspace`
   - `cargo build --release`
3. Update `docs/releases/<version>.md` with shipped changes, known gaps, and verification evidence.
4. Open a focused release PR; do not mix release metadata with unrelated runtime changes.
5. After merge, tag the release from `main` and publish only from CI.

Known long-tail work should stay in issues instead of being hidden in release notes.
