use super::{strip_terminal_escape_sequences, truncate_for_summary};
use crate::task_mgmt::{shorten_session_id_for_report, shorten_task_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceTone {
    Tool,
    Success,
    Error,
    Thinking,
}

impl SurfaceTone {
    const fn color(self) -> u8 {
        match self {
            Self::Tool => 75,
            Self::Success => 70,
            Self::Error => 203,
            Self::Thinking => 141,
        }
    }
}

fn color_surface(text: &str, tone: SurfaceTone, bold: bool) -> String {
    if bold {
        format!("\x1b[1;38;5;{}m{text}\x1b[0m", tone.color())
    } else {
        format!("\x1b[38;5;{}m{text}\x1b[0m", tone.color())
    }
}

fn surface_visible_width(value: &str) -> usize {
    strip_terminal_escape_sequences(value).chars().count()
}

fn pad_ansi_for_surface(line: &str, width: usize) -> String {
    let padding = width.saturating_sub(surface_visible_width(line));
    if padding == 0 {
        return line.to_string();
    }

    let spaces = " ".repeat(padding);
    if let Some(stripped) = line.strip_suffix("\u{1b}[0m") {
        format!("{stripped}{spaces}\u{1b}[0m")
    } else {
        format!("{line}{spaces}")
    }
}

pub(crate) fn render_surface_card(title: &str, body: &str, tone: SurfaceTone) -> String {
    let body_lines = if body.trim().is_empty() {
        Vec::new()
    } else {
        body.lines().map(ToOwned::to_owned).collect::<Vec<_>>()
    };
    let title_width = title.chars().count();
    let content_width = body_lines
        .iter()
        .map(|line| surface_visible_width(line))
        .max()
        .unwrap_or(0)
        .max(title_width + 1);
    let top_fill = "─".repeat(content_width.saturating_sub(title_width + 1));
    let top = color_surface(&format!("╭─ {title} {top_fill}╮"), tone, true);
    let bottom = color_surface(&format!("╰{}╯", "─".repeat(content_width + 2)), tone, true);
    let left = color_surface("│", tone, false);
    let right = color_surface("│", tone, false);

    let mut lines = vec![top];
    for line in body_lines {
        lines.push(format!(
            "{left} {} {right}",
            pad_ansi_for_surface(&line, content_width)
        ));
    }
    lines.push(bottom);
    lines.join("\n")
}

pub(crate) fn format_concise_tool_call(name: &str, parsed: &serde_json::Value) -> Option<String> {
    match name {
        "StructuredOutput" => Some("preparing structured response".to_string()),
        "Config" => {
            let setting = parsed
                .get("setting")
                .and_then(|value| value.as_str())
                .unwrap_or("setting");
            Some(format!("config: {setting}"))
        }
        "TodoWrite" => {
            let count = parsed
                .get("todos")
                .and_then(|value| value.as_array())
                .map_or(0, Vec::len);
            Some(format!(
                "updating {count} todo item{}",
                if count == 1 { "" } else { "s" }
            ))
        }
        "Sleep" => {
            let duration = parsed
                .get("duration_ms")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            Some(format!("sleeping for {duration}ms"))
        }
        "SendUserMessage" | "Brief" => parsed
            .get("message")
            .and_then(|value| value.as_str())
            .map(|message| truncate_for_summary(message, 120)),
        "AskUserQuestion" => parsed
            .get("question")
            .and_then(|value| value.as_str())
            .map(|question| truncate_for_summary(question, 120)),
        "EnterPlanMode" => Some("switching into plan mode".to_string()),
        "ExitPlanMode" => Some("returning to execution mode".to_string()),
        _ => None,
    }
}

pub(crate) fn format_tool_call_start(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));

    let detail = match name {
        "bash" | "Bash" => format_bash_call(&parsed),
        "read_file" | "Read" => {
            let path = extract_tool_path(&parsed);
            format!("read_file: reading {path}")
        }
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            let lines = parsed
                .get("content")
                .and_then(|value| value.as_str())
                .map_or(0, |content| content.lines().count());
            format!("write_file: writing {path} ({lines} lines)")
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            let old_value = parsed
                .get("old_string")
                .or_else(|| parsed.get("oldString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let new_value = parsed
                .get("new_string")
                .or_else(|| parsed.get("newString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            format!(
                "edit_file: editing {path}{}",
                format_patch_preview(old_value, new_value)
                    .map(|preview| format!("\n{preview}"))
                    .unwrap_or_default()
            )
        }
        "glob_search" | "Glob" => format_search_start("glob_search", &parsed),
        "grep_search" | "Grep" => format_search_start("grep_search", &parsed),
        "web_search" | "WebSearch" => {
            let query = parsed.get("query").and_then(|v| v.as_str()).unwrap_or("?");
            format!("web_search: \"{query}\"")
        }
        "WebFetch" => {
            let url = parsed.get("url").and_then(|v| v.as_str()).unwrap_or("?");
            let short = truncate_for_summary(url, 60);
            format!("web_fetch: {short}")
        }
        "MCPTool" => format_mcp_tool_call(&parsed),
        "LSPTool" => format_lsp_tool_call(&parsed),
        "ListMcpResources" => {
            let server = parsed
                .get("server_name")
                .or_else(|| parsed.get("serverName"))
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("mcp resources: {server}")
        }
        "ReadMcpResource" => {
            let server = parsed
                .get("server_name")
                .or_else(|| parsed.get("serverName"))
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let uri = parsed
                .get("resource_uri")
                .or_else(|| parsed.get("resourceUri"))
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            format!("mcp resource: {server}\n\x1b[2m{uri}\x1b[0m")
        }
        // Concise display for tools that don't need verbose bodies
        "StructuredOutput" | "Config" | "TodoWrite" | "Sleep"
        | "SendUserMessage" | "Brief" | "AskUserQuestion"
        | "EnterPlanMode" | "ExitPlanMode" => {
            format_concise_tool_call(name, &parsed).unwrap_or_default()
        }
        _ => summarize_tool_payload(input),
    };

    render_surface_card(&format!("[tool] {name}"), &detail, SurfaceTone::Tool)
}

pub(crate) fn format_tool_result(name: &str, output: &str, is_error: bool) -> String {
    if is_error {
        let summary = truncate_for_summary(output.trim(), 160);
        let body = if summary.is_empty() {
            String::new()
        } else {
            format!("\x1b[38;5;203m{summary}\x1b[0m")
        };
        return render_surface_card(&format!("[err] {name}"), &body, SurfaceTone::Error);
    }

    let parsed: serde_json::Value =
        serde_json::from_str(output).unwrap_or(serde_json::Value::String(output.to_string()));
    let body = match name {
        "bash" | "Bash" => format_bash_result(&parsed),
        "read_file" | "Read" => format_read_result(&parsed),
        "write_file" | "Write" => format_write_result(&parsed),
        "edit_file" | "Edit" => format_edit_result(&parsed),
        "glob_search" | "Glob" => format_glob_result(&parsed),
        "grep_search" | "Grep" => format_grep_result(&parsed),
        // Concise display for tools that produce verbose JSON
        "StructuredOutput" => String::new(),
        "WebSearch" => {
            let query = parsed.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let n = parsed.get("results").and_then(|v| v.as_array()).map_or(0, |a| a.len());
            format!("\x1b[2m{n} results for \"{query}\"\x1b[0m")
        }
        "WebFetch" => {
            let chars = output.len();
            format!("\x1b[2m{chars} chars fetched\x1b[0m")
        }
        "MCPTool" => format_mcp_tool_result(&parsed),
        "LSPTool" => format_lsp_tool_result(&parsed),
        "ListMcpResources" => format_mcp_resources_result(&parsed),
        "ReadMcpResource" => format_read_mcp_resource_result(&parsed),
        "TodoWrite" | "Config" | "Sleep" | "SendUserMessage" | "Brief"
        | "AskUserQuestion" | "EnterPlanMode" | "ExitPlanMode" => {
            String::new()
        }
        "Agent" => format_agent_result(&parsed),
        "Skill" | "ToolSearch" | "NotebookEdit" | "REPL" | "PowerShell" => {
            // Show brief summary, not full JSON
            let summary = truncate_for_summary(output.trim(), 80);
            if summary.is_empty() {
                String::new()
            } else {
                format!("\x1b[2m{summary}\x1b[0m")
            }
        }
        _ => format_generic_tool_result(&parsed),
    };

    render_surface_card(&format!("[ok] {name}"), &body, SurfaceTone::Success)
}

const DISPLAY_TRUNCATION_NOTICE: &str =
    "\x1b[2m... output truncated for display; full result preserved in session.\x1b[0m";
const READ_DISPLAY_MAX_LINES: usize = 80;
const READ_DISPLAY_MAX_CHARS: usize = 6_000;
const READ_CONTEXT_DISPLAY_MAX_LINES: usize = 24;
const READ_CONTEXT_DISPLAY_MAX_CHARS: usize = 2_000;
const TOOL_OUTPUT_DISPLAY_MAX_LINES: usize = 60;
const TOOL_OUTPUT_DISPLAY_MAX_CHARS: usize = 4_000;

pub(crate) fn extract_tool_path(parsed: &serde_json::Value) -> String {
    parsed
        .get("file_path")
        .or_else(|| parsed.get("filePath"))
        .or_else(|| parsed.get("path"))
        .and_then(|value| value.as_str())
        .unwrap_or("?")
        .to_string()
}

fn format_search_start(label: &str, parsed: &serde_json::Value) -> String {
    let pattern = parsed
        .get("pattern")
        .and_then(|value| value.as_str())
        .unwrap_or("?");
    let scope = parsed
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or(".");
    format!("{label}: {pattern}\n\x1b[2min {scope}\x1b[0m")
}

fn format_patch_preview(old_value: &str, new_value: &str) -> Option<String> {
    if old_value.is_empty() && new_value.is_empty() {
        return None;
    }
    Some(format!(
        "\x1b[38;5;203m- {}\x1b[0m\n\x1b[38;5;70m+ {}\x1b[0m",
        truncate_for_summary(first_visible_line(old_value), 72),
        truncate_for_summary(first_visible_line(new_value), 72)
    ))
}

fn format_mcp_tool_call(parsed: &serde_json::Value) -> String {
    let server = parsed
        .get("server_name")
        .or_else(|| parsed.get("serverName"))
        .and_then(|value| value.as_str())
        .unwrap_or("?");
    let tool = parsed
        .get("tool_name")
        .or_else(|| parsed.get("toolName"))
        .and_then(|value| value.as_str())
        .unwrap_or("?");

    let mut lines = vec![format!("mcp: {server}::{tool}")];
    if let Some(arguments) = parsed.get("arguments").filter(|value| !value.is_null()) {
        let preview = summarize_tool_payload(&arguments.to_string());
        if !preview.is_empty() && preview != "{}" {
            lines.push(format!("\x1b[2margs {preview}\x1b[0m"));
        }
    }
    lines.join("\n")
}

fn format_lsp_tool_call(parsed: &serde_json::Value) -> String {
    let action = parsed
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("?");
    let file = parsed
        .get("file_path")
        .or_else(|| parsed.get("filePath"))
        .and_then(|value| value.as_str());
    let line = parsed.get("line").and_then(serde_json::Value::as_u64);
    let character = parsed
        .get("character")
        .and_then(serde_json::Value::as_u64);

    let mut lines = vec![format!("lsp: {action}")];
    if let Some(file) = file {
        lines.push(format!("\x1b[2m{}\x1b[0m", format_location(file, line, character)));
    }
    lines.join("\n")
}

fn format_bash_call(parsed: &serde_json::Value) -> String {
    let command = parsed
        .get("command")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if command.is_empty() {
        String::new()
    } else {
        format!("$ {}", truncate_for_summary(command, 160))
    }
}

pub(crate) fn first_visible_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
}

fn format_bash_result(parsed: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    if let Some(task_id) = parsed
        .get("backgroundTaskId")
        .and_then(|value| value.as_str())
    {
        lines.push(format!("backgrounded ({task_id})"));
    } else if let Some(status) = parsed
        .get("returnCodeInterpretation")
        .and_then(|value| value.as_str())
        .filter(|status| !status.is_empty())
    {
        lines.push(format!("\x1b[2m{status}\x1b[0m"));
    }

    if let Some(stdout) = parsed.get("stdout").and_then(|value| value.as_str()) {
        if !stdout.trim().is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(truncate_output_for_display(
                stdout,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            ));
        }
    }
    if let Some(stderr) = parsed.get("stderr").and_then(|value| value.as_str()) {
        if !stderr.trim().is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!(
                "\x1b[38;5;203m{}\x1b[0m",
                truncate_output_for_display(
                    stderr,
                    TOOL_OUTPUT_DISPLAY_MAX_LINES,
                    TOOL_OUTPUT_DISPLAY_MAX_CHARS,
                )
            ));
        }
    }

    if lines.is_empty() {
        lines.push("\x1b[2mcompleted\x1b[0m".to_string());
    }

    lines.join("\n")
}

fn format_read_result(parsed: &serde_json::Value) -> String {
    let file = parsed.get("file").unwrap_or(parsed);
    let path = extract_tool_path(file);
    let start_line = file
        .get("startLine")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let num_lines = file
        .get("numLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let total_lines = file
        .get("totalLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(num_lines);
    let content = file
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let end_line = start_line.saturating_add(num_lines.saturating_sub(1));

    let mut body = format!(
        "read_file: read {path} (lines {}-{} of {})\n\n{}",
        start_line,
        end_line.max(start_line),
        total_lines,
        truncate_output_for_display(content, READ_DISPLAY_MAX_LINES, READ_DISPLAY_MAX_CHARS)
    );

    if let Some(injected_context) = parsed
        .get("injectedContext")
        .and_then(serde_json::Value::as_str)
        .filter(|context| !context.trim().is_empty())
    {
        body.push_str("\n\n\x1b[2mcontext from nearby README\x1b[0m\n");
        body.push_str(&truncate_output_for_display(
            injected_context,
            READ_CONTEXT_DISPLAY_MAX_LINES,
            READ_CONTEXT_DISPLAY_MAX_CHARS,
        ));
    }

    body
}

fn format_write_result(parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let kind = parsed
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("write");
    let line_count = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .map_or(0, |content| content.lines().count());
    format!(
        "write_file: {} {path} \x1b[2m({line_count} lines)\x1b[0m",
        if kind == "create" { "Wrote" } else { "Updated" },
    )
}

fn format_structured_patch_preview(parsed: &serde_json::Value) -> Option<String> {
    let hunks = parsed.get("structuredPatch")?.as_array()?;
    let mut preview = Vec::new();
    for hunk in hunks.iter().take(2) {
        let lines = hunk.get("lines")?.as_array()?;
        for line in lines.iter().filter_map(|value| value.as_str()).take(6) {
            match line.chars().next() {
                Some('+') => preview.push(format!("\x1b[38;5;70m{line}\x1b[0m")),
                Some('-') => preview.push(format!("\x1b[38;5;203m{line}\x1b[0m")),
                _ => preview.push(line.to_string()),
            }
        }
    }
    if preview.is_empty() {
        None
    } else {
        Some(preview.join("\n"))
    }
}

fn format_edit_result(parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let suffix = if parsed
        .get("replaceAll")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        " (replace all)"
    } else {
        ""
    };
    let preview = format_structured_patch_preview(parsed).or_else(|| {
        let old_value = parsed
            .get("oldString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let new_value = parsed
            .get("newString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        format_patch_preview(old_value, new_value)
    });

    match preview {
        Some(preview) => format!("edit_file: edited {path}{suffix}\n\n{preview}"),
        None => format!("edit_file: edited {path}{suffix}"),
    }
}

fn format_glob_result(parsed: &serde_json::Value) -> String {
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if filenames.is_empty() {
        format!("glob_search matched {num_files} files")
    } else {
        format!("glob_search matched {num_files} files\n\n{filenames}")
    }
}

fn format_grep_result(parsed: &serde_json::Value) -> String {
    let num_matches = parsed
        .get("numMatches")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let content = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let summary = format!(
        "grep_search found {num_matches} matches across {num_files} files"
    );
    if !content.trim().is_empty() {
        format!(
            "{summary}\n\n{}",
            truncate_output_for_display(
                content,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            )
        )
    } else if !filenames.is_empty() {
        format!("{summary}\n\n{filenames}")
    } else {
        summary
    }
}

fn format_mcp_tool_result(parsed: &serde_json::Value) -> String {
    if let Some(error) = parsed.get("error") {
        let code = error
            .get("code")
            .and_then(serde_json::Value::as_i64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("MCP tool failed");
        return format!("\x1b[38;5;203mMCP JSON-RPC error {code}: {message}\x1b[0m");
    }

    let result = parsed.get("result").unwrap_or(parsed);
    if let Some(message) = result
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|message| !message.trim().is_empty())
    {
        let mut lines = Vec::new();
        if let Some(server) = result.get("server").and_then(serde_json::Value::as_str) {
            lines.push(format!("server: {server}"));
        }
        if let Some(tool) = result.get("tool").and_then(serde_json::Value::as_str) {
            lines.push(format!("tool: {tool}"));
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(truncate_output_for_display(
            message,
            TOOL_OUTPUT_DISPLAY_MAX_LINES,
            TOOL_OUTPUT_DISPLAY_MAX_CHARS,
        ));
        return lines.join("\n");
    }

    let mut lines = Vec::new();
    if let Some(server) = result
        .get("structuredContent")
        .and_then(|value| value.get("server"))
        .and_then(serde_json::Value::as_str)
    {
        lines.push(format!("server: {server}"));
    }
    if result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        lines.push("\x1b[38;5;203mtool reported MCP-level error\x1b[0m".to_string());
    }

    let text_blocks = extract_mcp_text_blocks(result);
    if !text_blocks.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(truncate_output_for_display(
            &text_blocks.join("\n"),
            TOOL_OUTPUT_DISPLAY_MAX_LINES,
            TOOL_OUTPUT_DISPLAY_MAX_CHARS,
        ));
    }

    if let Some(structured) = result
        .get("structuredContent")
        .filter(|value| !value.is_null())
    {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("\x1b[2mstructured content\x1b[0m".to_string());
        lines.push(render_json_value_preview(
            structured,
            TOOL_OUTPUT_DISPLAY_MAX_LINES,
            TOOL_OUTPUT_DISPLAY_MAX_CHARS,
        ));
    }

    if lines.is_empty() {
        format_generic_tool_result(parsed)
    } else {
        lines.join("\n")
    }
}

fn format_mcp_resources_result(parsed: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    if let Some(server) = parsed.get("server").and_then(serde_json::Value::as_str) {
        lines.push(format!("server: {server}"));
    }

    if let Some(message) = parsed
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|message| !message.trim().is_empty())
    {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(truncate_output_for_display(
            message,
            TOOL_OUTPUT_DISPLAY_MAX_LINES,
            TOOL_OUTPUT_DISPLAY_MAX_CHARS,
        ));
        return lines.join("\n");
    }

    let resources = parsed
        .get("resources")
        .or_else(|| parsed.get("result").and_then(|value| value.get("resources")))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let count = parsed
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(resources.len() as u64);

    lines.push(format!(
        "{count} resource{} available",
        if count == 1 { "" } else { "s" }
    ));

    if !resources.is_empty() {
        let resource_lines = resources
            .iter()
            .take(8)
            .map(format_mcp_resource_entry)
            .collect::<Vec<_>>()
            .join("\n");
        if !resource_lines.is_empty() {
            lines.push(String::new());
            lines.push(resource_lines);
        }
    }

    lines.join("\n")
}

fn format_read_mcp_resource_result(parsed: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    if let Some(server) = parsed.get("server").and_then(serde_json::Value::as_str) {
        lines.push(format!("server: {server}"));
    }

    if let Some(message) = parsed
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|message| !message.trim().is_empty())
    {
        if let Some(uri) = parsed.get("uri").and_then(serde_json::Value::as_str) {
            lines.push(format!("uri: {uri}"));
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(truncate_output_for_display(
            message,
            TOOL_OUTPUT_DISPLAY_MAX_LINES,
            TOOL_OUTPUT_DISPLAY_MAX_CHARS,
        ));
        return lines.join("\n");
    }

    let contents = parsed
        .get("result")
        .and_then(|value| value.get("contents"))
        .or_else(|| parsed.get("contents"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    if contents.is_empty() {
        return format_generic_tool_result(parsed);
    }

    lines.push(format!(
        "{} resource entr{} read",
        contents.len(),
        if contents.len() == 1 { "y" } else { "ies" }
    ));

    for content in contents.iter().take(2) {
        let uri = content
            .get("uri")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let mime = content
            .get("mimeType")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let preview = content
            .get("text")
            .or_else(|| content.get("blob"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        lines.push(String::new());
        lines.push(format!("{uri} · {mime}"));
        if !preview.trim().is_empty() {
            lines.push(truncate_output_for_display(
                preview,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            ));
        }
    }

    lines.join("\n")
}

fn format_lsp_tool_result(parsed: &serde_json::Value) -> String {
    let payload = parsed.get("data").unwrap_or(parsed);
    let action = payload
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("lsp");
    let file = payload
        .get("file")
        .or_else(|| payload.get("file_path"))
        .or_else(|| payload.get("filePath"))
        .and_then(serde_json::Value::as_str);
    let line = payload.get("line").and_then(serde_json::Value::as_u64);
    let character = payload
        .get("character")
        .and_then(serde_json::Value::as_u64);
    let hint = payload
        .get("hint")
        .or_else(|| payload.get("message"))
        .and_then(serde_json::Value::as_str);

    let mut lines = vec![format!("action: {action}")];
    if let Some(file) = file {
        lines.push(format!("location: {}", format_location(file, line, character)));
    }

    if let Some(hint) = hint.filter(|hint| !hint.trim().is_empty()) {
        lines.push(String::new());
        lines.push(truncate_output_for_display(
            hint,
            TOOL_OUTPUT_DISPLAY_MAX_LINES,
            TOOL_OUTPUT_DISPLAY_MAX_CHARS,
        ));
    }

    if let Some(examples) = payload.get("examples").and_then(serde_json::Value::as_array) {
        let rendered_examples = examples
            .iter()
            .filter_map(serde_json::Value::as_str)
            .take(3)
            .map(|example| format!("• {}", truncate_for_summary(example, 120)))
            .collect::<Vec<_>>();
        if !rendered_examples.is_empty() {
            lines.push(String::new());
            lines.push("\x1b[2mexamples\x1b[0m".to_string());
            lines.extend(rendered_examples);
        }
    }

    lines.join("\n")
}

fn extract_mcp_text_blocks(result: &serde_json::Value) -> Vec<String> {
    result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn format_mcp_resource_entry(resource: &serde_json::Value) -> String {
    let uri = resource
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    match resource
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
    {
        Some(name) => format!("{name} — {uri}"),
        None => uri.to_string(),
    }
}

fn render_json_value_preview(
    value: &serde_json::Value,
    max_lines: usize,
    max_chars: usize,
) -> String {
    let rendered = match value {
        serde_json::Value::String(text) => text.clone(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    };
    truncate_output_for_display(&rendered, max_lines, max_chars)
}

fn format_location(file: &str, line: Option<u64>, character: Option<u64>) -> String {
    match (line, character) {
        (Some(line), Some(character)) => format!("{file}:{line}:{character}"),
        (Some(line), None) => format!("{file}:{line}"),
        _ => file.to_string(),
    }
}

fn format_agent_result(parsed: &serde_json::Value) -> String {
    let id = parsed
        .get("agentId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let status = parsed
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let description = parsed
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let session = parsed
        .get("parentSessionId")
        .and_then(serde_json::Value::as_str)
        .map(shorten_session_id_for_report);
    let log_path = parsed
        .get("outputFile")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let detail = parsed
        .get("statusDetail")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let mut lines = vec![format!(
        "task {} · {status}",
        shorten_task_id(id),
    )];
    if !description.trim().is_empty() {
        lines.push(truncate_for_summary(description, 96));
    }
    if !detail.trim().is_empty() {
        lines.push(format!("\x1b[2m{}\x1b[0m", truncate_for_summary(detail, 96)));
    }
    lines.push(format!("log: {log_path}"));
    if let Some(session) = session {
        lines.push(format!("session: {session}"));
    }
    lines.push(format!("follow: /tasks attach {}", shorten_task_id(id)));
    lines.join("\n")
}

fn format_generic_tool_result(parsed: &serde_json::Value) -> String {
    let rendered_output = match parsed {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string_pretty(parsed).unwrap_or_else(|_| parsed.to_string())
        }
        _ => parsed.to_string(),
    };
    let preview = truncate_output_for_display(
        &rendered_output,
        TOOL_OUTPUT_DISPLAY_MAX_LINES,
        TOOL_OUTPUT_DISPLAY_MAX_CHARS,
    );

    if preview.is_empty() {
        String::new()
    } else {
        preview
    }
}

pub(crate) fn summarize_tool_payload(payload: &str) -> String {
    let compact = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value.to_string(),
        Err(_) => payload.trim().to_string(),
    };
    truncate_for_summary(&compact, 96)
}

fn truncate_output_for_display(content: &str, max_lines: usize, max_chars: usize) -> String {
    let original = content.trim_end_matches('\n');
    if original.is_empty() {
        return String::new();
    }

    let mut preview_lines = Vec::new();
    let mut used_chars = 0usize;
    let mut truncated = false;

    for (index, line) in original.lines().enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }

        let newline_cost = usize::from(!preview_lines.is_empty());
        let available = max_chars.saturating_sub(used_chars + newline_cost);
        if available == 0 {
            truncated = true;
            break;
        }

        let line_chars = line.chars().count();
        if line_chars > available {
            preview_lines.push(line.chars().take(available).collect::<String>());
            truncated = true;
            break;
        }

        preview_lines.push(line.to_string());
        used_chars += newline_cost + line_chars;
    }

    let mut preview = preview_lines.join("\n");
    if truncated {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push_str(DISPLAY_TRUNCATION_NOTICE);
    }
    preview
}
