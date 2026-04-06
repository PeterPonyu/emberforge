use std::collections::BTreeMap;
use std::fs;
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

// ── Rule parsing from string format ───────────────────────────────────

/// Parse a rule string like `Bash(npm install)` → (tool_name, content_pattern).
///
/// Supported formats:
/// - `"Bash"` → tool_name=Bash, content_pattern=None
/// - `"Bash(*)"` → tool_name=Bash, content_pattern=None (wildcard = all)
/// - `"Bash()"` → tool_name=Bash, content_pattern=None (empty = all)
/// - `"Bash(npm install)"` → tool_name=Bash, content_pattern=Some("npm install")
/// - `"Bash(git push --force)"` → tool_name=Bash, content_pattern=Some("git push --force")
pub fn parse_rule_value(input: &str) -> (String, Option<String>) {
    let trimmed = input.trim();

    // Find unescaped opening paren
    let open = trimmed.find('(');
    let close = trimmed.rfind(')');

    match (open, close) {
        (Some(o), Some(c)) if c > o && c == trimmed.len() - 1 => {
            let tool_name = trimmed[..o].trim().to_string();
            let content = trimmed[o + 1..c].trim();
            let pattern = if content.is_empty() || content == "*" {
                None
            } else {
                // Unescape special chars: \( → (, \) → )
                let unescaped = content.replace("\\(", "(").replace("\\)", ")");
                Some(unescaped)
            };
            (normalize_tool_name_for_rule(&tool_name), pattern)
        }
        _ => (normalize_tool_name_for_rule(trimmed), None),
    }
}

/// Format a rule back to string: `Bash(npm install)`.
pub fn format_rule_value(tool_name: &str, content_pattern: Option<&str>) -> String {
    match content_pattern {
        Some(pattern) => {
            let escaped = pattern.replace('(', "\\(").replace(')', "\\)");
            format!("{tool_name}({escaped})")
        }
        None => tool_name.to_string(),
    }
}

/// Normalize legacy tool name aliases to canonical names.
fn normalize_tool_name_for_rule(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "bash" | "shell" | "sh" => "bash".to_string(),
        "fileread" | "file_read" | "read_file" | "read" => "read_file".to_string(),
        "filewrite" | "file_write" | "write_file" | "write" => "write_file".to_string(),
        "fileedit" | "file_edit" | "edit_file" | "edit" => "edit_file".to_string(),
        "glob" | "glob_search" => "glob_search".to_string(),
        "grep" | "grep_search" => "grep_search".to_string(),
        _ => name.to_string(),
    }
}

/// Parse multiple rules from a settings list (e.g., `permissions.allow` array).
pub fn parse_rules_from_settings(
    entries: &[String],
    behavior: RuleBehavior,
    source: RuleSource,
) -> Vec<PermissionRule> {
    entries
        .iter()
        .map(|entry| {
            let (tool_name, content_pattern) = parse_rule_value(entry);
            PermissionRule {
                tool_name,
                content_pattern,
                behavior,
                source,
            }
        })
        .collect()
}

// ── Filesystem sandbox ───────────────────────────────────────────────

/// Sensitive paths that should never be written to by tools.
const SENSITIVE_PATHS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".gpg",
    ".aws/credentials",
    ".azure",
    ".config/gcloud",
    ".kube/config",
    ".docker/config.json",
    ".npmrc",
    ".pypirc",
    ".netrc",
    ".git-credentials",
];

/// Sensitive absolute paths (system-wide).
const SENSITIVE_ABSOLUTE_PATHS: &[&str] = &[
    "/etc/shadow",
    "/etc/passwd",
    "/etc/sudoers",
    "/etc/ssl/private",
    "/root",
];

/// Internal paths that are always writable (within workspace).
const INTERNAL_WRITABLE_PREFIXES: &[&str] = &[
    ".ember/",
    ".ember-agents/",
    ".claw/",
    ".claw-agents/",
];

/// Check if a path is within the workspace, resolving symlinks to prevent escapes.
///
/// Returns `true` if the path resolves to a location within `workspace_root`
/// or any additional allowed directory.
pub fn is_path_within_workspace(path: &Path, workspace_root: &Path) -> bool {
    // Resolve symlinks for both paths
    let resolved_root = fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let resolved_path = resolve_path_safely(path, &resolved_root);

    resolved_path.starts_with(&resolved_root)
}

/// Check if a path targets a sensitive location (credentials, keys, etc.).
pub fn is_sensitive_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Check home-relative sensitive paths
    if let Some(home) = home_dir() {
        for sensitive in SENSITIVE_PATHS {
            let full = home.join(sensitive);
            if path.starts_with(&full) {
                return true;
            }
        }
    }

    // Check absolute sensitive paths
    for sensitive in SENSITIVE_ABSOLUTE_PATHS {
        if path.starts_with(sensitive) {
            return true;
        }
    }

    // Check for common sensitive file patterns
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if filename == ".env"
        || filename.starts_with(".env.")
        || filename == "credentials.json"
        || filename == "secrets.json"
        || filename == "id_rsa"
        || filename == "id_ed25519"
        || filename.ends_with(".pem")
        || filename.ends_with(".key")
    {
        return true;
    }

    // Check for path traversal attempts
    if path_str.contains("../") || path_str.contains("..\\") {
        // After resolution, this would be caught by is_path_within_workspace,
        // but flag it as sensitive for the warning message
        return true;
    }

    false
}

/// Check if a path is an internal writable path (always allowed within workspace).
pub fn is_internal_writable_path(path: &Path, workspace_root: &Path) -> bool {
    let relative = path.strip_prefix(workspace_root).unwrap_or(path);
    let rel_str = relative.to_string_lossy();
    INTERNAL_WRITABLE_PREFIXES
        .iter()
        .any(|prefix| rel_str.starts_with(prefix))
}

/// Tool-specific permission check result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPermissionResult {
    /// Tool is allowed to proceed.
    Allow,
    /// Tool should be denied.
    Deny { reason: String },
    /// Tool needs user confirmation.
    Ask { reason: String },
    /// No tool-specific opinion — fall through to mode-based check.
    Passthrough,
}

/// Check tool-specific permissions based on tool type and input.
///
/// This is the equivalent of CC's per-tool `checkPermissions()` method.
pub fn check_tool_permissions(
    tool_name: &str,
    input: &str,
    workspace_root: &Path,
) -> ToolPermissionResult {
    match tool_name {
        "bash" => check_bash_permissions(input, workspace_root),
        "write_file" | "edit_file" => check_file_write_permissions(input, workspace_root),
        "read_file" => check_file_read_permissions(input, workspace_root),
        _ => ToolPermissionResult::Passthrough,
    }
}

fn check_bash_permissions(input: &str, _workspace_root: &Path) -> ToolPermissionResult {
    // Extract command from JSON input
    let command = extract_json_field(input, "command").unwrap_or_default();
    if command.is_empty() {
        return ToolPermissionResult::Passthrough;
    }

    // Use the existing bash_security module for hard deny checks
    let cwd = std::env::current_dir().unwrap_or_default();
    match crate::validate_bash_command(&command, &cwd, &crate::PermissionMode::WorkspaceWrite) {
        crate::SecurityVerdict::Deny { reason, .. } => {
            ToolPermissionResult::Deny { reason }
        }
        crate::SecurityVerdict::Warn { reason, .. } => {
            ToolPermissionResult::Ask { reason }
        }
        crate::SecurityVerdict::Allow => ToolPermissionResult::Passthrough,
    }
}

fn check_file_write_permissions(input: &str, workspace_root: &Path) -> ToolPermissionResult {
    let path_str = extract_json_field(input, "path")
        .or_else(|| extract_json_field(input, "file_path"))
        .unwrap_or_default();
    if path_str.is_empty() {
        return ToolPermissionResult::Passthrough;
    }

    let path = PathBuf::from(&path_str);

    // Internal writable paths are always allowed
    if is_internal_writable_path(&path, workspace_root) {
        return ToolPermissionResult::Allow;
    }

    // Sensitive paths are always denied for writes
    if is_sensitive_path(&path) {
        return ToolPermissionResult::Deny {
            reason: format!("Write to sensitive path denied: {path_str}"),
        };
    }

    // Check workspace containment (with symlink resolution)
    if !is_path_within_workspace(&path, workspace_root) {
        return ToolPermissionResult::Ask {
            reason: format!("Write to path outside workspace: {path_str}"),
        };
    }

    ToolPermissionResult::Passthrough
}

fn check_file_read_permissions(input: &str, workspace_root: &Path) -> ToolPermissionResult {
    let path_str = extract_json_field(input, "path")
        .or_else(|| extract_json_field(input, "file_path"))
        .unwrap_or_default();
    if path_str.is_empty() {
        return ToolPermissionResult::Passthrough;
    }

    let path = PathBuf::from(&path_str);

    // Reading within workspace is always allowed
    if is_path_within_workspace(&path, workspace_root) {
        return ToolPermissionResult::Allow;
    }

    // Reading sensitive paths should ask
    if is_sensitive_path(&path) {
        return ToolPermissionResult::Ask {
            reason: format!("Read of sensitive path: {path_str}"),
        };
    }

    // Reading outside workspace: passthrough to mode-based check
    ToolPermissionResult::Passthrough
}

/// Resolve a path safely, handling symlinks and relative components.
fn resolve_path_safely(path: &Path, workspace_root: &Path) -> PathBuf {
    // Try canonical resolution first
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }

    // If the file doesn't exist yet, resolve the parent
    if let Some(parent) = path.parent() {
        if let Ok(canonical_parent) = fs::canonicalize(parent) {
            if let Some(filename) = path.file_name() {
                return canonical_parent.join(filename);
            }
        }
    }

    // Last resort: join with workspace root if relative
    if path.is_relative() {
        workspace_root.join(path)
    } else {
        path.to_path_buf()
    }
}

/// Extract a field from a JSON string (simple, no full parser dependency).
fn extract_json_field<'a>(json: &'a str, field: &str) -> Option<String> {
    let pattern = format!("\"{field}\"");
    let idx = json.find(&pattern)?;
    let after_key = &json[idx + pattern.len()..];
    // Skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let value_start = after_colon.trim_start();
    if value_start.starts_with('"') {
        // String value: find closing quote (handle escaped quotes)
        let inner = &value_start[1..];
        let mut end = 0;
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return Some(inner[..end].replace("\\\"", "\"").replace("\\\\", "\\"));
            }
            end += ch.len_utf8();
        }
        None
    } else {
        // Non-string value (number, bool, etc.)
        let end = value_start.find(|c: char| c == ',' || c == '}' || c == ']')?;
        Some(value_start[..end].trim().to_string())
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
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

    /// Full authorization pipeline (CC-equivalent):
    /// 1. Check deny rules → immediate deny
    /// 2. Check ask rules → route to prompter
    /// 3. Run tool-specific permission checks (bash security, file sandbox)
    /// 4. Check mode-based permissions
    /// 5. If mode allows, return allow
    /// 6. If prompter available, ask user
    /// 7. Otherwise deny
    #[must_use]
    pub fn authorize(
        &self,
        tool_name: &str,
        input: &str,
        mut prompter: Option<&mut dyn PermissionPrompter>,
    ) -> PermissionOutcome {
        // ── Step 1: Check deny rules first (highest priority) ──
        for rule in self.rules.iter().filter(|r| r.behavior == RuleBehavior::Deny) {
            if rule.matches(tool_name, input) {
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
        }

        // ── Step 2: Check ask rules ──
        for rule in self.rules.iter().filter(|r| r.behavior == RuleBehavior::Ask) {
            if rule.matches(tool_name, input) {
                let request = PermissionRequest {
                    tool_name: tool_name.to_string(),
                    input: input.to_string(),
                    current_mode: self.active_mode,
                    required_mode: self.required_mode_for(tool_name),
                };
                return match prompter.as_mut() {
                    Some(prompter) => match prompter.decide(&request) {
                        PermissionPromptDecision::Allow => PermissionOutcome::Allow,
                        PermissionPromptDecision::Deny { reason } => {
                            PermissionOutcome::Deny { reason }
                        }
                    },
                    None => PermissionOutcome::Deny {
                        reason: format!(
                            "tool '{tool_name}' requires confirmation (ask rule from {})",
                            format_rule_source(rule.source)
                        ),
                    },
                };
            }
        }

        // ── Step 3: Check allow rules ──
        for rule in self.rules.iter().filter(|r| r.behavior == RuleBehavior::Allow) {
            if rule.matches(tool_name, input) {
                return PermissionOutcome::Allow;
            }
        }

        // ── Step 4: Tool-specific permission checks ──
        let cwd = std::env::current_dir().unwrap_or_default();
        match check_tool_permissions(tool_name, input, &cwd) {
            ToolPermissionResult::Deny { reason } => {
                return PermissionOutcome::Deny { reason };
            }
            ToolPermissionResult::Ask { reason } => {
                // Safety checks are bypass-immune — always ask even in DangerFullAccess
                let request = PermissionRequest {
                    tool_name: tool_name.to_string(),
                    input: input.to_string(),
                    current_mode: self.active_mode,
                    required_mode: self.required_mode_for(tool_name),
                };
                return match prompter.as_mut() {
                    Some(prompter) => match prompter.decide(&request) {
                        PermissionPromptDecision::Allow => PermissionOutcome::Allow,
                        PermissionPromptDecision::Deny { reason: user_reason } => {
                            PermissionOutcome::Deny { reason: user_reason }
                        }
                    },
                    None => PermissionOutcome::Deny { reason },
                };
            }
            ToolPermissionResult::Allow => return PermissionOutcome::Allow,
            ToolPermissionResult::Passthrough => {} // Continue to mode-based check
        }

        // ── Step 5: Mode-based permission check ──
        let current_mode = self.active_mode();
        let required_mode = self.required_mode_for(tool_name);
        if current_mode == PermissionMode::Allow || current_mode >= required_mode {
            return PermissionOutcome::Allow;
        }

        // ── Step 6: Prompt user for escalation ──
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

        // ── Step 7: Default deny ──
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

    // ── Rule parsing tests ────────────────────────────────────────────

    #[test]
    fn parse_rule_value_tool_only() {
        let (name, pattern) = super::parse_rule_value("Bash");
        assert_eq!(name, "bash");
        assert_eq!(pattern, None);
    }

    #[test]
    fn parse_rule_value_with_content() {
        let (name, pattern) = super::parse_rule_value("Bash(npm install)");
        assert_eq!(name, "bash");
        assert_eq!(pattern.as_deref(), Some("npm install"));
    }

    #[test]
    fn parse_rule_value_wildcard() {
        let (name, pattern) = super::parse_rule_value("Bash(*)");
        assert_eq!(name, "bash");
        assert_eq!(pattern, None);
    }

    #[test]
    fn parse_rule_value_empty_parens() {
        let (name, pattern) = super::parse_rule_value("Bash()");
        assert_eq!(name, "bash");
        assert_eq!(pattern, None);
    }

    #[test]
    fn parse_rule_value_escaped_parens() {
        let (name, pattern) = super::parse_rule_value(r#"Bash(python -c "print\(1\)")"#);
        assert_eq!(name, "bash");
        assert!(pattern.is_some());
        assert!(pattern.unwrap().contains("print(1)"));
    }

    #[test]
    fn format_rule_value_roundtrip() {
        let formatted = super::format_rule_value("bash", Some("git push --force"));
        assert_eq!(formatted, "bash(git push --force)");
        let (name, pattern) = super::parse_rule_value(&formatted);
        assert_eq!(name, "bash");
        assert_eq!(pattern.as_deref(), Some("git push --force"));
    }

    #[test]
    fn parse_rules_from_settings_creates_rules() {
        let entries = vec![
            "Bash(npm install)".to_string(),
            "write_file".to_string(),
        ];
        let rules = super::parse_rules_from_settings(&entries, RuleBehavior::Allow, RuleSource::ProjectSettings);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].tool_name, "bash");
        assert_eq!(rules[0].content_pattern.as_deref(), Some("npm install"));
        assert_eq!(rules[1].tool_name, "write_file");
        assert_eq!(rules[1].content_pattern, None);
    }

    #[test]
    fn normalize_tool_name_aliases() {
        assert_eq!(super::normalize_tool_name_for_rule("Shell"), "bash");
        assert_eq!(super::normalize_tool_name_for_rule("FileRead"), "read_file");
        assert_eq!(super::normalize_tool_name_for_rule("edit"), "edit_file");
    }

    // ── Filesystem sandbox tests ──────────────────────────────────────

    #[test]
    fn sensitive_path_detection() {
        assert!(super::is_sensitive_path(&PathBuf::from("/home/user/.ssh/id_rsa")));
        assert!(super::is_sensitive_path(&PathBuf::from("/etc/shadow")));
        assert!(super::is_sensitive_path(&PathBuf::from("/home/user/project/.env")));
        assert!(super::is_sensitive_path(&PathBuf::from("/tmp/../etc/passwd")));
        assert!(!super::is_sensitive_path(&PathBuf::from("/home/user/project/src/main.rs")));
    }

    #[test]
    fn internal_writable_paths() {
        let root = PathBuf::from("/home/user/project");
        assert!(super::is_internal_writable_path(
            &PathBuf::from("/home/user/project/.ember/settings.json"),
            &root
        ));
        assert!(super::is_internal_writable_path(
            &PathBuf::from("/home/user/project/.ember-agents/task.json"),
            &root
        ));
        assert!(!super::is_internal_writable_path(
            &PathBuf::from("/home/user/project/src/main.rs"),
            &root
        ));
    }

    // ── Tool-specific permission tests ────────────────────────────────

    #[test]
    fn file_write_denies_sensitive_paths() {
        let cwd = PathBuf::from("/tmp");
        let result = super::check_file_write_permissions(
            r#"{"path": "/home/user/.ssh/id_rsa"}"#,
            &cwd,
        );
        assert!(matches!(result, super::ToolPermissionResult::Deny { .. }));
    }

    #[test]
    fn file_write_allows_internal_paths() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        let internal = cwd.join(".ember/settings.json");
        let input = format!(r#"{{"path": "{}"}}"#, internal.display());
        let result = super::check_file_write_permissions(&input, &cwd);
        assert!(matches!(result, super::ToolPermissionResult::Allow));
    }

    #[test]
    fn file_read_allows_workspace_paths() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        let file = cwd.join("src/main.rs");
        let input = format!(r#"{{"path": "{}"}}"#, file.display());
        let result = super::check_file_read_permissions(&input, &cwd);
        assert!(matches!(result, super::ToolPermissionResult::Allow));
    }

    #[test]
    fn json_field_extraction() {
        let json = r#"{"command": "ls -la", "timeout": 30}"#;
        assert_eq!(super::extract_json_field(json, "command").as_deref(), Some("ls -la"));
        assert_eq!(super::extract_json_field(json, "timeout").as_deref(), Some("30"));
        assert_eq!(super::extract_json_field(json, "missing"), None);
    }

    // ── Pipeline integration test ─────────────────────────────────────

    #[test]
    fn deny_rule_takes_priority_over_allow_rule() {
        let policy = PermissionPolicy::new(PermissionMode::DangerFullAccess)
            .with_rule(PermissionRule {
                tool_name: "bash".to_string(),
                content_pattern: Some("rm -rf".to_string()),
                behavior: RuleBehavior::Deny,
                source: RuleSource::PolicySettings,
            })
            .with_rule(PermissionRule {
                tool_name: "bash".to_string(),
                content_pattern: None,
                behavior: RuleBehavior::Allow,
                source: RuleSource::UserSettings,
            });

        // Deny rule should win even though allow rule also matches
        assert!(matches!(
            policy.authorize("bash", r#"{"command":"rm -rf /"}"#, None),
            PermissionOutcome::Deny { .. }
        ));

        // Non-matching deny rule: allow rule should kick in
        assert_eq!(
            policy.authorize("bash", r#"{"command":"ls"}"#, None),
            PermissionOutcome::Allow
        );
    }

    #[test]
    fn content_specific_allow_overrides_mode_denial() {
        let policy = PermissionPolicy::new(PermissionMode::ReadOnly)
            .with_tool_requirement("bash", PermissionMode::DangerFullAccess)
            .with_rule(PermissionRule {
                tool_name: "bash".to_string(),
                content_pattern: Some("cargo test".to_string()),
                behavior: RuleBehavior::Allow,
                source: RuleSource::ProjectSettings,
            });

        // Specific allow rule overrides mode restriction
        assert_eq!(
            policy.authorize("bash", r#"{"command":"cargo test"}"#, None),
            PermissionOutcome::Allow
        );

        // Non-matching: mode restriction applies
        assert!(matches!(
            policy.authorize("bash", r#"{"command":"cargo publish"}"#, None),
            PermissionOutcome::Deny { .. }
        ));
    }
}
