use std::env;
use std::path::PathBuf;

use runtime::{execute_bash, glob_search, grep_search, read_file, write_file, edit_file};
use runtime::{BashCommandInput, GrepSearchInput};
use runtime::{validate_bash_command, SecurityVerdict, PermissionMode};
use serde::Deserialize;
use serde_json::Value;

use crate::error::ToolExecError;
use crate::types::*;
use crate::implementations::*;

pub fn execute_tool(name: &str, input: &Value) -> Result<String, ToolExecError> {
    match name {
        "bash" => from_value::<BashCommandInput>(input).and_then(run_bash),
        "read_file" => from_value::<ReadFileInput>(input).and_then(run_read_file),
        "write_file" => from_value::<WriteFileInput>(input).and_then(run_write_file),
        "edit_file" => from_value::<EditFileInput>(input).and_then(run_edit_file),
        "glob_search" => from_value::<GlobSearchInputValue>(input).and_then(run_glob_search),
        "grep_search" => from_value::<GrepSearchInput>(input).and_then(run_grep_search),
        "WebFetch" => from_value::<WebFetchInput>(input).and_then(run_web_fetch),
        "WebSearch" => from_value::<WebSearchInput>(input).and_then(run_web_search),
        "TodoWrite" => from_value::<TodoWriteInput>(input).and_then(run_todo_write),
        "Skill" => from_value::<SkillInput>(input).and_then(run_skill),
        "Agent" => from_value::<AgentInput>(input).and_then(run_agent),
        "ToolSearch" => from_value::<ToolSearchInput>(input).and_then(run_tool_search),
        "NotebookEdit" => from_value::<NotebookEditInput>(input).and_then(run_notebook_edit),
        "Sleep" => from_value::<SleepInput>(input).and_then(run_sleep),
        "SendUserMessage" | "Brief" => from_value::<BriefInput>(input).and_then(run_brief),
        "Config" => from_value::<ConfigInput>(input).and_then(run_config),
        "StructuredOutput" => {
            from_value::<StructuredOutputInput>(input).and_then(run_structured_output)
        }
        "REPL" => from_value::<ReplInput>(input).and_then(run_repl),
        "PowerShell" => from_value::<PowerShellInput>(input).and_then(run_powershell),
        // ── New tools for TS parity ──
        "AskUserQuestion" => from_value::<AskUserQuestionInput>(input).and_then(run_ask_user_question),
        "EnterPlanMode" => from_value::<EnterPlanModeInput>(input).and_then(run_enter_plan_mode),
        "ExitPlanMode" => from_value::<ExitPlanModeInput>(input).and_then(run_exit_plan_mode),
        "MCPTool" => from_value::<McpToolInput>(input).and_then(run_mcp_tool),
        "LSPTool" => from_value::<LspToolInput>(input).and_then(run_lsp_tool),
        "ListMcpResources" => from_value::<ListMcpResourcesInput>(input).and_then(run_list_mcp_resources),
        "ReadMcpResource" => from_value::<ReadMcpResourceInput>(input).and_then(run_read_mcp_resource),
        // ── Phase 1: Cron & Worktree tools ──
        "CronCreate" => from_value::<CronCreateInput>(input).and_then(run_cron_create),
        "CronDelete" => from_value::<CronDeleteInput>(input).and_then(run_cron_delete),
        "CronList" => from_value::<CronListInput>(input).and_then(run_cron_list),
        "EnterWorktree" => from_value::<EnterWorktreeInput>(input).and_then(run_enter_worktree),
        "ExitWorktree" => from_value::<ExitWorktreeInput>(input).and_then(run_exit_worktree),
        // ── Phase 2: Task management & messaging ──
        "TaskCreate" => from_value::<TaskCreateInput>(input).and_then(run_task_create),
        "TaskUpdate" => from_value::<TaskUpdateInput>(input).and_then(run_task_update),
        "TaskGet" => from_value::<TaskGetInput>(input).and_then(run_task_get),
        "TaskList" => from_value::<TaskListInput>(input).and_then(run_task_list),
        "TaskStop" => from_value::<TaskStopInput>(input).and_then(run_task_stop),
        "TaskOutput" => from_value::<TaskOutputInput>(input).and_then(run_task_output),
        "SendMessage" => from_value::<SendMessageInput>(input).and_then(run_send_message),
        // ── Phase 3: Team orchestration ──
        "TeamCreate" => from_value::<TeamCreateInput>(input).and_then(run_team_create),
        "TeamDelete" => from_value::<TeamDeleteInput>(input).and_then(run_team_delete),
        _ => Err(ToolExecError::UnsupportedTool(name.to_string())),
    }
}

pub(crate) fn from_value<T: for<'de> Deserialize<'de>>(input: &Value) -> Result<T, ToolExecError> {
    serde_json::from_value(input.clone()).map_err(ToolExecError::Deserialize)
}

pub(crate) fn run_bash(input: BashCommandInput) -> Result<String, ToolExecError> {
    // Security validation before execution.
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let permission_mode = if input.dangerously_disable_sandbox.unwrap_or(false) {
        PermissionMode::DangerFullAccess
    } else {
        PermissionMode::WorkspaceWrite
    };
    match validate_bash_command(&input.command, &cwd, &permission_mode) {
        SecurityVerdict::Allow => {}
        SecurityVerdict::Deny { reason, check_id } => {
            return Err(ToolExecError::Runtime(format!(
                "Command blocked by security check #{check_id}: {reason}"
            )));
        }
        SecurityVerdict::Warn { reason, check_id } => {
            eprintln!("⚠ Security warning (check #{check_id}): {reason}");
        }
    }

    let output = execute_bash(input).map_err(|e| ToolExecError::Runtime(e.to_string()))?;
    Ok(serde_json::to_string_pretty(&output)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_read_file(input: ReadFileInput) -> Result<String, ToolExecError> {
    to_pretty_json(read_file(&input.path, input.offset, input.limit)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_write_file(input: WriteFileInput) -> Result<String, ToolExecError> {
    to_pretty_json(write_file(&input.path, &input.content)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_edit_file(input: EditFileInput) -> Result<String, ToolExecError> {
    to_pretty_json(edit_file(
        &input.path,
        &input.old_string,
        &input.new_string,
        input.replace_all.unwrap_or(false),
    )?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_glob_search(input: GlobSearchInputValue) -> Result<String, ToolExecError> {
    to_pretty_json(glob_search(&input.pattern, input.path.as_deref())?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_grep_search(input: GrepSearchInput) -> Result<String, ToolExecError> {
    to_pretty_json(grep_search(&input)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_web_fetch(input: WebFetchInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_web_fetch(&input)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_web_search(input: WebSearchInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_web_search(&input)?)
}

pub(crate) fn run_todo_write(input: TodoWriteInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_todo_write(input)?)
}

pub(crate) fn run_skill(input: SkillInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_skill(input)?)
}

pub(crate) fn run_agent(input: AgentInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_agent(input)?)
}

pub(crate) fn run_tool_search(input: ToolSearchInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_tool_search(input))
}

pub(crate) fn run_notebook_edit(input: NotebookEditInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_notebook_edit(input)?)
}

pub(crate) fn run_sleep(input: SleepInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_sleep(input))
}

pub(crate) fn run_brief(input: BriefInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_brief(input)?)
}

pub(crate) fn run_config(input: ConfigInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_config(input)?)
}

pub(crate) fn run_structured_output(input: StructuredOutputInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_structured_output(input))
}

pub(crate) fn run_repl(input: ReplInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_repl(input)?)
}

pub(crate) fn run_powershell(input: PowerShellInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_powershell(input)?)
}

pub(crate) fn to_pretty_json<T: serde::Serialize>(value: T) -> Result<String, ToolExecError> {
    serde_json::to_string_pretty(&value).map_err(ToolExecError::Serialize)
}

// ── New tool run wrappers for TS parity ──────────────────────────

pub(crate) fn run_ask_user_question(input: AskUserQuestionInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_ask_user_question(input)?)
}

pub(crate) fn run_enter_plan_mode(input: EnterPlanModeInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_enter_plan_mode(input)?)
}

pub(crate) fn run_exit_plan_mode(input: ExitPlanModeInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_exit_plan_mode(input)?)
}

pub(crate) fn run_mcp_tool(input: McpToolInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_mcp_tool(input)?)
}

pub(crate) fn run_lsp_tool(input: LspToolInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_lsp_tool(input)?)
}

pub(crate) fn run_list_mcp_resources(input: ListMcpResourcesInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_list_mcp_resources(input)?)
}

pub(crate) fn run_read_mcp_resource(input: ReadMcpResourceInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_read_mcp_resource(input)?)
}

// ── Phase 1: Cron & Worktree run wrappers ──────────────────────

pub(crate) fn run_cron_create(input: CronCreateInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_cron_create(input)?)
}

pub(crate) fn run_cron_delete(input: CronDeleteInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_cron_delete(input)?)
}

pub(crate) fn run_cron_list(input: CronListInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_cron_list(input)?)
}

pub(crate) fn run_enter_worktree(input: EnterWorktreeInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_enter_worktree(input)?)
}

pub(crate) fn run_exit_worktree(input: ExitWorktreeInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_exit_worktree(input)?)
}

// ── Phase 2: Task management & messaging run wrappers ──────────

pub(crate) fn run_task_create(input: TaskCreateInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_task_create(input)?)
}

pub(crate) fn run_task_update(input: TaskUpdateInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_task_update(input)?)
}

pub(crate) fn run_task_get(input: TaskGetInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_task_get(input)?)
}

pub(crate) fn run_task_list(input: TaskListInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_task_list(input)?)
}

pub(crate) fn run_task_stop(input: TaskStopInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_task_stop(input)?)
}

pub(crate) fn run_task_output(input: TaskOutputInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_task_output(input)?)
}

pub(crate) fn run_send_message(input: SendMessageInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_send_message(input)?)
}

// ── Phase 3: Team orchestration run wrappers ────────────────────

pub(crate) fn run_team_create(input: TeamCreateInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_team_create(input)?)
}

pub(crate) fn run_team_delete(input: TeamDeleteInput) -> Result<String, ToolExecError> {
    to_pretty_json(execute_team_delete(input)?)
}
