# Changelog

All notable changes to Emberforge are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- MIT `LICENSE` file (Cargo.toml already declared `license = "MIT"`)
- `CHANGELOG.md` (this file)
- Release-receipt CI gate job on tag push
- Deprecation warnings when legacy `.claw/` paths are loaded
- `/model` slash-command argument hint now includes `auto` routing mode

### Changed
- `CONTRIBUTING.md`: removed stale "parent repo / `cd rust/`" instruction
- `docs/PARITY.md`: added dated banner noting the document's last-update scope
- `crates/commands/src/spec.rs`: `/model` hint updated to `[auto|<model>|list]`

---

## [0.1.0] — release candidate (unreleased)

### Summary

Emberforge `0.1.0` is the first public release-prep milestone for the current Rust
implementation. Emberforge is Claude Code inspired and built as a clean-room Rust
implementation; it is not a direct port or copy. This release centers on a usable
local CLI experience: interactive sessions, non-interactive prompts, workspace tools,
configuration loading, sessions, plugins, and local agent/skill discovery.

### Highlights

- Initial public `0.1.0` release candidate for Emberforge
- Safe-Rust implementation as the current primary product surface
- `ember` CLI for interactive and one-shot coding-agent workflows
- Built-in workspace tools for shell, file operations, search, web fetch/search,
  todo tracking, and notebook updates
- Slash-command surface for status, compaction, config inspection, sessions,
  diff/export, and version info
- Local plugin, agent, and skill discovery/management surfaces
- OAuth login/logout plus model/provider selection

### Known limitations

- Source-build distribution only; packaged release artifacts are not yet published
- CI covers Ubuntu and macOS release builds, checks, and tests
- Windows release readiness is not yet established
- Some integration coverage is opt-in (live provider credentials required)
- Public interfaces may continue to evolve during the `0.x` release line

<!-- Link references will be added once v0.1.0 is tagged and published.
[Unreleased]: https://github.com/PeterPonyu/emberforge/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/PeterPonyu/emberforge/releases/tag/v0.1.0
-->
