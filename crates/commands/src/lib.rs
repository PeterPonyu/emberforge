mod handlers;
mod help;
mod parse;
mod spec;

#[cfg(test)]
mod tests;

// Re-export the public API (same surface as before the split).
pub use handlers::{
    detect_default_branch, handle_agents_slash_command, handle_branch_slash_command,
    handle_commit_push_pr_slash_command, handle_commit_slash_command,
    handle_plugins_slash_command, handle_skills_slash_command, handle_slash_command,
    handle_worktree_slash_command, render_plugins_report, CommitPushPrRequest,
    PluginsCommandResult, SlashCommandResult,
};
pub use help::{render_slash_command_help, suggest_slash_commands};
pub use parse::SlashCommand;
pub use spec::{
    resume_supported_slash_commands, slash_command_specs, CommandManifestEntry, CommandRegistry,
    CommandSource, SlashCommandCategory, SlashCommandSpec,
};
