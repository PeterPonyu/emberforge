#![allow(clippy::expect_used)]

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, ConversationRuntime, MessageRole,
    PermissionMode, PermissionPolicy, RuntimeError, RuntimeFeatureConfig, RuntimeHookConfig,
    Session, ToolError, ToolExecutor,
};
use serde_json::json;
use tools::GlobalToolRegistry;

struct ScriptedApiClient {
    responses: VecDeque<Vec<AssistantEvent>>,
}

impl ScriptedApiClient {
    fn new(responses: Vec<Vec<AssistantEvent>>) -> Self {
        Self {
            responses: VecDeque::from(responses),
        }
    }
}

impl ApiClient for ScriptedApiClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if request.messages.len() > 1 {
            let last = request
                .messages
                .last()
                .expect("tool result should be sent back before the follow-up request");
            assert_eq!(last.role, MessageRole::Tool);
        }
        self.responses
            .pop_front()
            .ok_or_else(|| RuntimeError::new("unexpected extra API call"))
    }
}

struct RegistryToolExecutor {
    registry: GlobalToolRegistry,
}

impl RegistryToolExecutor {
    fn builtin() -> Self {
        Self {
            registry: GlobalToolRegistry::builtin(),
        }
    }
}

impl ToolExecutor for RegistryToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        let input = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input: {error}")))?;
        self.registry
            .execute(tool_name, &input)
            .map_err(|error| ToolError::new(error.to_string()))
    }
}

#[test]
fn cross_crate_runtime_integration_covers_tool_hooks_and_permissions() {
    let original_cwd = std::env::current_dir().expect("current dir should be readable");
    let workspace = unique_temp_workspace();
    fs::create_dir_all(&workspace).expect("temp workspace should be creatable");
    std::env::set_current_dir(&workspace).expect("temp workspace should become cwd");

    let result = std::panic::catch_unwind(|| {
        writes_file_through_conversation_runtime_and_tool_registry(&workspace);
        pre_tool_use_hook_blocks_real_tool_execution(&workspace);
        read_only_permission_mode_denies_workspace_write_tool(&workspace);
    });

    std::env::set_current_dir(original_cwd).expect("original cwd should be restorable");
    fs::remove_dir_all(&workspace).ok();

    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn writes_file_through_conversation_runtime_and_tool_registry(workspace: &Path) {
    let path = workspace.join("tool-roundtrip.txt");
    let input = json!({
        "path": path.display().to_string(),
        "content": "created by the integration test\n",
    })
    .to_string();
    let mut runtime = runtime_with(
        ScriptedApiClient::new(vec![
            tool_request("tool-1", "write_file", &input),
            final_text(),
        ]),
        PermissionMode::WorkspaceWrite,
        &RuntimeFeatureConfig::default(),
    );

    let summary = runtime
        .run_turn("create the file", None)
        .expect("conversation should execute the write_file tool");

    assert_eq!(summary.iterations, 2);
    assert_eq!(summary.tool_results.len(), 1);
    assert_eq!(
        fs::read_to_string(path).expect("tool should create file"),
        "created by the integration test\n"
    );
    assert_tool_result(&summary.tool_results[0], "write_file", false, "create");
}

fn pre_tool_use_hook_blocks_real_tool_execution(workspace: &Path) {
    let path = workspace.join("hook-blocked.txt");
    let input = json!({
        "path": path.display().to_string(),
        "content": "this must not be written\n",
    })
    .to_string();
    let hooks = RuntimeHookConfig::new(
        vec!["echo blocked by integration hook && exit 2".to_string()],
        vec![],
    );
    let mut runtime = runtime_with(
        ScriptedApiClient::new(vec![
            tool_request("tool-2", "write_file", &input),
            final_text(),
        ]),
        PermissionMode::WorkspaceWrite,
        &RuntimeFeatureConfig::default().with_hooks(hooks),
    );

    let summary = runtime
        .run_turn("try a blocked write", None)
        .expect("hook denial should be represented as a tool result, not a runtime error");

    assert!(
        !path.exists(),
        "pre-tool hook must stop the write_file implementation"
    );
    assert_tool_result(
        &summary.tool_results[0],
        "write_file",
        true,
        "blocked by integration hook",
    );
}

fn read_only_permission_mode_denies_workspace_write_tool(workspace: &Path) {
    let path = workspace.join("read-only-denied.txt");
    let input = json!({
        "path": path.display().to_string(),
        "content": "this must not be written\n",
    })
    .to_string();
    let mut runtime = runtime_with(
        ScriptedApiClient::new(vec![
            tool_request("tool-3", "write_file", &input),
            final_text(),
        ]),
        PermissionMode::ReadOnly,
        &RuntimeFeatureConfig::default(),
    );

    let summary = runtime
        .run_turn("try a read-only write", None)
        .expect("permission denial should be represented as a tool result");

    assert!(!path.exists(), "read-only mode must stop workspace writes");
    assert_tool_result(
        &summary.tool_results[0],
        "write_file",
        true,
        "requires workspace-write permission",
    );
}

fn runtime_with(
    api_client: ScriptedApiClient,
    permission_mode: PermissionMode,
    feature_config: &RuntimeFeatureConfig,
) -> ConversationRuntime<ScriptedApiClient, RegistryToolExecutor> {
    ConversationRuntime::new_with_features(
        Session::new(),
        api_client,
        RegistryToolExecutor::builtin(),
        policy_from_builtin_tool_specs(permission_mode),
        vec!["integration test system prompt".to_string()],
        feature_config,
    )
    .with_max_iterations(2)
}

fn policy_from_builtin_tool_specs(mode: PermissionMode) -> PermissionPolicy {
    GlobalToolRegistry::builtin()
        .permission_specs(None)
        .into_iter()
        .fold(
            PermissionPolicy::new(mode),
            |policy, (name, required_mode)| policy.with_tool_requirement(name, required_mode),
        )
}

fn tool_request(id: &str, name: &str, input: &str) -> Vec<AssistantEvent> {
    vec![
        AssistantEvent::TextDelta("I will use a tool.".to_string()),
        AssistantEvent::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input: input.to_string(),
        },
        AssistantEvent::MessageStop,
    ]
}

fn final_text() -> Vec<AssistantEvent> {
    vec![
        AssistantEvent::TextDelta("done".to_string()),
        AssistantEvent::MessageStop,
    ]
}

fn assert_tool_result(
    message: &runtime::ConversationMessage,
    tool_name: &str,
    is_error: bool,
    contains: &str,
) {
    let Some(ContentBlock::ToolResult {
        tool_name: actual_tool,
        output,
        is_error: actual_error,
        ..
    }) = message.blocks.first()
    else {
        panic!("expected a tool result block");
    };

    assert_eq!(actual_tool, tool_name);
    assert_eq!(*actual_error, is_error);
    assert!(
        output.contains(contains),
        "tool output should contain {contains:?}, got {output:?}"
    );
}

fn unique_temp_workspace() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "emberforge-cross-crate-runtime-{}-{nanos}",
        std::process::id()
    ))
}
