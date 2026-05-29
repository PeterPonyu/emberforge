//! Integration tests for the hook-dispatch seam.
//!
//! These exercise `plugins::HookRunner` end-to-end: it spawns real hook
//! subprocesses for `PreToolUse` / `PostToolUse` events and turns their exit
//! codes into allow / deny / warn outcomes. This is the cross-process seam the
//! conversation loop relies on to gate tool execution, and it can only be
//! validated by actually running commands — exactly what an integration test
//! is for.
//!
//! Hook protocol (from `plugins::hooks`): exit 0 = allow (stdout becomes a
//! message), exit 2 = deny, any other code = warn-and-continue.

use plugins::{HookRunner, PluginHooks};

#[test]
fn pre_tool_use_allows_and_propagates_hook_message() {
    let runner = HookRunner::new(PluginHooks {
        pre_tool_use: vec!["printf 'pre-hook ran'; exit 0".to_string()],
        post_tool_use: Vec::new(),
    });

    let result = runner.run_pre_tool_use("read_file", r#"{"path":"README.md"}"#);

    assert!(!result.is_denied(), "exit 0 must allow the tool");
    assert_eq!(result.messages(), &["pre-hook ran".to_string()]);
}

#[test]
fn pre_tool_use_denies_on_exit_two() {
    let runner = HookRunner::new(PluginHooks {
        pre_tool_use: vec!["printf 'blocked: dangerous bash'; exit 2".to_string()],
        post_tool_use: Vec::new(),
    });

    let result = runner.run_pre_tool_use("bash", r#"{"command":"rm -rf /"}"#);

    assert!(result.is_denied(), "exit 2 must deny the tool");
    assert_eq!(result.messages(), &["blocked: dangerous bash".to_string()]);
}

#[test]
fn pre_tool_use_short_circuits_remaining_hooks_after_deny() {
    // A denying hook must stop the chain: the second hook (which would push a
    // message) should never run, so only the deny message is present.
    let runner = HookRunner::new(PluginHooks {
        pre_tool_use: vec![
            "printf 'first denies'; exit 2".to_string(),
            "printf 'second should not run'; exit 0".to_string(),
        ],
        post_tool_use: Vec::new(),
    });

    let result = runner.run_pre_tool_use("bash", r#"{"command":"ls"}"#);

    assert!(result.is_denied());
    assert_eq!(result.messages(), &["first denies".to_string()]);
}

#[test]
fn post_tool_use_forwards_tool_output_to_hook() {
    // PostToolUse hooks receive the tool output via the HOOK_TOOL_OUTPUT env
    // var; assert the hook can observe it and that a clean exit allows it.
    let runner = HookRunner::new(PluginHooks {
        pre_tool_use: Vec::new(),
        post_tool_use: vec!["printf 'saw:%s' \"$HOOK_TOOL_OUTPUT\"; exit 0".to_string()],
    });

    let result = runner.run_post_tool_use(
        "read_file",
        r#"{"path":"README.md"}"#,
        "file-contents-here",
        false,
    );

    assert!(!result.is_denied());
    assert_eq!(result.messages(), &["saw:file-contents-here".to_string()]);
}

#[test]
fn empty_hooks_allow_without_spawning_processes() {
    // The common case: no hooks configured. Must be an unconditional allow with
    // no messages and no subprocess spawned.
    let runner = HookRunner::new(PluginHooks::default());

    let pre = runner.run_pre_tool_use("read_file", "{}");
    let post = runner.run_post_tool_use("read_file", "{}", "out", false);

    assert!(!pre.is_denied());
    assert!(pre.messages().is_empty());
    assert!(!post.is_denied());
    assert!(post.messages().is_empty());
}
