//! End-to-end integration test for the full tool-execution pipeline (issue #21).
//!
//! Unit tests inside each crate cover one slice of the pipeline while mocking
//! the surrounding stages. None of them exercise the whole chain — registration
//! -> permission check -> `PreToolUse` hook -> tool invocation -> result piping
//! -> `PostToolUse` hook -> conversation update — through the *real* components of
//! every crate at once. A regression that lands between two of those stages
//! (e.g. between hook return and conversation injection) slips through the
//! per-crate unit grid silently. This test stitches the stages together using
//! only public APIs:
//!
//! - `runtime::ConversationRuntime` drives the conversation loop.
//! - A stub `runtime::ApiClient` returns exactly one tool-use response, then a
//!   terminal text response.
//! - A real `tools::GlobalToolRegistry` (holding a real `plugins::PluginTool`)
//!   backs the `runtime::ToolExecutor`, so the tool actually runs as the
//!   registry would run it in production.
//! - A real `runtime::PermissionPolicy` gates the call (mode-based check).
//! - Real inline `PreToolUse` / `PostToolUse` hook subprocesses fire through the
//!   runtime's `HookRunner`.
//!
//! Because hooks run as subprocesses, the only way to observe *in-process* that
//! they fired with the right data is to have them record what they saw to the
//! filesystem; the test then reads those records back. The tool invocation count
//! is observed directly via a shared counter inside the `ToolExecutor` adapter.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use plugins::{PluginTool, PluginToolDefinition, PluginToolPermission};
use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, ConversationRuntime, MessageRole,
    PermissionMode, PermissionPolicy, RuntimeError, RuntimeFeatureConfig, RuntimeHookConfig,
    Session, ToolError, ToolExecutor,
};
use serde_json::{json, Value};
use tempfile::tempdir;

/// The single tool the stub provider asks for. It is a *custom* (plugin) tool,
/// not a builtin, so the permission gate exercised is the clean mode-based check
/// (Step 5 of `PermissionPolicy::authorize`) rather than builtin path heuristics.
const TOOL_NAME: &str = "echo_tool";

/// Build a real `plugins::PluginTool` whose backing command echoes the JSON
/// payload that the registry hands it. Mirrors the foundation's
/// `tool_registry.rs` echo tool, but requires `WorkspaceWrite` so a non-trivial
/// permission requirement sits on the asserted path.
fn echo_plugin_tool() -> PluginTool {
    let definition = PluginToolDefinition {
        name: TOOL_NAME.to_string(),
        description: Some("integration-test echo tool".to_string()),
        input_schema: json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
            "additionalProperties": false
        }),
    };

    let (command, args) = echo_command();

    PluginTool::new(
        "integration.echo_pipeline",
        "Integration Echo Pipeline",
        definition,
        command,
        args,
        PluginToolPermission::WorkspaceWrite,
        None,
    )
}

#[cfg(windows)]
fn echo_command() -> (&'static str, Vec<String>) {
    (
        "powershell.exe",
        vec![
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "[Console]::Out.Write($env:EMBER_TOOL_INPUT)".to_string(),
        ],
    )
}

#[cfg(not(windows))]
fn echo_command() -> (&'static str, Vec<String>) {
    (
        "sh",
        vec![
            "-c".to_string(),
            "printf '%s' \"$EMBER_TOOL_INPUT\"".to_string(),
        ],
    )
}

/// Adapter that drives the real `tools::GlobalToolRegistry` through the runtime's
/// `ToolExecutor` seam. The registry's `execute` takes a `serde_json::Value` and
/// returns `tools::ToolExecError`; the runtime hands the executor a `&str` and
/// expects `runtime::ToolError`. This adapter is the production-shaped bridge
/// between those two public surfaces, and counts invocations so the test can
/// assert the tool ran exactly once.
struct RegistryToolExecutor {
    registry: tools::GlobalToolRegistry,
    invocations: Arc<AtomicUsize>,
}

impl ToolExecutor for RegistryToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let value: Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        self.registry
            .execute(tool_name, &value)
            .map_err(|error| ToolError::new(error.to_string()))
    }
}

/// Stub provider: turn 1 emits one tool-use, turn 2 emits a terminal text
/// response once it sees the tool result fed back into the conversation.
struct OneToolUseThenDoneClient {
    calls: usize,
    tool_input: String,
}

impl ApiClient for OneToolUseThenDoneClient {
    fn stream(&mut self, request: ApiRequest<'_>) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.calls += 1;
        match self.calls {
            1 => Ok(vec![
                AssistantEvent::TextDelta("Let me echo that.".to_string()),
                AssistantEvent::ToolUse {
                    id: "tool-1".to_string(),
                    name: TOOL_NAME.to_string(),
                    input: self.tool_input.clone(),
                },
                AssistantEvent::MessageStop,
            ]),
            2 => {
                // The result-piping stage must have appended a Tool message
                // before the provider is asked to continue the turn.
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|message| message.role == MessageRole::Tool),
                    "tool result must be piped back into the conversation before the follow-up call"
                );
                Ok(vec![
                    AssistantEvent::TextDelta("Echoed.".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
            _ => Err(RuntimeError::new("unexpected extra API call")),
        }
    }
}

/// Build an inline `PreToolUse` hook snippet that exposes the tool context. Unix
/// records the payload to a file for the strongest assertion; Windows uses hook
/// stdout because `cmd.exe` file redirection is fragile around quoted JSON.
#[cfg(not(windows))]
fn pre_hook_snippet(record: &Path) -> String {
    format!(
        "printf 'PRE name=%s input=%s\\n' \"$HOOK_TOOL_NAME\" \"$HOOK_TOOL_INPUT\" >> '{}'; exit 0",
        record.display()
    )
}

#[cfg(windows)]
fn pre_hook_snippet(_record: &Path) -> String {
    "echo PRE name=%HOOK_TOOL_NAME%".to_string()
}

/// Build an inline `PostToolUse` hook snippet that appends the tool context it
/// observed to `record`, then exits 0 (allow).
#[cfg(not(windows))]
fn post_hook_snippet(record: &Path) -> String {
    format!(
        "printf 'POST name=%s output=%s\\n' \"$HOOK_TOOL_NAME\" \"$HOOK_TOOL_OUTPUT\" >> '{}'; exit 0",
        record.display()
    )
}

#[cfg(windows)]
fn post_hook_snippet(_record: &Path) -> String {
    "echo POST name=%HOOK_TOOL_NAME%".to_string()
}

#[cfg(not(windows))]
fn read_records(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
#[allow(clippy::too_many_lines)] // One cohesive end-to-end pipeline assertion.
fn full_tool_pipeline_drives_registration_permission_hooks_and_conversation_update() {
    // ── Stage: hook recording sink ──
    let dir = tempdir().expect("temp dir");
    let pre_record: PathBuf = dir.path().join("pre.log");
    let post_record: PathBuf = dir.path().join("post.log");

    let pre_hook = pre_hook_snippet(&pre_record);
    let post_hook = post_hook_snippet(&post_record);

    // ── Stage: registration ── a real plugin tool in the real registry.
    let registry = tools::GlobalToolRegistry::with_plugin_tools(vec![echo_plugin_tool()])
        .expect("plugin tool registration should succeed");
    assert!(
        registry
            .definitions(None)
            .iter()
            .any(|def| def.name == TOOL_NAME),
        "tool must be registered and surfaced before the turn runs"
    );

    let invocations = Arc::new(AtomicUsize::new(0));
    let tool_executor = RegistryToolExecutor {
        registry,
        invocations: Arc::clone(&invocations),
    };

    // ── Stage: permission gate ── mode-based requirement on the asserted path.
    // Active mode WorkspaceWrite satisfies the tool's WorkspaceWrite requirement,
    // so authorize() returns Allow via the real mode check (no prompter needed).
    let permission_policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite)
        .with_tool_requirement(TOOL_NAME, PermissionMode::WorkspaceWrite);

    // ── Stage: hooks ── real inline PreToolUse / PostToolUse subprocesses.
    let feature_config = RuntimeFeatureConfig::default()
        .with_hooks(RuntimeHookConfig::new(vec![pre_hook], vec![post_hook]));

    let tool_input = r#"{"value":"round-trip-payload"}"#.to_string();
    let api_client = OneToolUseThenDoneClient {
        calls: 0,
        tool_input: tool_input.clone(),
    };

    let mut runtime = ConversationRuntime::new_with_features(
        Session::new(),
        api_client,
        tool_executor,
        permission_policy,
        vec!["system".to_string()],
        &feature_config,
    );

    // ── Drive one full turn ──
    let summary = runtime
        .run_turn("please echo round-trip-payload", None)
        .expect("the full tool pipeline should complete one turn");

    // ── Assert: tool invoked exactly once ──
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "the real tool must be invoked exactly once"
    );

    // ── Assert: PreToolUse fired with the right tool name + input ──
    #[cfg(not(windows))]
    {
        let pre = read_records(&pre_record);
        assert!(
            pre.contains(&format!("name={TOOL_NAME}")),
            "PreToolUse hook should fire for the tool, got: {pre:?}"
        );
        assert!(
            pre.contains("round-trip-payload"),
            "PreToolUse hook should observe the tool input, got: {pre:?}"
        );
    }

    // ── Assert: PostToolUse fired with the tool output ──
    #[cfg(not(windows))]
    {
        let post = read_records(&post_record);
        assert!(
            post.contains(&format!("name={TOOL_NAME}")),
            "PostToolUse hook should fire for the tool, got: {post:?}"
        );
        assert!(
            post.contains("round-trip-payload"),
            "PostToolUse hook should observe the (echoed) tool output, got: {post:?}"
        );
    }

    // ── Assert: tool-result message landed in the conversation in order ──
    // Expected message sequence: [user, assistant(text+tool_use), tool_result,
    // assistant(text)]. The tool result must sit at index 2, immediately after
    // the assistant tool-use message and before the terminal assistant reply.
    let messages = &runtime.session().messages;
    assert_eq!(
        messages.len(),
        4,
        "unexpected message sequence: {messages:?}"
    );
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert!(
        matches!(messages[1].blocks[1], ContentBlock::ToolUse { .. }),
        "assistant message should contain the tool-use block"
    );

    assert_eq!(
        messages[2].role,
        MessageRole::Tool,
        "the tool result must occupy the position right after the tool-use"
    );
    let ContentBlock::ToolResult {
        tool_use_id,
        tool_name,
        output,
        is_error,
    } = &messages[2].blocks[0]
    else {
        panic!(
            "message[2] should be a tool-result block: {:?}",
            messages[2]
        );
    };
    assert_eq!(tool_use_id, "tool-1", "tool result must correlate by id");
    assert_eq!(tool_name, TOOL_NAME);
    assert!(!is_error, "successful tool run must not be an error result");
    assert!(
        output.contains("round-trip-payload"),
        "tool result output should carry the echoed payload, got: {output:?}"
    );
    #[cfg(windows)]
    {
        assert!(
            output.contains(&format!("PRE name={TOOL_NAME}")),
            "Windows PreToolUse hook feedback should be merged into output, got: {output:?}"
        );
        assert!(
            output.contains(&format!("POST name={TOOL_NAME}")),
            "Windows PostToolUse hook feedback should be merged into output, got: {output:?}"
        );
    }

    assert_eq!(messages[3].role, MessageRole::Assistant);

    // ── Assert: the summary reflects the same single-tool turn ──
    assert_eq!(summary.iterations, 2);
    assert_eq!(summary.tool_results.len(), 1);
    assert_eq!(summary.assistant_messages.len(), 2);
}
