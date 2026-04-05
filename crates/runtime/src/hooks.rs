use std::ffi::OsStr;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    SessionStart,
    SessionEnd,
    SubagentStart,
    SubagentStop,
    CompactStart,
    CompactEnd,
    ToolError,
    PermissionDenied,
    ConfigChange,
    UserPromptSubmit,
    Notification,
    PluginLoad,
    PluginUnload,
    CwdChanged,
    FileChanged,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::CompactStart => "CompactStart",
            Self::CompactEnd => "CompactEnd",
            Self::ToolError => "ToolError",
            Self::PermissionDenied => "PermissionDenied",
            Self::ConfigChange => "ConfigChange",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::Notification => "Notification",
            Self::PluginLoad => "PluginLoad",
            Self::PluginUnload => "PluginUnload",
            Self::CwdChanged => "CwdChanged",
            Self::FileChanged => "FileChanged",
        }
    }

    /// Whether this event fires for tool-related hooks (has tool_name context).
    #[must_use]
    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            Self::PreToolUse | Self::PostToolUse | Self::ToolError
        )
    }
}

// ── Hook definition with match rules and execution backend ─────────────

/// Execution backend for a hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HookBackend {
    /// Execute a shell command.
    Command {
        /// The shell command to run.
        run: String,
    },
    /// POST a webhook.
    Http {
        /// The URL to POST to.
        url: String,
        /// Optional custom headers.
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
    },
}

/// Match rule to filter which tool calls trigger a hook.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HookMatchRule {
    /// Only trigger for these tool names. Empty = match all.
    #[serde(default)]
    pub tool_names: Vec<String>,
    /// Only trigger when the command matches these patterns (for bash tool).
    #[serde(default)]
    pub commands: Vec<String>,
}

impl HookMatchRule {
    /// Check if this rule matches the given tool name and input.
    #[must_use]
    pub fn matches(&self, tool_name: &str, tool_input: &str) -> bool {
        // If tool_names is specified, tool must match.
        if !self.tool_names.is_empty()
            && !self.tool_names.iter().any(|name| name == tool_name)
        {
            return false;
        }
        // If commands patterns are specified, input must match one.
        if !self.commands.is_empty() {
            let input_lower = tool_input.to_ascii_lowercase();
            if !self.commands.iter().any(|pattern| {
                let p = pattern.to_ascii_lowercase();
                if p.ends_with('*') {
                    input_lower.contains(&p[..p.len() - 1])
                } else {
                    input_lower.contains(&p)
                }
            }) {
                return false;
            }
        }
        true
    }
}

/// A structured hook definition (for settings.json style configuration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookDefinition {
    /// Which event triggers this hook.
    pub event: HookEvent,
    /// Execution backend.
    #[serde(flatten)]
    pub backend: HookBackend,
    /// Optional match rule (only for tool events).
    #[serde(default, rename = "match")]
    pub match_rule: Option<HookMatchRule>,
    /// Timeout in seconds (default: 30).
    #[serde(default = "default_hook_timeout")]
    pub timeout_secs: u32,
    /// Whether to run asynchronously (non-blocking).
    #[serde(default)]
    pub r#async: bool,
    /// Custom status message during execution.
    #[serde(default)]
    pub status_message: Option<String>,
    /// Fire only once, then auto-remove.
    #[serde(default)]
    pub once: bool,
}

fn default_hook_timeout() -> u32 {
    30
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunResult {
    denied: bool,
    messages: Vec<String>,
}

impl HookRunResult {
    #[must_use]
    pub fn allow(messages: Vec<String>) -> Self {
        Self {
            denied: false,
            messages,
        }
    }

    #[must_use]
    pub fn is_denied(&self) -> bool {
        self.denied
    }

    #[must_use]
    pub fn messages(&self) -> &[String] {
        &self.messages
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookRunner {
    config: RuntimeHookConfig,
}

#[derive(Debug, Clone, Copy)]
struct HookCommandRequest<'a> {
    event: HookEvent,
    tool_name: &'a str,
    tool_input: &'a str,
    tool_output: Option<&'a str>,
    is_error: bool,
    payload: &'a str,
}

impl HookRunner {
    #[must_use]
    pub fn new(config: RuntimeHookConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn from_feature_config(feature_config: &RuntimeFeatureConfig) -> Self {
        Self::new(feature_config.hooks().clone())
    }

    #[must_use]
    pub fn run_pre_tool_use(&self, tool_name: &str, tool_input: &str) -> HookRunResult {
        self.run_commands(
            HookEvent::PreToolUse,
            self.config.pre_tool_use(),
            tool_name,
            tool_input,
            None,
            false,
        )
    }

    #[must_use]
    pub fn run_post_tool_use(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
    ) -> HookRunResult {
        self.run_commands(
            HookEvent::PostToolUse,
            self.config.post_tool_use(),
            tool_name,
            tool_input,
            Some(tool_output),
            is_error,
        )
    }

    // ── Lifecycle event dispatchers ────────────────────────────────────

    /// Fire a lifecycle event (no tool context).
    pub fn fire_event(&self, event: HookEvent) {
        self.fire_event_with_context(event, "", "");
    }

    /// Fire a lifecycle event with arbitrary context.
    pub fn fire_event_with_context(
        &self,
        event: HookEvent,
        context_key: &str,
        context_value: &str,
    ) {
        let commands = self.config.commands_for_event(event);
        if commands.is_empty() {
            return;
        }
        // Fire-and-forget for lifecycle events: we don't block on the result.
        let _ = self.run_commands(event, &commands, context_key, context_value, None, false);
    }

    /// Execute an HTTP hook by POSTing the payload to the given URL.
    fn run_http_hook(
        url: &str,
        headers: &std::collections::BTreeMap<String, String>,
        payload: &str,
        timeout: Duration,
    ) -> HookCommandOutcome {
        // Use a simple blocking HTTP POST via std::process::Command (curl).
        let mut args = vec![
            "-s".to_string(),
            "-X".to_string(),
            "POST".to_string(),
            "-H".to_string(),
            "Content-Type: application/json".to_string(),
        ];
        for (key, value) in headers {
            args.push("-H".to_string());
            args.push(format!("{key}: {value}"));
        }
        args.push("--max-time".to_string());
        args.push(timeout.as_secs().to_string());
        args.push("-d".to_string());
        args.push(payload.to_string());
        args.push(url.to_string());

        match Command::new("curl").args(&args).output() {
            Ok(output) if output.status.success() => {
                let body = String::from_utf8_lossy(&output.stdout).trim().to_string();
                HookCommandOutcome::Allow {
                    message: (!body.is_empty()).then_some(body),
                }
            }
            Ok(output) => HookCommandOutcome::Warn {
                message: format!(
                    "HTTP hook to {url} returned status {}",
                    output.status.code().unwrap_or(-1)
                ),
            },
            Err(e) => HookCommandOutcome::Warn {
                message: format!("HTTP hook to {url} failed: {e}"),
            },
        }
    }

    fn run_commands(
        &self,
        event: HookEvent,
        commands: &[String],
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        is_error: bool,
    ) -> HookRunResult {
        if commands.is_empty() {
            return HookRunResult::allow(Vec::new());
        }

        let payload = json!({
            "hook_event_name": event.as_str(),
            "tool_name": tool_name,
            "tool_input": parse_tool_input(tool_input),
            "tool_input_json": tool_input,
            "tool_output": tool_output,
            "tool_result_is_error": is_error,
        })
        .to_string();

        let mut messages = Vec::new();

        for command in commands {
            match Self::run_command(
                command,
                HookCommandRequest {
                    event,
                    tool_name,
                    tool_input,
                    tool_output,
                    is_error,
                    payload: &payload,
                },
            ) {
                HookCommandOutcome::Allow { message } => {
                    if let Some(message) = message {
                        messages.push(message);
                    }
                }
                HookCommandOutcome::Deny { message } => {
                    let message = message.unwrap_or_else(|| {
                        format!("{} hook denied tool `{tool_name}`", event.as_str())
                    });
                    messages.push(message);
                    return HookRunResult {
                        denied: true,
                        messages,
                    };
                }
                HookCommandOutcome::Warn { message } => messages.push(message),
            }
        }

        HookRunResult::allow(messages)
    }

    fn run_command(command: &str, request: HookCommandRequest<'_>) -> HookCommandOutcome {
        let mut child = shell_command(command);
        child.stdin(std::process::Stdio::piped());
        child.stdout(std::process::Stdio::piped());
        child.stderr(std::process::Stdio::piped());
        child.env("HOOK_EVENT", request.event.as_str());
        child.env("HOOK_TOOL_NAME", request.tool_name);
        child.env("HOOK_TOOL_INPUT", request.tool_input);
        child.env(
            "HOOK_TOOL_IS_ERROR",
            if request.is_error { "1" } else { "0" },
        );
        if let Some(tool_output) = request.tool_output {
            child.env("HOOK_TOOL_OUTPUT", tool_output);
        }

        match child.output_with_stdin(request.payload.as_bytes()) {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let message = (!stdout.is_empty()).then_some(stdout);
                match output.status.code() {
                    Some(0) => HookCommandOutcome::Allow { message },
                    Some(2) => HookCommandOutcome::Deny { message },
                    Some(code) => HookCommandOutcome::Warn {
                        message: format_hook_warning(
                            command,
                            code,
                            message.as_deref(),
                            stderr.as_str(),
                        ),
                    },
                    None => HookCommandOutcome::Warn {
                        message: format!(
                            "{} hook `{command}` terminated by signal while handling `{}`",
                            request.event.as_str(),
                            request.tool_name
                        ),
                    },
                }
            }
            Err(error) => HookCommandOutcome::Warn {
                message: format!(
                    "{} hook `{command}` failed to start for `{}`: {error}",
                    request.event.as_str(),
                    request.tool_name
                ),
            },
        }
    }
}

enum HookCommandOutcome {
    Allow { message: Option<String> },
    Deny { message: Option<String> },
    Warn { message: String },
}

fn parse_tool_input(tool_input: &str) -> serde_json::Value {
    serde_json::from_str(tool_input).unwrap_or_else(|_| json!({ "raw": tool_input }))
}

fn format_hook_warning(command: &str, code: i32, stdout: Option<&str>, stderr: &str) -> String {
    let mut message =
        format!("Hook `{command}` exited with status {code}; allowing tool execution to continue");
    if let Some(stdout) = stdout.filter(|stdout| !stdout.is_empty()) {
        message.push_str(": ");
        message.push_str(stdout);
    } else if !stderr.is_empty() {
        message.push_str(": ");
        message.push_str(stderr);
    }
    message
}

fn shell_command(command: &str) -> CommandWithStdin {
    #[cfg(windows)]
    let mut command_builder = {
        let mut command_builder = Command::new("cmd");
        command_builder.arg("/C").arg(command);
        CommandWithStdin::new(command_builder)
    };

    #[cfg(not(windows))]
    let command_builder = {
        let mut command_builder = Command::new("sh");
        command_builder.arg("-lc").arg(command);
        CommandWithStdin::new(command_builder)
    };

    command_builder
}

struct CommandWithStdin {
    command: Command,
}

impl CommandWithStdin {
    fn new(command: Command) -> Self {
        Self { command }
    }

    fn stdin(&mut self, cfg: std::process::Stdio) -> &mut Self {
        self.command.stdin(cfg);
        self
    }

    fn stdout(&mut self, cfg: std::process::Stdio) -> &mut Self {
        self.command.stdout(cfg);
        self
    }

    fn stderr(&mut self, cfg: std::process::Stdio) -> &mut Self {
        self.command.stderr(cfg);
        self
    }

    fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.command.env(key, value);
        self
    }

    fn output_with_stdin(&mut self, stdin: &[u8]) -> std::io::Result<std::process::Output> {
        let mut child = self.command.spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() {
            use std::io::Write;
            child_stdin.write_all(stdin)?;
        }
        child.wait_with_output()
    }
}

#[cfg(test)]
mod tests {
    use super::{HookBackend, HookDefinition, HookEvent, HookMatchRule, HookRunResult, HookRunner};
    use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};

    #[test]
    fn allows_exit_code_zero_and_captures_stdout() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("printf 'pre ok'")],
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        assert_eq!(result, HookRunResult::allow(vec!["pre ok".to_string()]));
    }

    #[test]
    fn denies_exit_code_two() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("printf 'blocked by hook'; exit 2")],
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);

        assert!(result.is_denied());
        assert_eq!(result.messages(), &["blocked by hook".to_string()]);
    }

    #[test]
    fn warns_for_other_non_zero_statuses() {
        let runner = HookRunner::from_feature_config(&RuntimeFeatureConfig::default().with_hooks(
            RuntimeHookConfig::new(
                vec![shell_snippet("printf 'warning hook'; exit 1")],
                Vec::new(),
            ),
        ));

        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        assert!(!result.is_denied());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("allowing tool execution to continue")));
    }

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String {
        script.replace('\'', "\"")
    }

    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String {
        script.to_string()
    }

    // ── Match rule tests ───────────────────────────────────────────────

    #[test]
    fn match_rule_empty_matches_everything() {
        let rule = HookMatchRule::default();
        assert!(rule.matches("bash", r#"{"command":"ls"}"#));
        assert!(rule.matches("read_file", r#"{"path":"foo.rs"}"#));
    }

    #[test]
    fn match_rule_tool_names_filter() {
        let rule = HookMatchRule {
            tool_names: vec!["bash".to_string(), "REPL".to_string()],
            commands: Vec::new(),
        };
        assert!(rule.matches("bash", "{}"));
        assert!(rule.matches("REPL", "{}"));
        assert!(!rule.matches("read_file", "{}"));
    }

    #[test]
    fn match_rule_commands_pattern() {
        let rule = HookMatchRule {
            tool_names: vec!["bash".to_string()],
            commands: vec!["rm *".to_string(), "git push*".to_string()],
        };
        assert!(rule.matches("bash", r#"{"command":"rm -rf /tmp"}"#));
        assert!(rule.matches("bash", r#"{"command":"git push --force"}"#));
        assert!(!rule.matches("bash", r#"{"command":"ls -la"}"#));
    }

    #[test]
    fn match_rule_commands_without_tool_filter() {
        let rule = HookMatchRule {
            tool_names: Vec::new(),
            commands: vec!["rm".to_string()],
        };
        // Matches any tool as long as input contains "rm"
        assert!(rule.matches("bash", r#"{"command":"rm foo"}"#));
        assert!(rule.matches("other_tool", r#"rm something"#));
    }

    // ── Hook definition serialization ──────────────────────────────────

    #[test]
    fn hook_definition_roundtrip() {
        let def = HookDefinition {
            event: HookEvent::PreToolUse,
            backend: HookBackend::Command {
                run: "echo hello".to_string(),
            },
            match_rule: Some(HookMatchRule {
                tool_names: vec!["bash".to_string()],
                commands: vec!["rm*".to_string()],
            }),
            timeout_secs: 10,
            r#async: false,
            status_message: Some("Checking...".to_string()),
            once: false,
        };
        let json = serde_json::to_string(&def).expect("serialize");
        let parsed: HookDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.event, HookEvent::PreToolUse);
        assert_eq!(parsed.timeout_secs, 10);
    }

    #[test]
    fn http_backend_serializes() {
        let backend = HookBackend::Http {
            url: "https://example.com/hook".to_string(),
            headers: std::collections::BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer tok".to_string(),
            )]),
        };
        let json = serde_json::to_string(&backend).expect("serialize");
        assert!(json.contains("https://example.com/hook"));
        assert!(json.contains("Authorization"));
    }

    // ── Lifecycle event dispatch ───────────────────────────────────────

    #[test]
    fn fire_event_runs_registered_commands() {
        let mut config = RuntimeHookConfig::new(Vec::new(), Vec::new());
        config.set_event_commands(
            HookEvent::SessionStart,
            vec![shell_snippet("printf 'session started'")],
        );
        let runner = HookRunner::new(config);
        // fire_event should not panic even without capturing output
        runner.fire_event(HookEvent::SessionStart);
    }

    #[test]
    fn fire_event_noop_when_no_commands_registered() {
        let runner = HookRunner::new(RuntimeHookConfig::default());
        runner.fire_event(HookEvent::SessionEnd); // should not panic
    }

    #[test]
    fn event_commands_merge_correctly() {
        let mut a = RuntimeHookConfig::new(
            vec!["pre_a".to_string()],
            Vec::new(),
        );
        a.set_event_commands(
            HookEvent::SessionStart,
            vec!["start_a".to_string()],
        );

        let mut b = RuntimeHookConfig::new(
            vec!["pre_b".to_string()],
            Vec::new(),
        );
        b.set_event_commands(
            HookEvent::SessionStart,
            vec!["start_b".to_string()],
        );

        let merged = a.merged(&b);
        assert_eq!(merged.pre_tool_use().len(), 2);
        let session_cmds = merged.commands_for_event(HookEvent::SessionStart);
        assert_eq!(session_cmds.len(), 2);
        assert!(session_cmds.contains(&"start_a".to_string()));
        assert!(session_cmds.contains(&"start_b".to_string()));
    }

    #[test]
    fn hook_event_is_tool_event() {
        assert!(HookEvent::PreToolUse.is_tool_event());
        assert!(HookEvent::PostToolUse.is_tool_event());
        assert!(HookEvent::ToolError.is_tool_event());
        assert!(!HookEvent::SessionStart.is_tool_event());
        assert!(!HookEvent::CwdChanged.is_tool_event());
    }
}
