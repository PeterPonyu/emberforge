
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandManifestEntry {
    pub name: String,
    pub source: CommandSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    Builtin,
    InternalOnly,
    FeatureGated,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandRegistry {
    entries: Vec<CommandManifestEntry>,
}

impl CommandRegistry {
    #[must_use]
    pub fn new(entries: Vec<CommandManifestEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[CommandManifestEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandCategory {
    Core,
    Workspace,
    Session,
    Git,
    Automation,
}

impl SlashCommandCategory {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Core => "Core flow",
            Self::Workspace => "Workspace & memory",
            Self::Session => "Sessions & output",
            Self::Git => "Git & GitHub",
            Self::Automation => "Automation & discovery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
    pub argument_hint: Option<&'static str>,
    pub resume_supported: bool,
    pub category: SlashCommandCategory,
}

pub(crate) const SLASH_COMMAND_SPECS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        name: "help",
        aliases: &[],
        summary: "Show available slash commands",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "status",
        aliases: &[],
        summary: "Show current session status",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "compact",
        aliases: &[],
        summary: "Compact local session history",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "doctor",
        aliases: &[],
        summary: "Run cached setup diagnostics or the optional family audit",
        argument_hint: Some("[quick|full|status|reset]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "model",
        aliases: &[],
        summary: "Show, list, or switch the active model",
        argument_hint: Some("[model|list]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "permissions",
        aliases: &[],
        summary: "Show or switch the active permission mode",
        argument_hint: Some("[read-only|workspace-write|danger-full-access]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "clear",
        aliases: &[],
        summary: "Start a fresh local session",
        argument_hint: Some("[--confirm]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "cost",
        aliases: &[],
        summary: "Show cumulative token usage for this session",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "resume",
        aliases: &[],
        summary: "Load a saved session into the REPL",
        argument_hint: Some("<session-path>"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "config",
        aliases: &[],
        summary: "Inspect Emberforge config files or merged sections",
        argument_hint: Some("[env|hooks|model|plugins]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "memory",
        aliases: &[],
        summary: "Inspect loaded Emberforge instruction memory files",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "init",
        aliases: &[],
        summary: "Create a starter EMBER.md for this repo",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "diff",
        aliases: &[],
        summary: "Show git diff for current workspace changes",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "version",
        aliases: &[],
        summary: "Show CLI version and build information",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "bughunter",
        aliases: &[],
        summary: "Inspect the codebase for likely bugs",
        argument_hint: Some("[scope]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "branch",
        aliases: &[],
        summary: "List, create, or switch git branches",
        argument_hint: Some("[list|create <name>|switch <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    SlashCommandSpec {
        name: "worktree",
        aliases: &[],
        summary: "List, add, remove, or prune git worktrees",
        argument_hint: Some("[list|add <path> [branch]|remove <path>|prune]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    SlashCommandSpec {
        name: "commit",
        aliases: &[],
        summary: "Generate a commit message and create a git commit",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    SlashCommandSpec {
        name: "commit-push-pr",
        aliases: &[],
        summary: "Commit workspace changes, push the branch, and open a PR",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    SlashCommandSpec {
        name: "pr",
        aliases: &[],
        summary: "Draft or create a pull request from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    SlashCommandSpec {
        name: "issue",
        aliases: &[],
        summary: "Draft or create a GitHub issue from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    SlashCommandSpec {
        name: "ultraplan",
        aliases: &[],
        summary: "Run a deep planning prompt with multi-step reasoning",
        argument_hint: Some("[task]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "teleport",
        aliases: &[],
        summary: "Jump to a file or symbol by searching the workspace",
        argument_hint: Some("<symbol-or-path>"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "debug-tool-call",
        aliases: &[],
        summary: "Replay the last tool call with debug details",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "export",
        aliases: &[],
        summary: "Export the current conversation to a file",
        argument_hint: Some("[file]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "session",
        aliases: &[],
        summary: "List or switch managed local sessions",
        argument_hint: Some("[list|switch <session-id>]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "plugin",
        aliases: &["plugins", "marketplace"],
        summary: "Manage Emberforge plugins",
        argument_hint: Some(
            "[list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]",
        ),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "agents",
        aliases: &[],
        summary: "List configured agents",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "skills",
        aliases: &[],
        summary: "List available skills",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Automation,
    },
    // ── New commands for TS parity ──
    SlashCommandSpec {
        name: "hooks",
        aliases: &[],
        summary: "List configured hooks",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "mcp",
        aliases: &[],
        summary: "List MCP servers and their status",
        argument_hint: Some("[list|connect|disconnect]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "plan",
        aliases: &[],
        summary: "Toggle plan mode (design without executing)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "tasks",
        aliases: &["task"],
        summary: "List and manage background tasks",
        argument_hint: Some("[list|show <id>|logs <id>|attach <id>|stop <id>|restart <id>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "review",
        aliases: &[],
        summary: "Trigger a code review of recent changes",
        argument_hint: Some("[scope]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    // ── Tier 2 features ──
    SlashCommandSpec {
        name: "effort",
        aliases: &[],
        summary: "Show or set the reasoning effort level",
        argument_hint: Some("[relaxed|balanced|thorough]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "theme",
        aliases: &[],
        summary: "Switch the terminal color theme",
        argument_hint: Some("[dark|light]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "fast",
        aliases: &[],
        summary: "Toggle fast mode (prioritize speed over thoroughness)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    // ── Phase 1: Missing high-priority commands ────────────────────
    SlashCommandSpec {
        name: "login",
        aliases: &[],
        summary: "Authenticate with a cloud provider (Anthropic, xAI)",
        argument_hint: Some("[provider]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "logout",
        aliases: &[],
        summary: "Clear stored credentials for a cloud provider",
        argument_hint: Some("[provider]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "context",
        aliases: &["ctx"],
        summary: "Display and manipulate the current conversation context",
        argument_hint: Some("[show|clear|size]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "copy",
        aliases: &["cp"],
        summary: "Copy text or file content to the clipboard",
        argument_hint: Some("[last|file <path>|selection]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "files",
        aliases: &["ls"],
        summary: "List files in the current workspace",
        argument_hint: Some("[path] [--tree]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
    },
    SlashCommandSpec {
        name: "tag",
        aliases: &[],
        summary: "Tag the current conversation for later reference",
        argument_hint: Some("<tag-name>"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "rewind",
        aliases: &[],
        summary: "Rewind the conversation to a previous turn",
        argument_hint: Some("[steps]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
    },
    // ── Phase 2: Stats & insights commands ─────────────────────────
    SlashCommandSpec {
        name: "stats",
        aliases: &[],
        summary: "Show session statistics (turns, tokens, tools used)",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "insights",
        aliases: &[],
        summary: "Show analytical insights about the current session",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Session,
    },
    SlashCommandSpec {
        name: "usage",
        aliases: &[],
        summary: "Show API usage summary across sessions",
        argument_hint: Some("[today|week|month]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
    },
    // ── Phase 3: Advanced commands ─────────────────────────────────
    SlashCommandSpec {
        name: "vim",
        aliases: &[],
        summary: "Toggle vim keybindings for the input editor",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "bridge",
        aliases: &[],
        summary: "Start or stop IDE bridge mode",
        argument_hint: Some("[start|stop|status]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "security-review",
        aliases: &["secreview"],
        summary: "Run a security audit on recent changes",
        argument_hint: Some("[scope]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
    },
    SlashCommandSpec {
        name: "fork",
        aliases: &[],
        summary: "Fork the current conversation into a new sub-agent",
        argument_hint: Some("[prompt]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    // ── Phase 4: Stretch goal commands ─────────────────────────────
    SlashCommandSpec {
        name: "voice",
        aliases: &[],
        summary: "Toggle voice input mode",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
    },
    SlashCommandSpec {
        name: "buddy",
        aliases: &[],
        summary: "Start a collaborative buddy agent",
        argument_hint: Some("[task]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "peers",
        aliases: &["teammates"],
        summary: "List and manage connected peers/teammates",
        argument_hint: Some("[list|invite|remove]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
    SlashCommandSpec {
        name: "proactive",
        aliases: &[],
        summary: "Toggle proactive suggestions",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Automation,
    },
];

#[must_use]
pub fn slash_command_specs() -> &'static [SlashCommandSpec] {
    SLASH_COMMAND_SPECS
}

#[must_use]
pub fn resume_supported_slash_commands() -> Vec<&'static SlashCommandSpec> {
    slash_command_specs()
        .iter()
        .filter(|spec| spec.resume_supported)
        .collect()
}

