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
//!
//! The hook command strings are spelled for the shell `plugins::hooks`
//! actually invokes: `sh -lc` on Unix, `cmd /C` on Windows. The protocol and
//! assertions are identical across platforms; only the inline command syntax
//! differs (`printf`/`;`/`$VAR` versus `echo`/`&`/`%VAR%`). Each helper returns
//! the right spelling so the seam is exercised on every CI runner.

use plugins::{HookRunner, PluginHooks};

/// A hook that prints `message` to stdout and exits with `code`.
fn print_and_exit(message: &str, code: u8) -> String {
    #[cfg(windows)]
    {
        // `echo` keeps everything up to the `&`; the trailing space before
        // `&` is trimmed by the hook runner, leaving exactly `message`.
        format!("echo {message}& exit {code}")
    }
    #[cfg(not(windows))]
    {
        format!("printf '{message}'; exit {code}")
    }
}

/// A hook that echoes the `HOOK_TOOL_OUTPUT` env var prefixed with `saw:`.
fn echo_tool_output() -> String {
    #[cfg(windows)]
    {
        "echo saw:%HOOK_TOOL_OUTPUT%& exit 0".to_string()
    }
    #[cfg(not(windows))]
    {
        "printf 'saw:%s' \"$HOOK_TOOL_OUTPUT\"; exit 0".to_string()
    }
}

#[test]
fn pre_tool_use_allows_and_propagates_hook_message() {
    let runner = HookRunner::new(PluginHooks {
        pre_tool_use: vec![print_and_exit("pre-hook ran", 0)],
        post_tool_use: Vec::new(),
    });

    let result = runner.run_pre_tool_use("read_file", r#"{"path":"README.md"}"#);

    assert!(!result.is_denied(), "exit 0 must allow the tool");
    assert_eq!(result.messages(), &["pre-hook ran".to_string()]);
}

#[test]
fn pre_tool_use_denies_on_exit_two() {
    let runner = HookRunner::new(PluginHooks {
        pre_tool_use: vec![print_and_exit("blocked: dangerous bash", 2)],
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
            print_and_exit("first denies", 2),
            print_and_exit("second should not run", 0),
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
        post_tool_use: vec![echo_tool_output()],
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
