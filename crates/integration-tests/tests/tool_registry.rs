//! Integration tests for the tool-registry seam.
//!
//! These exercise `tools::GlobalToolRegistry` end-to-end across crate
//! boundaries: it pulls built-in specs from the `tools` crate, accepts
//! `plugins::PluginTool` values, surfaces `api::ToolDefinition`s, and reports
//! `runtime::PermissionMode`s. A single registry therefore stitches together
//! the `tools`, `plugins`, `runtime`, and `api` crates — a seam no per-crate
//! unit test can reach.

use std::collections::BTreeSet;

use plugins::{PluginTool, PluginToolDefinition, PluginToolPermission};
use runtime::PermissionMode;
use serde_json::json;
use tempfile::tempdir;

/// Build a self-contained `PluginTool` whose backing command is a portable
/// shell snippet, so the test never depends on anything outside the process.
fn echo_plugin_tool(name: &str) -> PluginTool {
    let definition = PluginToolDefinition {
        name: name.to_string(),
        description: Some("integration-test echo tool".to_string()),
        input_schema: json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
            "additionalProperties": false
        }),
    };

    // `sh -c` reads the JSON payload from the EMBER_TOOL_INPUT env var that
    // `PluginTool::execute` sets, and echoes the `value` field back.
    PluginTool::new(
        "integration.echo",
        "Integration Echo",
        definition,
        "sh",
        vec![
            "-c".to_string(),
            "printf '%s' \"$EMBER_TOOL_INPUT\"".to_string(),
        ],
        PluginToolPermission::ReadOnly,
        None,
    )
}

#[test]
fn builtin_registry_executes_read_file_through_public_api() {
    // Seam: tools::GlobalToolRegistry -> tools::execute_tool -> runtime::read_file.
    let dir = tempdir().expect("temp dir");
    let file_path = dir.path().join("hello.txt");
    std::fs::write(&file_path, "integration-foundation\n").expect("write fixture");

    let registry = tools::GlobalToolRegistry::builtin();
    let output = registry
        .execute(
            "read_file",
            &json!({ "path": file_path.to_str().expect("utf8 path") }),
        )
        .expect("read_file should succeed");

    assert!(
        output.contains("integration-foundation"),
        "read_file output should contain file contents, got: {output}"
    );
}

#[test]
fn registry_with_plugin_tool_dispatches_and_reports_permissions() {
    // Seam: a plugins::PluginTool is registered into the tools registry, then
    // surfaced through api::ToolDefinition and dispatched via the registry.
    let registry =
        tools::GlobalToolRegistry::with_plugin_tools(vec![echo_plugin_tool("echo_back")])
            .expect("plugin tool registration should succeed");

    // The plugin tool appears in the api::ToolDefinition surface alongside builtins.
    let definitions = registry.definitions(None);
    assert!(
        definitions.iter().any(|def| def.name == "echo_back"),
        "plugin tool should be present in tool definitions"
    );
    assert!(
        definitions.iter().any(|def| def.name == "read_file"),
        "built-in tools should still be present alongside the plugin tool"
    );

    // Its permission is mapped from the plugin permission into runtime::PermissionMode.
    let perms = registry.permission_specs(None);
    let echo_perm = perms
        .iter()
        .find(|(name, _)| name == "echo_back")
        .map(|(_, mode)| *mode)
        .expect("plugin tool should have a permission spec");
    assert_eq!(echo_perm, PermissionMode::ReadOnly);

    // Dispatching by name reaches the plugin tool and returns its output.
    let output = registry
        .execute("echo_back", &json!({ "value": "round-trip" }))
        .expect("plugin tool dispatch should succeed");
    assert!(
        output.contains("round-trip"),
        "plugin tool should echo its JSON input, got: {output}"
    );
}

#[test]
fn registry_rejects_plugin_tool_colliding_with_builtin() {
    // Seam: the registry guards the builtin/plugin namespace boundary. A plugin
    // that reuses a built-in name (`read_file`) must be rejected, not silently
    // shadow the built-in dispatch path.
    let result = tools::GlobalToolRegistry::with_plugin_tools(vec![echo_plugin_tool("read_file")]);
    let err = result.expect_err("plugin tool colliding with a builtin must be rejected");
    assert!(
        err.contains("read_file"),
        "error should name the conflicting tool, got: {err}"
    );
}

#[test]
fn normalize_allowed_tools_resolves_aliases_and_filters_definitions() {
    // Seam: allow-list normalization (aliases like `read` -> `read_file`) feeds
    // back into the definition surface, gating which api::ToolDefinitions are
    // exposed. This is the path the CLI/server use to honor `--allowedTools`.
    let registry = tools::GlobalToolRegistry::builtin();

    let allowed = registry
        .normalize_allowed_tools(&["read, edit".to_string(), "Bash".to_string()])
        .expect("normalization should succeed")
        .expect("non-empty input yields an allow-list");

    let expected: BTreeSet<String> = ["read_file", "edit_file", "bash"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(allowed, expected);

    let definitions = registry.definitions(Some(&allowed));
    let names: BTreeSet<String> = definitions.into_iter().map(|def| def.name).collect();
    assert_eq!(names, expected);

    // An unknown tool name surfaces a descriptive error rather than silently dropping.
    let err = registry
        .normalize_allowed_tools(&["definitely_not_a_tool".to_string()])
        .expect_err("unknown tool should error");
    assert!(err.contains("definitely_not_a_tool"));
}
