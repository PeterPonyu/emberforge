use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
    Prompt,
    Allow,
}

impl PermissionMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
            Self::Prompt => "prompt",
            Self::Allow => "allow",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub input: String,
    pub current_mode: PermissionMode,
    pub required_mode: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionPromptDecision {
    Allow,
    Deny { reason: String },
}

pub trait PermissionPrompter {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionOutcome {
    Allow,
    Deny { reason: String },
}

// ── Permission rule system ─────────────────────────────────────────────

/// Source of a permission rule (for debugging/auditing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleSource {
    /// From user-level settings (~/.ember/settings.json).
    UserSettings,
    /// From project-level settings (.ember/settings.json).
    ProjectSettings,
    /// From local settings (.ember/settings.local.json).
    LocalSettings,
    /// From a CLI argument (--allow, --deny).
    CliArg,
    /// From a policy file (enterprise/MDM).
    PolicySettings,
    /// From the current session (runtime override).
    Session,
}

/// Behavior of a permission rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleBehavior {
    Allow,
    Deny,
    Ask,
}

/// A permission rule that matches tool calls by name and optional content pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRule {
    /// The tool name this rule applies to (e.g., "bash", "write_file").
    pub tool_name: String,
    /// Optional content pattern to match against the input.
    /// Supports simple glob: "git *" matches any input containing "git ".
    pub content_pattern: Option<String>,
    /// What to do when this rule matches.
    pub behavior: RuleBehavior,
    /// Where this rule came from.
    pub source: RuleSource,
}

impl PermissionRule {
    /// Check if this rule matches the given tool name and input.
    #[must_use]
    pub fn matches(&self, tool_name: &str, input: &str) -> bool {
        if self.tool_name != tool_name {
            return false;
        }
        match &self.content_pattern {
            None => true,
            Some(pattern) => {
                let p = pattern.to_ascii_lowercase();
                let input_lower = input.to_ascii_lowercase();
                if p.ends_with('*') {
                    input_lower.contains(&p[..p.len() - 1])
                } else {
                    input_lower.contains(&p)
                }
            }
        }
    }
}

/// Tracks denied tool calls for pattern detection.
#[derive(Debug, Clone, Default)]
pub struct DenialTracker {
    /// History of denied (tool_name, reason) pairs.
    denials: Vec<(String, String)>,
}

impl DenialTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a denial.
    pub fn record(&mut self, tool_name: &str, reason: &str) {
        self.denials
            .push((tool_name.to_string(), reason.to_string()));
    }

    /// Get total denial count.
    #[must_use]
    pub fn total_denials(&self) -> usize {
        self.denials.len()
    }

    /// Get denial count for a specific tool.
    #[must_use]
    pub fn denials_for_tool(&self, tool_name: &str) -> usize {
        self.denials
            .iter()
            .filter(|(name, _)| name == tool_name)
            .count()
    }

    /// Check if a tool has been repeatedly denied (3+ times).
    #[must_use]
    pub fn is_repeatedly_denied(&self, tool_name: &str) -> bool {
        self.denials_for_tool(tool_name) >= 3
    }

    /// Get a suggestion message if a tool is being repeatedly denied.
    #[must_use]
    pub fn suggestion_for(&self, tool_name: &str) -> Option<String> {
        if self.is_repeatedly_denied(tool_name) {
            Some(format!(
                "Tool '{tool_name}' has been denied {} times. Consider adding an allow rule in settings.",
                self.denials_for_tool(tool_name)
            ))
        } else {
            None
        }
    }

    /// Get all denials.
    #[must_use]
    pub fn denials(&self) -> &[(String, String)] {
        &self.denials
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionPolicy {
    active_mode: PermissionMode,
    tool_requirements: BTreeMap<String, PermissionMode>,
    /// Explicit permission rules (allow/deny/ask) with patterns.
    rules: Vec<PermissionRule>,
    /// Additional directories beyond cwd that tools can access.
    additional_directories: Vec<PathBuf>,
}

impl PermissionPolicy {
    #[must_use]
    pub fn new(active_mode: PermissionMode) -> Self {
        Self {
            active_mode,
            tool_requirements: BTreeMap::new(),
            rules: Vec::new(),
            additional_directories: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_tool_requirement(
        mut self,
        tool_name: impl Into<String>,
        required_mode: PermissionMode,
    ) -> Self {
        self.tool_requirements
            .insert(tool_name.into(), required_mode);
        self
    }

    /// Add a permission rule.
    #[must_use]
    pub fn with_rule(mut self, rule: PermissionRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Add an additional directory that tools are allowed to access.
    #[must_use]
    pub fn with_additional_directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.additional_directories.push(dir.into());
        self
    }

    #[must_use]
    pub fn active_mode(&self) -> PermissionMode {
        self.active_mode
    }

    /// Get the additional directories.
    #[must_use]
    pub fn additional_directories(&self) -> &[PathBuf] {
        &self.additional_directories
    }

    /// Check if a path is within the workspace or any additional directory.
    #[must_use]
    pub fn is_path_allowed(&self, path: &Path, cwd: &Path) -> bool {
        if path.starts_with(cwd) {
            return true;
        }
        self.additional_directories
            .iter()
            .any(|dir| path.starts_with(dir))
    }

    #[must_use]
    pub fn required_mode_for(&self, tool_name: &str) -> PermissionMode {
        self.tool_requirements
            .get(tool_name)
            .copied()
            .unwrap_or(PermissionMode::DangerFullAccess)
    }

    /// Get all rules.
    #[must_use]
    pub fn rules(&self) -> &[PermissionRule] {
        &self.rules
    }

    #[must_use]
    pub fn authorize(
        &self,
        tool_name: &str,
        input: &str,
        mut prompter: Option<&mut dyn PermissionPrompter>,
    ) -> PermissionOutcome {
        // Check explicit rules first (they override mode-based logic).
        for rule in &self.rules {
            if rule.matches(tool_name, input) {
                match rule.behavior {
                    RuleBehavior::Allow => return PermissionOutcome::Allow,
                    RuleBehavior::Deny => {
                        return PermissionOutcome::Deny {
                            reason: format!(
                                "denied by {} rule for '{}'{}",
                                format_rule_source(rule.source),
                                tool_name,
                                rule.content_pattern
                                    .as_deref()
                                    .map(|p| format!(" (pattern: {p})"))
                                    .unwrap_or_default()
                            ),
                        };
                    }
                    RuleBehavior::Ask => {
                        // Fall through to prompter logic below.
                    }
                }
            }
        }

        let current_mode = self.active_mode();
        let required_mode = self.required_mode_for(tool_name);
        if current_mode == PermissionMode::Allow || current_mode >= required_mode {
            return PermissionOutcome::Allow;
        }

        let request = PermissionRequest {
            tool_name: tool_name.to_string(),
            input: input.to_string(),
            current_mode,
            required_mode,
        };

        if current_mode == PermissionMode::Prompt
            || (current_mode == PermissionMode::WorkspaceWrite
                && required_mode == PermissionMode::DangerFullAccess)
        {
            return match prompter.as_mut() {
                Some(prompter) => match prompter.decide(&request) {
                    PermissionPromptDecision::Allow => PermissionOutcome::Allow,
                    PermissionPromptDecision::Deny { reason } => PermissionOutcome::Deny { reason },
                },
                None => PermissionOutcome::Deny {
                    reason: format!(
                        "tool '{tool_name}' requires approval to escalate from {} to {}",
                        current_mode.as_str(),
                        required_mode.as_str()
                    ),
                },
            };
        }

        PermissionOutcome::Deny {
            reason: format!(
                "tool '{tool_name}' requires {} permission; current mode is {}",
                required_mode.as_str(),
                current_mode.as_str()
            ),
        }
    }
}

fn format_rule_source(source: RuleSource) -> &'static str {
    match source {
        RuleSource::UserSettings => "user settings",
        RuleSource::ProjectSettings => "project settings",
        RuleSource::LocalSettings => "local settings",
        RuleSource::CliArg => "CLI argument",
        RuleSource::PolicySettings => "policy",
        RuleSource::Session => "session",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DenialTracker, PermissionMode, PermissionOutcome, PermissionPolicy,
        PermissionPromptDecision, PermissionPrompter, PermissionRequest, PermissionRule,
        RuleBehavior, RuleSource,
    };
    use std::path::PathBuf;

    struct RecordingPrompter {
        seen: Vec<PermissionRequest>,
        allow: bool,
    }

    impl PermissionPrompter for RecordingPrompter {
        fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
            self.seen.push(request.clone());
            if self.allow {
                PermissionPromptDecision::Allow
            } else {
                PermissionPromptDecision::Deny {
                    reason: "not now".to_string(),
                }
            }
        }
    }

    #[test]
    fn allows_tools_when_active_mode_meets_requirement() {
        let policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite)
            .with_tool_requirement("read_file", PermissionMode::ReadOnly)
            .with_tool_requirement("write_file", PermissionMode::WorkspaceWrite);

        assert_eq!(
            policy.authorize("read_file", "{}", None),
            PermissionOutcome::Allow
        );
        assert_eq!(
            policy.authorize("write_file", "{}", None),
            PermissionOutcome::Allow
        );
    }

    #[test]
    fn denies_read_only_escalations_without_prompt() {
        let policy = PermissionPolicy::new(PermissionMode::ReadOnly)
            .with_tool_requirement("write_file", PermissionMode::WorkspaceWrite)
            .with_tool_requirement("bash", PermissionMode::DangerFullAccess);

        assert!(matches!(
            policy.authorize("write_file", "{}", None),
            PermissionOutcome::Deny { reason } if reason.contains("requires workspace-write permission")
        ));
        assert!(matches!(
            policy.authorize("bash", "{}", None),
            PermissionOutcome::Deny { reason } if reason.contains("requires danger-full-access permission")
        ));
    }

    #[test]
    fn prompts_for_workspace_write_to_danger_full_access_escalation() {
        let policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite)
            .with_tool_requirement("bash", PermissionMode::DangerFullAccess);
        let mut prompter = RecordingPrompter {
            seen: Vec::new(),
            allow: true,
        };

        let outcome = policy.authorize("bash", "echo hi", Some(&mut prompter));

        assert_eq!(outcome, PermissionOutcome::Allow);
        assert_eq!(prompter.seen.len(), 1);
        assert_eq!(prompter.seen[0].tool_name, "bash");
        assert_eq!(
            prompter.seen[0].current_mode,
            PermissionMode::WorkspaceWrite
        );
        assert_eq!(
            prompter.seen[0].required_mode,
            PermissionMode::DangerFullAccess
        );
    }

    #[test]
    fn honors_prompt_rejection_reason() {
        let policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite)
            .with_tool_requirement("bash", PermissionMode::DangerFullAccess);
        let mut prompter = RecordingPrompter {
            seen: Vec::new(),
            allow: false,
        };

        assert!(matches!(
            policy.authorize("bash", "echo hi", Some(&mut prompter)),
            PermissionOutcome::Deny { reason } if reason == "not now"
        ));
    }

    // ── Permission rule tests ──────────────────────────────────────────

    #[test]
    fn allow_rule_overrides_mode_restriction() {
        let policy = PermissionPolicy::new(PermissionMode::ReadOnly)
            .with_tool_requirement("bash", PermissionMode::DangerFullAccess)
            .with_rule(PermissionRule {
                tool_name: "bash".to_string(),
                content_pattern: Some("git *".to_string()),
                behavior: RuleBehavior::Allow,
                source: RuleSource::ProjectSettings,
            });

        assert_eq!(
            policy.authorize("bash", r#"{"command":"git status"}"#, None),
            PermissionOutcome::Allow
        );
    }

    #[test]
    fn deny_rule_blocks_even_with_full_access() {
        let policy = PermissionPolicy::new(PermissionMode::DangerFullAccess)
            .with_rule(PermissionRule {
                tool_name: "bash".to_string(),
                content_pattern: Some("rm -rf".to_string()),
                behavior: RuleBehavior::Deny,
                source: RuleSource::PolicySettings,
            });

        assert!(matches!(
            policy.authorize("bash", r#"{"command":"rm -rf /"}"#, None),
            PermissionOutcome::Deny { reason } if reason.contains("policy")
        ));
    }

    #[test]
    fn deny_rule_without_pattern_blocks_all_calls() {
        let policy = PermissionPolicy::new(PermissionMode::DangerFullAccess)
            .with_rule(PermissionRule {
                tool_name: "REPL".to_string(),
                content_pattern: None,
                behavior: RuleBehavior::Deny,
                source: RuleSource::UserSettings,
            });

        assert!(matches!(
            policy.authorize("REPL", "{}", None),
            PermissionOutcome::Deny { .. }
        ));
    }

    #[test]
    fn unmatched_rule_falls_through() {
        let policy = PermissionPolicy::new(PermissionMode::DangerFullAccess)
            .with_rule(PermissionRule {
                tool_name: "bash".to_string(),
                content_pattern: Some("rm".to_string()),
                behavior: RuleBehavior::Deny,
                source: RuleSource::ProjectSettings,
            });

        // "ls" doesn't match the "rm" pattern, so no rule fires → falls through to allow.
        assert_eq!(
            policy.authorize("bash", r#"{"command":"ls"}"#, None),
            PermissionOutcome::Allow
        );
    }

    #[test]
    fn additional_directory_allows_path() {
        let policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite)
            .with_additional_directory("/home/user/shared");

        let cwd = PathBuf::from("/home/user/project");
        assert!(policy.is_path_allowed(&PathBuf::from("/home/user/project/src/main.rs"), &cwd));
        assert!(policy.is_path_allowed(&PathBuf::from("/home/user/shared/data.json"), &cwd));
        assert!(!policy.is_path_allowed(&PathBuf::from("/etc/passwd"), &cwd));
    }

    // ── Denial tracker tests ───────────────────────────────────────────

    #[test]
    fn denial_tracker_counts_per_tool() {
        let mut tracker = DenialTracker::new();
        tracker.record("bash", "not allowed");
        tracker.record("bash", "still not allowed");
        tracker.record("write_file", "blocked");

        assert_eq!(tracker.total_denials(), 3);
        assert_eq!(tracker.denials_for_tool("bash"), 2);
        assert_eq!(tracker.denials_for_tool("write_file"), 1);
        assert!(!tracker.is_repeatedly_denied("bash"));
    }

    #[test]
    fn denial_tracker_detects_repeated_denials() {
        let mut tracker = DenialTracker::new();
        for _ in 0..3 {
            tracker.record("bash", "denied");
        }
        assert!(tracker.is_repeatedly_denied("bash"));
        assert!(tracker.suggestion_for("bash").is_some());
        assert!(tracker.suggestion_for("read_file").is_none());
    }
}
