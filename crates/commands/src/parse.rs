
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Help,
    Status,
    Compact,
    Branch {
        action: Option<String>,
        target: Option<String>,
    },
    Bughunter {
        scope: Option<String>,
    },
    Worktree {
        action: Option<String>,
        path: Option<String>,
        branch: Option<String>,
    },
    Commit,
    CommitPushPr {
        context: Option<String>,
    },
    Pr {
        context: Option<String>,
    },
    Issue {
        context: Option<String>,
    },
    Ultraplan {
        task: Option<String>,
    },
    Teleport {
        target: Option<String>,
    },
    DebugToolCall,
    Doctor {
        mode: Option<String>,
    },
    Model {
        model: Option<String>,
    },
    Permissions {
        mode: Option<String>,
    },
    Clear {
        confirm: bool,
    },
    Cost,
    Resume {
        session_path: Option<String>,
    },
    Config {
        section: Option<String>,
    },
    Memory,
    Init,
    Diff,
    Version,
    Export {
        path: Option<String>,
    },
    Session {
        action: Option<String>,
        target: Option<String>,
    },
    Plugins {
        action: Option<String>,
        target: Option<String>,
    },
    Agents {
        args: Option<String>,
    },
    Skills {
        args: Option<String>,
    },
    // ── New commands for TS parity ──
    Hooks,
    Mcp {
        action: Option<String>,
    },
    Plan,
    Tasks {
        action: Option<String>,
    },
    Review {
        scope: Option<String>,
    },
    // ── Tier 2 features ──
    Effort {
        level: Option<String>,
    },
    Theme {
        mode: Option<String>,
    },
    Fast,
    // ── Phase 1: High-priority missing commands ──
    Login {
        provider: Option<String>,
    },
    Logout {
        provider: Option<String>,
    },
    Context {
        action: Option<String>,
    },
    Copy {
        target: Option<String>,
    },
    Files {
        path: Option<String>,
    },
    Tag {
        name: Option<String>,
    },
    Rewind {
        steps: Option<String>,
    },
    // ── Phase 2: Stats & insights ──
    Stats,
    Insights,
    Usage {
        period: Option<String>,
    },
    // ── Phase 3: Advanced commands ──
    Vim,
    Bridge {
        action: Option<String>,
    },
    SecurityReview {
        scope: Option<String>,
    },
    Fork {
        prompt: Option<String>,
    },
    // ── Phase 4: Stretch goal commands ──
    Voice,
    Buddy {
        task: Option<String>,
    },
    Peers {
        action: Option<String>,
    },
    Proactive,
    Unknown(String),
}

impl SlashCommand {
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let mut parts = trimmed.trim_start_matches('/').split_whitespace();
        let command = parts.next().unwrap_or_default();
        Some(match command {
            "help" => Self::Help,
            "status" => Self::Status,
            "compact" => Self::Compact,
            "branch" => Self::Branch {
                action: parts.next().map(ToOwned::to_owned),
                target: parts.next().map(ToOwned::to_owned),
            },
            "bughunter" => Self::Bughunter {
                scope: remainder_after_command(trimmed, command),
            },
            "worktree" => Self::Worktree {
                action: parts.next().map(ToOwned::to_owned),
                path: parts.next().map(ToOwned::to_owned),
                branch: parts.next().map(ToOwned::to_owned),
            },
            "commit" => Self::Commit,
            "commit-push-pr" => Self::CommitPushPr {
                context: remainder_after_command(trimmed, command),
            },
            "pr" => Self::Pr {
                context: remainder_after_command(trimmed, command),
            },
            "issue" => Self::Issue {
                context: remainder_after_command(trimmed, command),
            },
            "ultraplan" => Self::Ultraplan {
                task: remainder_after_command(trimmed, command),
            },
            "teleport" => Self::Teleport {
                target: remainder_after_command(trimmed, command),
            },
            "debug-tool-call" => Self::DebugToolCall,
            "doctor" => Self::Doctor {
                mode: remainder_after_command(trimmed, command),
            },
            "model" => Self::Model {
                model: parts.next().map(ToOwned::to_owned),
            },
            "permissions" => Self::Permissions {
                mode: parts.next().map(ToOwned::to_owned),
            },
            "clear" => Self::Clear {
                confirm: parts.next() == Some("--confirm"),
            },
            "cost" => Self::Cost,
            "resume" => Self::Resume {
                session_path: parts.next().map(ToOwned::to_owned),
            },
            "config" => Self::Config {
                section: parts.next().map(ToOwned::to_owned),
            },
            "memory" => Self::Memory,
            "init" => Self::Init,
            "diff" => Self::Diff,
            "version" => Self::Version,
            "export" => Self::Export {
                path: parts.next().map(ToOwned::to_owned),
            },
            "session" => Self::Session {
                action: parts.next().map(ToOwned::to_owned),
                target: parts.next().map(ToOwned::to_owned),
            },
            "plugin" | "plugins" | "marketplace" => Self::Plugins {
                action: parts.next().map(ToOwned::to_owned),
                target: {
                    let remainder = parts.collect::<Vec<_>>().join(" ");
                    (!remainder.is_empty()).then_some(remainder)
                },
            },
            "agents" => Self::Agents {
                args: remainder_after_command(trimmed, command),
            },
            "skills" => Self::Skills {
                args: remainder_after_command(trimmed, command),
            },
            // ── New commands for TS parity ──
            "hooks" => Self::Hooks,
            "mcp" => Self::Mcp {
                action: parts.next().map(ToOwned::to_owned),
            },
            "plan" => Self::Plan,
            "tasks" | "task" => Self::Tasks {
                action: remainder_after_command(trimmed, command),
            },
            "review" => Self::Review {
                scope: remainder_after_command(trimmed, command),
            },
            // ── Tier 2 features ──
            "effort" => Self::Effort {
                level: parts.next().map(ToOwned::to_owned),
            },
            "theme" => Self::Theme {
                mode: parts.next().map(ToOwned::to_owned),
            },
            "fast" => Self::Fast,
            // ── Phase 1 ──
            "login" => Self::Login {
                provider: parts.next().map(ToOwned::to_owned),
            },
            "logout" => Self::Logout {
                provider: parts.next().map(ToOwned::to_owned),
            },
            "context" | "ctx" => Self::Context {
                action: parts.next().map(ToOwned::to_owned),
            },
            "copy" | "cp" => Self::Copy {
                target: remainder_after_command(trimmed, command),
            },
            "files" | "ls" => Self::Files {
                path: parts.next().map(ToOwned::to_owned),
            },
            "tag" => Self::Tag {
                name: parts.next().map(ToOwned::to_owned),
            },
            "rewind" => Self::Rewind {
                steps: parts.next().map(ToOwned::to_owned),
            },
            // ── Phase 2 ──
            "stats" => Self::Stats,
            "insights" => Self::Insights,
            "usage" => Self::Usage {
                period: parts.next().map(ToOwned::to_owned),
            },
            // ── Phase 3 ──
            "vim" => Self::Vim,
            "bridge" => Self::Bridge {
                action: parts.next().map(ToOwned::to_owned),
            },
            "security-review" | "secreview" => Self::SecurityReview {
                scope: remainder_after_command(trimmed, command),
            },
            "fork" => Self::Fork {
                prompt: remainder_after_command(trimmed, command),
            },
            // ── Phase 4 ──
            "voice" => Self::Voice,
            "buddy" => Self::Buddy {
                task: remainder_after_command(trimmed, command),
            },
            "peers" | "teammates" => Self::Peers {
                action: parts.next().map(ToOwned::to_owned),
            },
            "proactive" => Self::Proactive,
            other => Self::Unknown(other.to_string()),
        })
    }
}

fn remainder_after_command(input: &str, command: &str) -> Option<String> {
    input
        .trim()
        .strip_prefix(&format!("/{command}"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

