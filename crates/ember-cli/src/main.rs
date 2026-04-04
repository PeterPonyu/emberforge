mod codebase_map;
mod context;
mod doctor;
mod init;
mod input;
mod keywords;
mod notifications;
mod render;
mod task_mgmt;
mod tool_display;
mod ui;

use tool_display::*;

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use api::{
    resolve_startup_auth_source, AuthSource, ClawApiClient, ContentBlockDelta, InputContentBlock,
    InputMessage, MessageRequest, MessageResponse, OutputContentBlock, ProviderClient,
    StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

use commands::{
    handle_agents_slash_command, handle_plugins_slash_command, handle_skills_slash_command,
    render_slash_command_help, resume_supported_slash_commands, slash_command_specs,
    suggest_slash_commands, SlashCommand,
};
use compat_harness::{extract_manifest, UpstreamPaths};
use init::initialize_repo;
use plugins::{PluginManager, PluginManagerConfig};
use render::{MarkdownStreamState, TerminalRenderer};
use runtime::{
    clear_oauth_credentials, generate_pkce_pair, generate_state, load_system_prompt,
    parse_oauth_callback_request_target, save_oauth_credentials, ApiClient, ApiRequest,
    AssistantEvent, CompactionConfig, ConfigLoader, ConfigSource, ContentBlock,
    ConversationMessage, ConversationRuntime, MessageRole, OAuthAuthorizationRequest, OAuthConfig,
    OAuthTokenExchangeRequest, PermissionMode, PermissionPolicy, ProjectContext, RuntimeError,
    RuntimeUiConfig, Session, TokenUsage, ToolError, ToolExecutor, UsageTracker,
};
use serde_json::json;
use task_mgmt::{
    attach_to_task, count_running_background_tasks, find_task_by_prefix, load_task_manifests,
    render_task_list_report, render_task_logs_report, render_task_show_report, request_task_stop,
    shorten_task_id, task_status_label,
};
use tools::GlobalToolRegistry;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use ui::animation::{
    is_fire_spinner_running, play_intro_animation, should_play_intro_animation,
    start_fire_spinner, stop_fire_spinner,
};
use ui::banner::{render_startup_banner, StartupBannerContext};
use ui::capabilities::{detect_terminal_capabilities, TerminalCapabilities};
use ui::hud::{render_turn_hud, TurnHudContext};

const ANTHROPIC_DEFAULT_MODEL: &str = "claude-opus-4-6";
const XAI_DEFAULT_MODEL: &str = "grok-3-mini";
const DOTENV_FILE_NAME: &str = ".env";
const EMBER_PROMPT: &str = "ember> ";
const MODEL_ALIAS_ROWS: &[(&str, &str)] = &[
    ("opus", "claude-opus-4-6"),
    ("sonnet", "claude-sonnet-4-6"),
    ("haiku", "claude-haiku-4-5-20251213"),
    ("grok", "grok-3"),
    ("grok-mini", "grok-3-mini"),
];
const DOCTOR_FAMILY_REPRESENTATIVES: &[(&str, &str)] = &[
    ("aya-expanse", "aya-expanse:8b"),
    ("deepseek-r1", "deepseek-r1:1.5b"),
    ("exaone3.5", "exaone3.5:2.4b"),
    ("falcon3", "falcon3:3b"),
    ("gemma2", "gemma2:9b-instruct-q8_0"),
    ("gemma3", "gemma3:1b"),
    ("glm4", "glm4:9b"),
    ("granite3.3", "granite3.3:2b"),
    ("internlm2", "internlm2:1.8b"),
    ("llama3.1", "llama3.1:8b-instruct-q4_K_M"),
    ("llama3.2", "llama3.2:1b"),
    ("mistral", "mistral:7b-instruct-v0.3-q4_K_M"),
    ("mistral-nemo", "mistral-nemo:12b-instruct-2407-q8_0"),
    ("mistral-small", "mistral-small:24b-instruct-2501-q4_K_M"),
    ("phi4", "phi4:14b-q8_0"),
    ("phi4-mini", "phi4-mini:latest"),
    ("qwen2.5", "qwen2.5:0.5b"),
    ("qwen3", "qwen3:4b"),
    ("qwen3.5", "qwen3.5:4b"),
    ("solar", "solar:10.7b"),
    ("solar-pro", "solar-pro:22b"),
    ("starcoder2", "starcoder2:3b"),
    ("yi", "yi:6b"),
];
const PLACEHOLDER_PROVIDER_CREDENTIAL_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "XAI_API_KEY",
    "OPENAI_API_KEY",
];

/// Shared flag: when true, show thinking/reasoning tokens during streaming.
static VERBOSE_MODE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
const OLLAMA_DEFAULT_MODEL: &str = "qwen3:8b";

fn initialize_process_env() {
    if let Ok(cwd) = env::current_dir() {
        initialize_process_env_from(&cwd);
    } else {
        sanitize_placeholder_provider_credentials();
    }
}

fn initialize_process_env_from(start_dir: &Path) {
    load_dotenv_file_from(start_dir);
    sanitize_placeholder_provider_credentials();
}

fn load_dotenv_file_from(start_dir: &Path) {
    let Some(dotenv_path) = find_dotenv_path(start_dir) else {
        return;
    };
    let Ok(contents) = fs::read_to_string(dotenv_path) else {
        return;
    };

    for raw_line in contents.lines() {
        let Some((key, value)) = parse_dotenv_line(raw_line) else {
            continue;
        };
        if env::var_os(&key).is_none() {
            env::set_var(key, value);
        }
    }
}

fn find_dotenv_path(start_dir: &Path) -> Option<PathBuf> {
    start_dir
        .ancestors()
        .map(|dir| dir.join(DOTENV_FILE_NAME))
        .find(|candidate| candidate.is_file())
}

fn parse_dotenv_line(raw_line: &str) -> Option<(String, String)> {
    let line = raw_line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    let line = line
        .strip_prefix("export")
        .map(str::trim_start)
        .unwrap_or(line);
    let (key, raw_value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }

    Some((key.to_string(), parse_dotenv_value(raw_value.trim())))
}

fn parse_dotenv_value(raw_value: &str) -> String {
    if raw_value.len() >= 2 {
        let first = raw_value.as_bytes()[0] as char;
        let last = raw_value.as_bytes()[raw_value.len() - 1] as char;
        if first == last && matches!(first, '"' | '\'') {
            let inner = &raw_value[1..raw_value.len() - 1];
            return if first == '"' {
                unescape_double_quoted_dotenv(inner)
            } else {
                inner.to_string()
            };
        }
    }

    raw_value
        .split_once(" #")
        .map_or(raw_value, |(value, _)| value)
        .trim()
        .to_string()
}

fn unescape_double_quoted_dotenv(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            unescaped.push(ch);
            continue;
        }

        match chars.next() {
            Some('n') => unescaped.push('\n'),
            Some('r') => unescaped.push('\r'),
            Some('t') => unescaped.push('\t'),
            Some('"') => unescaped.push('"'),
            Some('\\') => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }

    unescaped
}

fn sanitize_placeholder_provider_credentials() {
    for key in PLACEHOLDER_PROVIDER_CREDENTIAL_KEYS {
        if env::var(key)
            .ok()
            .is_some_and(|value| looks_like_placeholder_secret(&value))
        {
            env::remove_var(key);
        }
    }
}

fn looks_like_placeholder_secret(value: &str) -> bool {
    let normalized = value
        .trim()
        .trim_matches(|ch| matches!(ch, '<' | '>'))
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "changeme" | "change_me" | "replace_me" | "replace-with-real-value"
            | "placeholder" | "your_api_key_here"
    ) || normalized.starts_with("your_") && normalized.ends_with("_here")
}

#[cfg(not(test))]
fn env_var_has_real_value(key: &str) -> bool {
    env::var(key)
        .ok()
        .is_some_and(|value| !value.trim().is_empty() && !looks_like_placeholder_secret(&value))
}

#[cfg(not(test))]
fn has_anthropic_auth() -> bool {
    env_var_has_real_value("ANTHROPIC_API_KEY") || env_var_has_real_value("ANTHROPIC_AUTH_TOKEN")
}

#[cfg(not(test))]
fn has_xai_auth() -> bool {
    env_var_has_real_value("XAI_API_KEY")
}

fn default_model_choice(has_anthropic_auth: bool, has_xai_auth: bool) -> &'static str {
    if has_anthropic_auth {
        ANTHROPIC_DEFAULT_MODEL
    } else if has_xai_auth {
        XAI_DEFAULT_MODEL
    } else {
        OLLAMA_DEFAULT_MODEL
    }
}

#[cfg(not(test))]
fn default_model() -> String {
    default_model_choice(has_anthropic_auth(), has_xai_auth()).to_string()
}

fn provider_label_for_model(model: &str) -> &'static str {
    match api::detect_provider_kind(model) {
        api::ProviderKind::ClawApi => "Anthropic",
        api::ProviderKind::Xai => "xAI",
        api::ProviderKind::OpenAi => "OpenAI",
        api::ProviderKind::Ollama => "Ollama",
    }
}

// Keep this for test compatibility
#[cfg(test)]
const DEFAULT_MODEL: &str = ANTHROPIC_DEFAULT_MODEL;
#[cfg(test)]
fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

fn max_tokens_for_model(model: &str) -> u32 {
    // For cloud models (Anthropic/xAI), use the API crate's heuristic.
    // For Ollama models, use cached metadata when present and otherwise fall
    // back immediately to a sensible default so the first user turn does not
    // block on a cold `/api/show` metadata fetch.
    if model.contains("claude") || model.contains("opus") || model.contains("sonnet")
        || model.contains("haiku") || model.contains("grok")
    {
        return api::max_tokens_for_model(model);
    }
    runtime::model_profiles::cached_profile_or_default(model).recommended_max_tokens()
}
const DEFAULT_DATE: &str = "2026-03-31";
const DEFAULT_OAUTH_CALLBACK_PORT: u16 = 4545;
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_TARGET: Option<&str> = option_env!("TARGET");
const GIT_SHA: Option<&str> = option_env!("GIT_SHA");
const INTERNAL_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

type AllowedToolSet = BTreeSet<String>;

fn main() {
    initialize_process_env();
        if let Err(error) = run() {
        eprintln!("{}", render_cli_error(&error.to_string()));
        std::process::exit(1);
    }
}

fn render_cli_error(problem: &str) -> String {
    let mut lines = vec!["Error".to_string()];
    for (index, line) in problem.lines().enumerate() {
        let label = if index == 0 {
            "  Problem          "
        } else {
            "                   "
        };
        lines.push(format!("{label}{line}"));
    }
    lines.push("  Help             ember --help".to_string());
    lines.join("\n")
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args)? {
        CliAction::DumpManifests => dump_manifests(),
        CliAction::BootstrapPlan => print_bootstrap_plan(),
        CliAction::RenderSmoke { scenario } => run_render_smoke(scenario.as_deref())?,
        CliAction::Models => print_models(),
        CliAction::Doctor { mode, model } => doctor::run_doctor_cli(mode.as_deref(), &model)?,
        CliAction::Agents { args } => LiveCli::print_agents(args.as_deref())?,
        CliAction::Tasks { args } => LiveCli::print_tasks(args.as_deref(), None)?,
        CliAction::Skills { args } => LiveCli::print_skills(args.as_deref())?,
        CliAction::PrintSystemPrompt { cwd, date } => print_system_prompt(cwd, date),
        CliAction::Version => print_version(),
        CliAction::ResumeSession {
            session_path,
            commands,
        } => resume_session(&session_path, &commands),
        CliAction::Prompt {
            prompt,
            model,
            output_format,
            allowed_tools,
            permission_mode,
        } => LiveCli::new(model, true, allowed_tools, permission_mode, false)?
            .run_turn_with_output(&prompt, output_format)?,
        CliAction::Login => run_login()?,
        CliAction::Logout => run_logout()?,
        CliAction::Init => run_init()?,
        CliAction::Repl {
            model,
            allowed_tools,
            permission_mode,
        } => run_repl(model, allowed_tools, permission_mode)?,
        CliAction::Help => print_help(),
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    DumpManifests,
    BootstrapPlan,
    RenderSmoke {
        scenario: Option<String>,
    },
    Models,
    Agents {
        args: Option<String>,
    },
    Tasks {
        args: Option<String>,
    },
    Skills {
        args: Option<String>,
    },
    PrintSystemPrompt {
        cwd: PathBuf,
        date: String,
    },
    Version,
    Doctor {
        mode: Option<String>,
        model: String,
    },
    ResumeSession {
        session_path: PathBuf,
        commands: Vec<String>,
    },
    Prompt {
        prompt: String,
        model: String,
        output_format: CliOutputFormat,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
    },
    Login,
    Logout,
    Init,
    Repl {
        model: String,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
    },
    // prompt-mode formatting is only supported for non-interactive runs
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliOutputFormat {
    Text,
    Json,
    Ndjson,
}

impl CliOutputFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Ndjson => "ndjson",
        }
    }

    fn is_structured(self) -> bool {
        !matches!(self, Self::Text)
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "ndjson" => Ok(Self::Ndjson),
            other => Err(format!(
                "unsupported value for --output-format: {other} (expected text, json, or ndjson)"
            )),
        }
    }
}

fn require_prompt_mode_for_output_format(output_format: CliOutputFormat) -> Result<(), String> {
    if output_format.is_structured() {
        Err(format!(
            "--output-format {} is only supported with prompt mode (-p or `ember prompt ...`)",
            output_format.as_str()
        ))
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_lines)]
fn parse_args(args: &[String]) -> Result<CliAction, String> {
    let mut model = default_model();
    let mut output_format = CliOutputFormat::Text;
    let mut permission_mode = default_permission_mode();
    let mut wants_version = false;
    let mut allowed_tool_values = Vec::new();
    let mut rest = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--version" | "-V" => {
                wants_version = true;
                index += 1;
            }
            "--model" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --model".to_string())?;
                model = resolve_model_alias(value).to_string();
                index += 2;
            }
            flag if flag.starts_with("--model=") => {
                model = resolve_model_alias(&flag[8..]).to_string();
                index += 1;
            }
            "--output-format" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --output-format".to_string())?;
                output_format = CliOutputFormat::parse(value)?;
                index += 2;
            }
            "--permission-mode" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --permission-mode".to_string())?;
                permission_mode = parse_permission_mode_arg(value)?;
                index += 2;
            }
            flag if flag.starts_with("--output-format=") => {
                output_format = CliOutputFormat::parse(&flag[16..])?;
                index += 1;
            }
            flag if flag.starts_with("--permission-mode=") => {
                permission_mode = parse_permission_mode_arg(&flag[18..])?;
                index += 1;
            }
            "--dangerously-skip-permissions" => {
                permission_mode = PermissionMode::DangerFullAccess;
                index += 1;
            }
            "-p" => {
                // Emberforge compat: -p "prompt" = one-shot prompt
                let prompt = args[index + 1..].join(" ");
                if prompt.trim().is_empty() {
                    return Err("-p requires a prompt string".to_string());
                }
                return Ok(CliAction::Prompt {
                    prompt,
                    model: resolve_model_alias(&model).to_string(),
                    output_format,
                    allowed_tools: normalize_allowed_tools(&allowed_tool_values)?,
                    permission_mode,
                });
            }
            "--print" => {
                // Emberforge compat: --print makes output non-interactive
                output_format = CliOutputFormat::Text;
                index += 1;
            }
            "--allowedTools" | "--allowed-tools" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --allowedTools".to_string())?;
                allowed_tool_values.push(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--allowedTools=") => {
                allowed_tool_values.push(flag[15..].to_string());
                index += 1;
            }
            flag if flag.starts_with("--allowed-tools=") => {
                allowed_tool_values.push(flag[16..].to_string());
                index += 1;
            }
            other => {
                rest.push(other.to_string());
                index += 1;
            }
        }
    }

    if wants_version {
        require_prompt_mode_for_output_format(output_format)?;
        return Ok(CliAction::Version);
    }

    let allowed_tools = normalize_allowed_tools(&allowed_tool_values)?;

    if rest.is_empty() {
        require_prompt_mode_for_output_format(output_format)?;
        return Ok(CliAction::Repl {
            model,
            allowed_tools,
            permission_mode,
        });
    }
    if matches!(rest.first().map(String::as_str), Some("--help" | "-h")) {
        require_prompt_mode_for_output_format(output_format)?;
        return Ok(CliAction::Help);
    }
    if rest.first().map(String::as_str) == Some("--resume") {
        require_prompt_mode_for_output_format(output_format)?;
        return parse_resume_args(&rest[1..]);
    }

    let action = match rest[0].as_str() {
        "dump-manifests" => Ok(CliAction::DumpManifests),
        "bootstrap-plan" => Ok(CliAction::BootstrapPlan),
        "render-smoke" => Ok(CliAction::RenderSmoke {
            scenario: join_optional_args(&rest[1..]),
        }),
        "models" => Ok(CliAction::Models),
        "doctor" => Ok(CliAction::Doctor {
            mode: join_optional_args(&rest[1..]),
            model,
        }),
        "agents" => Ok(CliAction::Agents {
            args: join_optional_args(&rest[1..]),
        }),
        "tasks" => Ok(CliAction::Tasks {
            args: join_optional_args(&rest[1..]),
        }),
        "skills" => Ok(CliAction::Skills {
            args: join_optional_args(&rest[1..]),
        }),
        "system-prompt" => parse_system_prompt_args(&rest[1..]),
        "login" => Ok(CliAction::Login),
        "logout" => Ok(CliAction::Logout),
        "init" => Ok(CliAction::Init),
        "prompt" => {
            let prompt = rest[1..].join(" ");
            if prompt.trim().is_empty() {
                return Err("prompt subcommand requires a prompt string".to_string());
            }
            Ok(CliAction::Prompt {
                prompt,
                model,
                output_format,
                allowed_tools,
                permission_mode,
            })
        }
        other if other.starts_with('/') => parse_direct_slash_cli_action(&rest),
        _other => Ok(CliAction::Prompt {
            prompt: rest.join(" "),
            model,
            output_format,
            allowed_tools,
            permission_mode,
        }),
    }?;

    if output_format.is_structured() && !matches!(action, CliAction::Prompt { .. }) {
        require_prompt_mode_for_output_format(output_format)?;
    }

    Ok(action)
}

fn join_optional_args(args: &[String]) -> Option<String> {
    let joined = args.join(" ");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_direct_slash_cli_action(rest: &[String]) -> Result<CliAction, String> {
    let raw = rest.join(" ");
    match SlashCommand::parse(&raw) {
        Some(SlashCommand::Help) => Ok(CliAction::Help),
        Some(SlashCommand::Agents { args }) => Ok(CliAction::Agents { args }),
        Some(SlashCommand::Tasks { action }) => Ok(CliAction::Tasks { args: action }),
        Some(SlashCommand::Skills { args }) => Ok(CliAction::Skills { args }),
        Some(command) => Err(format_direct_slash_command_error(
            match &command {
                SlashCommand::Unknown(name) => format!("/{name}"),
                _ => rest[0].clone(),
            }
            .as_str(),
            matches!(command, SlashCommand::Unknown(_)),
        )),
        None => Err(format!("unknown subcommand: {}", rest[0])),
    }
}

fn format_direct_slash_command_error(command: &str, is_unknown: bool) -> String {
    let trimmed = command.trim().trim_start_matches('/');
    let mut lines = vec![
        "Direct slash command unavailable".to_string(),
        format!("  Command          /{trimmed}"),
    ];
    if is_unknown {
        append_slash_command_suggestions(&mut lines, trimmed);
    } else {
        lines.push("  Try              Start `ember` to use interactive slash commands".to_string());
        lines.push(
            "  Tip              Resume-safe commands also work with `ember --resume SESSION.json ...`"
                .to_string(),
        );
    }
    lines.join("\n")
}

pub(crate) fn resolve_model_alias(model: &str) -> &str {
    match model {
        "opus" => "claude-opus-4-6",
        "sonnet" => "claude-sonnet-4-6",
        "haiku" => "claude-haiku-4-5-20251213",
        "grok" | "grok-3" => "grok-3",
        "grok-mini" | "grok-3-mini" => "grok-3-mini",
        "grok-2" => "grok-2",
        _ => model,
    }
}

fn normalize_allowed_tools(values: &[String]) -> Result<Option<AllowedToolSet>, String> {
    if values.is_empty() {
        return Ok(None);
    }

    match current_tool_registry() {
        Ok(registry) => registry.normalize_allowed_tools(values),
        Err(_) => GlobalToolRegistry::builtin().normalize_allowed_tools(values),
    }
}

fn current_tool_registry() -> Result<GlobalToolRegistry, String> {
    let cwd = env::current_dir().map_err(|error| error.to_string())?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader.load().map_err(|error| error.to_string())?;
    let plugin_manager = build_plugin_manager(&cwd, &loader, &runtime_config);
    let plugin_tools = plugin_manager
        .aggregated_tools()
        .map_err(|error| error.to_string())?;
    GlobalToolRegistry::with_plugin_tools(plugin_tools)
}

fn parse_permission_mode_arg(value: &str) -> Result<PermissionMode, String> {
    normalize_permission_mode(value)
        .ok_or_else(|| {
            format!(
                "unsupported permission mode '{value}'. Use read-only, workspace-write, or danger-full-access."
            )
        })
        .map(permission_mode_from_label)
}

fn permission_mode_from_label(mode: &str) -> PermissionMode {
    match mode {
        "read-only" => PermissionMode::ReadOnly,
        "workspace-write" => PermissionMode::WorkspaceWrite,
        "danger-full-access" => PermissionMode::DangerFullAccess,
        other => panic!("unsupported permission mode label: {other}"),
    }
}

fn default_permission_mode() -> PermissionMode {
    env::var("EMBER_PERMISSION_MODE")
        .ok()
        .or_else(|| env::var("CLAW_PERMISSION_MODE").ok())
        .as_deref()
        .and_then(normalize_permission_mode)
        .map_or(PermissionMode::DangerFullAccess, permission_mode_from_label)
}

/// Meta-tools that should NOT be sent to the model — they cause the model
/// to call them unnecessarily (e.g. SendUserMessage for simple greetings,
/// AskUserQuestion when no clarification is needed, StructuredOutput for
/// every response). The model should only use core workspace tools.
const HIDDEN_TOOLS: &[&str] = &[
    "SendUserMessage",
    "Brief",
    "AskUserQuestion",
    "StructuredOutput",
    "EnterPlanMode",
    "ExitPlanMode",
    "MCPTool",
    "LSPTool",
    "ListMcpResources",
    "ReadMcpResource",
];

fn filter_tool_specs(
    tool_registry: &GlobalToolRegistry,
    allowed_tools: Option<&AllowedToolSet>,
) -> Vec<ToolDefinition> {
    tool_registry
        .definitions(allowed_tools)
        .into_iter()
        .filter(|def| !HIDDEN_TOOLS.contains(&def.name.as_str()))
        .collect()
}

fn parse_system_prompt_args(args: &[String]) -> Result<CliAction, String> {
    let mut cwd = env::current_dir().map_err(|error| error.to_string())?;
    let mut date = DEFAULT_DATE.to_string();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --cwd".to_string())?;
                cwd = PathBuf::from(value);
                index += 2;
            }
            "--date" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --date".to_string())?;
                date.clone_from(value);
                index += 2;
            }
            other => return Err(format!("unknown system-prompt option: {other}")),
        }
    }

    Ok(CliAction::PrintSystemPrompt { cwd, date })
}

fn parse_resume_args(args: &[String]) -> Result<CliAction, String> {
    let session_path = args
        .first()
        .ok_or_else(|| "missing session path for --resume".to_string())
        .map(PathBuf::from)?;
    let commands = args[1..].to_vec();
    if commands
        .iter()
        .any(|command| !command.trim_start().starts_with('/'))
    {
        return Err("--resume trailing arguments must be slash commands".to_string());
    }
    Ok(CliAction::ResumeSession {
        session_path,
        commands,
    })
}

fn dump_manifests() {
    let workspace_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let paths = UpstreamPaths::from_workspace_dir(&workspace_dir);
    match extract_manifest(&paths) {
        Ok(manifest) => {
            println!("commands: {}", manifest.commands.entries().len());
            println!("tools: {}", manifest.tools.entries().len());
            println!("bootstrap phases: {}", manifest.bootstrap.phases().len());
        }
        Err(error) => {
            eprintln!("failed to extract manifests: {error}");
            std::process::exit(1);
        }
    }
}

fn print_bootstrap_plan() {
    for phase in runtime::BootstrapPlan::claw_default().phases() {
        println!("- {phase:?}");
    }
}

fn default_oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: String::from("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
        authorize_url: String::from("https://platform.claw.dev/oauth/authorize"),
        token_url: String::from("https://platform.claw.dev/v1/oauth/token"),
        callback_port: None,
        manual_redirect_url: None,
        scopes: vec![
            String::from("user:profile"),
            String::from("user:inference"),
            String::from("user:sessions:claw_code"),
        ],
    }
}

fn run_login() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let config = ConfigLoader::default_for(&cwd).load()?;
    let default_oauth = default_oauth_config();
    let oauth = config.oauth().unwrap_or(&default_oauth);
    let callback_port = oauth.callback_port.unwrap_or(DEFAULT_OAUTH_CALLBACK_PORT);
    let redirect_uri = runtime::loopback_redirect_uri(callback_port);
    let pkce = generate_pkce_pair()?;
    let state = generate_state()?;
    let authorize_url =
        OAuthAuthorizationRequest::from_config(oauth, redirect_uri.clone(), state.clone(), &pkce)
            .build_url();

    println!("Starting Emberforge OAuth login...");
    println!("Listening for callback on {redirect_uri}");
    if let Err(error) = open_browser(&authorize_url) {
        eprintln!("warning: failed to open browser automatically: {error}");
        println!("Open this URL manually:\n{authorize_url}");
    }

    let callback = wait_for_oauth_callback(callback_port)?;
    if let Some(error) = callback.error {
        let description = callback
            .error_description
            .unwrap_or_else(|| "authorization failed".to_string());
        return Err(io::Error::other(format!("{error}: {description}")).into());
    }
    let code = callback.code.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "callback did not include code")
    })?;
    let returned_state = callback.state.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "callback did not include state")
    })?;
    if returned_state != state {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oauth state mismatch").into());
    }

    let client = ClawApiClient::from_auth(AuthSource::None).with_base_url(api::read_base_url());
    let exchange_request =
        OAuthTokenExchangeRequest::from_config(oauth, code, state, pkce.verifier, redirect_uri);
    let runtime = tokio::runtime::Runtime::new()?;
    let token_set = runtime.block_on(client.exchange_oauth_code(oauth, &exchange_request))?;
    save_oauth_credentials(&runtime::OAuthTokenSet {
        access_token: token_set.access_token,
        refresh_token: token_set.refresh_token,
        expires_at: token_set.expires_at,
        scopes: token_set.scopes,
    })?;
    println!("Emberforge OAuth login complete.");
    Ok(())
}

fn run_logout() -> Result<(), Box<dyn std::error::Error>> {
    clear_oauth_credentials()?;
    println!("Emberforge OAuth credentials cleared.");
    Ok(())
}

fn open_browser(url: &str) -> io::Result<()> {
    let commands = if cfg!(target_os = "macos") {
        vec![("open", vec![url])]
    } else if cfg!(target_os = "windows") {
        vec![("cmd", vec!["/C", "start", "", url])]
    } else {
        vec![("xdg-open", vec![url])]
    };
    for (program, args) in commands {
        match Command::new(program).args(args).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no supported browser opener command found",
    ))
}

fn wait_for_oauth_callback(
    port: u16,
) -> Result<runtime::OAuthCallbackParams, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let (mut stream, _) = listener.accept()?;
    let mut buffer = [0_u8; 4096];
    let bytes_read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let request_line = request.lines().next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing callback request line")
    })?;
    let target = request_line.split_whitespace().nth(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "missing callback request target",
        )
    })?;
    let callback = parse_oauth_callback_request_target(target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let body = if callback.error.is_some() {
        "Emberforge OAuth login failed. You can close this window."
    } else {
        "Emberforge OAuth login succeeded. You can close this window."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    Ok(callback)
}

fn print_system_prompt(cwd: PathBuf, date: String) {
    match load_system_prompt(cwd, date, env::consts::OS, "unknown") {
        Ok(sections) => println!("{}", sections.join("\n\n")),
        Err(error) => {
            eprintln!("failed to build system prompt: {error}");
            std::process::exit(1);
        }
    }
}

fn print_version() {
    println!("{}", render_version_report());
}

fn print_models() {
    let model = default_model();
    println!("{}", format_available_models_report(&model, &discover_available_models(&model)));
}

struct ScriptedToolTurnApiClient {
    call_count: usize,
    command: String,
    success_reply: String,
}

impl ScriptedToolTurnApiClient {
    fn new(command: impl Into<String>, success_reply: impl Into<String>) -> Self {
        Self {
            call_count: 0,
            command: command.into(),
            success_reply: success_reply.into(),
        }
    }
}

impl ApiClient for ScriptedToolTurnApiClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.call_count += 1;
        match self.call_count {
            1 => Ok(vec![
                AssistantEvent::ToolUse {
                    id: "render-smoke-tool-1".to_string(),
                    name: "bash".to_string(),
                    input: json!({ "command": self.command }).to_string(),
                },
                AssistantEvent::MessageStop,
            ]),
            2 => {
                let last_message = request
                    .messages
                    .last()
                    .ok_or_else(|| RuntimeError::new("tool result should be present"))?;
                let (output, is_error) = match last_message.blocks.first() {
                    Some(ContentBlock::ToolResult {
                        output,
                        is_error,
                        ..
                    }) => (output.clone(), *is_error),
                    _ => {
                        return Err(RuntimeError::new(
                            "scripted tool smoke expected a tool result message",
                        ))
                    }
                };
                let reply = if is_error {
                    "BLOCKED".to_string()
                } else if output.contains(&self.success_reply) {
                    self.success_reply.clone()
                } else {
                    format!("missing marker: {}", self.success_reply)
                };
                Ok(vec![
                    AssistantEvent::TextDelta(reply),
                    AssistantEvent::MessageStop,
                ])
            }
            _ => Err(RuntimeError::new("unexpected extra API call in render-smoke")),
        }
    }
}

fn run_prompt_transport_smoke_summary(
    command: &str,
    success_reply: &str,
    permission_mode: PermissionMode,
    output_format: CliOutputFormat,
) -> Result<runtime::TurnSummary, Box<dyn std::error::Error>> {
    let (_, tool_registry) = build_runtime_plugin_state()?;
    let allowed_tools = Some(
        [String::from("bash")]
            .into_iter()
            .collect::<AllowedToolSet>(),
    );
    let executor = CliToolExecutor::new(allowed_tools, false, tool_registry.clone());
    let permission_policy = permission_policy(permission_mode, &tool_registry);
    let mut runtime = ConversationRuntime::new(
        Session::new(),
        ScriptedToolTurnApiClient::new(command, success_reply),
        executor,
        permission_policy,
        Vec::new(),
    );

    match permission_mode {
        PermissionMode::WorkspaceWrite | PermissionMode::Prompt => {
            let mut permission_prompter = MachineReadablePermissionPrompter::new(output_format);
            runtime
                .run_turn("render smoke", Some(&mut permission_prompter))
                .map_err(|error| io::Error::other(error.to_string()).into())
        }
        _ => runtime
            .run_turn("render smoke", None)
            .map_err(|error| io::Error::other(error.to_string()).into()),
    }
}

fn print_prompt_transport_smoke_json(
    command: &str,
    success_reply: &str,
    permission_mode: PermissionMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let summary = run_prompt_transport_smoke_summary(
        command,
        success_reply,
        permission_mode,
        CliOutputFormat::Json,
    )?;
    write_structured_json_line(&prompt_summary_payload("render-smoke", &summary))?;
    Ok(())
}

fn print_prompt_transport_smoke_ndjson(
    command: &str,
    success_reply: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let summary = run_prompt_transport_smoke_summary(
        command,
        success_reply,
        PermissionMode::DangerFullAccess,
        CliOutputFormat::Ndjson,
    )?;
    for event in prompt_summary_ndjson_events("render-smoke", &summary) {
        write_structured_json_line(&event)?;
    }
    Ok(())
}

fn run_render_smoke(scenario: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let scenario = scenario.unwrap_or("help").trim();
    match scenario {
        "help" | "" => {
            println!(
                "Render smoke\n  Scenarios         tool-success | tool-truncate | json-tool | prompt-json-tool | prompt-ndjson-tool | prompt-json-permission-denied | markdown-demo\n  Example           ember render-smoke tool-success"
            );
            Ok(())
        }
        "tool-success" => {
            let (_, tool_registry) = build_runtime_plugin_state()?;
            let input = r#"{"command":"printf TOOL_OK"}"#;
            println!("{}", format_tool_call_start("bash", input));
            let mut executor = CliToolExecutor::new(
                Some([String::from("bash")].into_iter().collect()),
                true,
                tool_registry,
            );
            executor.execute("bash", input)?;
            Ok(())
        }
        "tool-truncate" => {
            let (_, tool_registry) = build_runtime_plugin_state()?;
            let input = r#"{"command":"yes ROW | head -n 120"}"#;
            println!("{}", format_tool_call_start("bash", input));
            let mut executor = CliToolExecutor::new(
                Some([String::from("bash")].into_iter().collect()),
                true,
                tool_registry,
            );
            executor.execute("bash", input)?;
            Ok(())
        }
        "json-tool" => {
            let (_, tool_registry) = build_runtime_plugin_state()?;
            let mut executor = CliToolExecutor::new(
                Some([String::from("bash")].into_iter().collect()),
                false,
                tool_registry,
            );
            let input = json!({ "command": "printf TOOL_JSON_OK" });
            let output = executor.execute("bash", &input.to_string())?;
            let parsed_output = serde_json::from_str::<serde_json::Value>(&output)
                .unwrap_or_else(|_| json!({ "raw": output }));
            println!(
                "{}",
                json!({
                    "message": "TOOL_JSON_OK",
                    "model": "render-smoke",
                    "iterations": 1,
                    "tool_uses": [{
                        "id": "render-smoke-tool-1",
                        "name": "bash",
                        "input": input,
                    }],
                    "tool_results": [{
                        "tool_use_id": "render-smoke-tool-1",
                        "tool_name": "bash",
                        "output": parsed_output,
                        "is_error": false,
                    }],
                    "usage": TokenUsage::default(),
                })
            );
            Ok(())
        }
        "prompt-json-tool" => print_prompt_transport_smoke_json(
            "printf TOOL_JSON_WET",
            "TOOL_JSON_WET",
            PermissionMode::DangerFullAccess,
        ),
        "prompt-ndjson-tool" => {
            print_prompt_transport_smoke_ndjson("printf TOOL_NDJSON_WET", "TOOL_NDJSON_WET")
        }
        "prompt-json-permission-denied" => print_prompt_transport_smoke_json(
            "printf TOOL_PERMISSION_DENIED",
            "BLOCKED",
            PermissionMode::WorkspaceWrite,
        ),
        "markdown-demo" => {
            let renderer = TerminalRenderer::new();
            renderer.stream_markdown(
                "# Demo\n\n> Quote\n\n- item\n\n```rust\nfn main() { println!(\"hi\"); }\n```",
                &mut io::stdout(),
            )?;
            Ok(())
        }
        other => Err(format!(
            "unknown render-smoke scenario '{other}' (expected tool-success, tool-truncate, json-tool, prompt-json-tool, prompt-ndjson-tool, prompt-json-permission-denied, or markdown-demo)"
        )
        .into()),
    }
}

fn resume_session(session_path: &Path, commands: &[String]) {
    let session = match Session::load_from_path(session_path) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("failed to restore session: {error}");
            std::process::exit(1);
        }
    };

    if commands.is_empty() {
        println!(
            "Restored session from {} ({} messages).",
            session_path.display(),
            session.messages.len()
        );
        return;
    }

    let mut session = session;
    for raw_command in commands {
        let Some(command) = SlashCommand::parse(raw_command) else {
            eprintln!("unsupported resumed command: {raw_command}");
            std::process::exit(2);
        };
        match run_resume_command(session_path, &session, &command) {
            Ok(ResumeCommandOutcome {
                session: next_session,
                message,
            }) => {
                session = next_session;
                if let Some(message) = message {
                    println!("{message}");
                }
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ResumeCommandOutcome {
    session: Session,
    message: Option<String>,
}

#[derive(Debug, Clone)]
struct StatusContext {
    cwd: PathBuf,
    session_path: Option<PathBuf>,
    loaded_config_files: usize,
    discovered_config_files: usize,
    memory_file_count: usize,
    project_root: Option<PathBuf>,
    git_branch: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct StatusUsage {
    message_count: usize,
    turns: u32,
    latest: TokenUsage,
    cumulative: TokenUsage,
    estimated_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AvailableModelCatalog {
    pub(crate) ollama_models: Vec<String>,
    pub(crate) ollama_status: String,
}

pub(crate) fn discover_available_models(current_model: &str) -> AvailableModelCatalog {
    let mut ollama_models = BTreeSet::new();
    let current_is_ollama = matches!(
        api::detect_provider_kind(current_model),
        api::ProviderKind::Ollama
    );
    if current_is_ollama {
        ollama_models.insert(current_model.to_string());
    }

    let ollama_status = match runtime::model_profiles::list_ollama_models() {
        Ok(models) => {
            ollama_models.extend(models);
            if ollama_models.is_empty() {
                "reachable, but no local models were reported".to_string()
            } else {
                format!("reachable - {} local model(s) detected", ollama_models.len())
            }
        }
        Err(error) => {
            if current_is_ollama {
                format!(
                    "unreachable - showing the current session model only ({})",
                    truncate_for_summary(&error, 60)
                )
            } else {
                format!("unreachable ({})", truncate_for_summary(&error, 60))
            }
        }
    };

    AvailableModelCatalog {
        ollama_models: ollama_models.into_iter().collect(),
        ollama_status,
    }
}

fn format_available_models_report(current_model: &str, catalog: &AvailableModelCatalog) -> String {
    let mut lines = vec![
        "Available models".to_string(),
        format!("  Ollama state     {}", catalog.ollama_status),
    ];

    if catalog.ollama_models.is_empty() {
        lines.push("  Ollama models    none listed".to_string());
    } else {
        lines.push("  Ollama models".to_string());
        for model in &catalog.ollama_models {
            let marker = if model == current_model { "*" } else { "-" };
            lines.push(format!("    {marker} {model}"));
        }
    }

    lines.push("Cloud shortcuts".to_string());
    for (alias, model) in MODEL_ALIAS_ROWS {
        let marker = if *model == current_model { "*" } else { "-" };
        lines.push(format!("  {marker} {alias:<10} {model}"));
    }

    lines.push("Routing shortcuts".to_string());
    lines.push("  - auto       Route simpler prompts to a faster model".to_string());
    lines.push("  - hybrid     Prefer local for lighter work, cloud for harder work".to_string());
    lines.join("\n")
}

fn render_model_report(model: &str, message_count: usize, turns: u32) -> String {
    let catalog = discover_available_models(model);
    format!(
        "{}\n\n{}",
        format_model_report(model, message_count, turns),
        format_available_models_report(model, &catalog)
    )
}

fn format_model_report(model: &str, message_count: usize, turns: u32) -> String {
    format!(
        "Model
  Current          {model}
    Session          {message_count} messages | {turns} turns

Aliases
  opus             claude-opus-4-6
  sonnet           claude-sonnet-4-6
  haiku            claude-haiku-4-5-20251213
  grok             grok-3
  grok-mini        grok-3-mini

Next
  /model           Show the current model and available choices
  /model list      List available models
  /model <name>    Switch models for this REPL session"
    )
}

fn format_model_switch_report(previous: &str, next: &str, message_count: usize) -> String {
    format!(
        "Model updated
  Previous         {previous}
  Current          {next}
  Preserved        {message_count} messages
  Tip              Existing conversation context stayed attached"
    )
}

fn format_permissions_report(mode: &str) -> String {
    let modes = [
        ("read-only", "Read/search tools only", mode == "read-only"),
        (
            "workspace-write",
            "Edit files inside the workspace",
            mode == "workspace-write",
        ),
        (
            "danger-full-access",
            "Unrestricted tool access",
            mode == "danger-full-access",
        ),
    ]
    .into_iter()
    .map(|(name, description, is_current)| {
        let marker = if is_current { "* current" } else { "- available" };
        format!("  {name:<18} {marker:<11} {description}")
    })
    .collect::<Vec<_>>()
    .join(
        "
",
    );

    let effect = match mode {
        "read-only" => "Only read/search tools can run automatically",
        "workspace-write" => "Editing tools can modify files in the workspace",
        "danger-full-access" => "All tools can run without additional sandbox limits",
        _ => "Unknown permission mode",
    };

    format!(
        "Permissions
  Active mode      {mode}
  Effect           {effect}

Modes
{modes}

Next
  /permissions              Show the current mode
  /permissions <mode>       Switch modes for subsequent tool calls"
    )
}

fn format_permissions_switch_report(previous: &str, next: &str) -> String {
    format!(
        "Permissions updated
  Previous mode    {previous}
  Active mode      {next}
  Applies to       Subsequent tool calls in this REPL
  Tip              Run /permissions to review all available modes"
    )
}

fn format_cost_report(usage: TokenUsage) -> String {
    format!(
        "Cost
  Input tokens     {}
  Output tokens    {}
  Cache create     {}
  Cache read       {}
  Total tokens     {}

Next
  /status          See session + workspace context
  /compact         Trim local history if the session is getting large",
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        usage.total_tokens(),
    )
}

fn format_resume_report(session_path: &str, message_count: usize, turns: u32) -> String {
    format!(
        "Session resumed
  Session file     {session_path}
    History          {message_count} messages | {turns} turns
    Next             /status | /diff | /export"
    )
}

fn format_compact_report(removed: usize, resulting_messages: usize, skipped: bool) -> String {
    if skipped {
        format!(
            "Compact
  Result           skipped
  Reason           Session is already below the compaction threshold
  Messages kept    {resulting_messages}"
        )
    } else {
        format!(
            "Compact
  Result           compacted
  Messages removed {removed}
  Messages kept    {resulting_messages}
  Tip              Use /status to review the trimmed session"
        )
    }
}

fn parse_git_status_metadata(status: Option<&str>) -> (Option<PathBuf>, Option<String>) {
    let Some(status) = status else {
        return (None, None);
    };
    let branch = status.lines().next().and_then(|line| {
        line.strip_prefix("## ")
            .map(|line| {
                line.split(['.', ' '])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .filter(|value| !value.is_empty())
    });
    let project_root = find_git_root().ok();
    (project_root, branch)
}

fn find_git_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        return Err("not a git repository".into());
    }
    let path = String::from_utf8(output.stdout)?.trim().to_string();
    if path.is_empty() {
        return Err("empty git root".into());
    }
    Ok(PathBuf::from(path))
}

#[allow(clippy::too_many_lines)]
fn run_resume_command(
    session_path: &Path,
    session: &Session,
    command: &SlashCommand,
) -> Result<ResumeCommandOutcome, Box<dyn std::error::Error>> {
    match command {
        SlashCommand::Help => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_repl_help()),
        }),
        SlashCommand::Compact => {
            let result = runtime::compact_session(
                session,
                CompactionConfig {
                    max_estimated_tokens: 0,
                    ..CompactionConfig::default()
                },
            );
            let removed = result.removed_message_count;
            let kept = result.compacted_session.messages.len();
            let skipped = removed == 0;
            result.compacted_session.save_to_path(session_path)?;
            Ok(ResumeCommandOutcome {
                session: result.compacted_session,
                message: Some(format_compact_report(removed, kept, skipped)),
            })
        }
        SlashCommand::Clear { confirm } => {
            if !confirm {
                return Ok(ResumeCommandOutcome {
                    session: session.clone(),
                    message: Some(
                        "clear: confirmation required; rerun with /clear --confirm".to_string(),
                    ),
                });
            }
            let cleared = Session::new();
            cleared.save_to_path(session_path)?;
            Ok(ResumeCommandOutcome {
                session: cleared,
                message: Some(format!(
                    "Cleared resumed session file {}.",
                    session_path.display()
                )),
            })
        }
        SlashCommand::Status => {
            let tracker = UsageTracker::from_session(session);
            let usage = tracker.cumulative_usage();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_status_report(
                    "restored-session",
                    StatusUsage {
                        message_count: session.messages.len(),
                        turns: tracker.turns(),
                        latest: tracker.current_turn_usage(),
                        cumulative: usage,
                        estimated_tokens: 0,
                    },
                    default_permission_mode().as_str(),
                    &status_context(Some(session_path))?,
                )),
            })
        }
        SlashCommand::Cost => {
            let usage = UsageTracker::from_session(session).cumulative_usage();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_cost_report(usage)),
            })
        }
        SlashCommand::Config { section } => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_config_report(section.as_deref())?),
        }),
        SlashCommand::Memory => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_memory_report()?),
        }),
        SlashCommand::Init => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(init_ember_md()?),
        }),
        SlashCommand::Diff => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_diff_report()?),
        }),
        SlashCommand::Version => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_version_report()),
        }),
        SlashCommand::Export { path } => {
            let export_path = resolve_export_path(path.as_deref(), session)?;
            fs::write(&export_path, render_export_text(session))?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Export\n  Result           wrote transcript\n  File             {}\n  Messages         {}",
                    export_path.display(),
                    session.messages.len(),
                )),
            })
        }
        SlashCommand::Agents { args } => {
            let cwd = env::current_dir()?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_agents_slash_command(args.as_deref(), &cwd)?),
            })
        }
        SlashCommand::Skills { args } => {
            let cwd = env::current_dir()?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_skills_slash_command(args.as_deref(), &cwd)?),
            })
        }
        SlashCommand::Bughunter { .. }
        | SlashCommand::Branch { .. }
        | SlashCommand::Worktree { .. }
        | SlashCommand::CommitPushPr { .. }
        | SlashCommand::Commit
        | SlashCommand::Pr { .. }
        | SlashCommand::Issue { .. }
        | SlashCommand::Ultraplan { .. }
        | SlashCommand::Teleport { .. }
        | SlashCommand::DebugToolCall
        | SlashCommand::Doctor { .. }
        | SlashCommand::Resume { .. }
        | SlashCommand::Model { .. }
        | SlashCommand::Permissions { .. }
        | SlashCommand::Session { .. }
        | SlashCommand::Plugins { .. }
        | SlashCommand::Hooks
        | SlashCommand::Mcp { .. }
        | SlashCommand::Plan
        | SlashCommand::Tasks { .. }
        | SlashCommand::Review { .. }
        | SlashCommand::Effort { .. }
        | SlashCommand::Theme { .. }
        | SlashCommand::Fast
        | SlashCommand::Unknown(_) => Err("unsupported resumed slash command".into()),
    }
}

fn run_repl(
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cli = LiveCli::new(model, true, allowed_tools, permission_mode, true)?;
    let startup_capabilities = detect_terminal_capabilities();
    let mut editor = input::LineEditor::new(EMBER_PROMPT, slash_command_completion_candidates());
    if should_play_intro_animation(&cli.ui_config, &startup_capabilities) {
        let _ = play_intro_animation(&startup_capabilities);
    }
    println!("{}", cli.startup_banner_with_capabilities(&startup_capabilities));
    if let Some(hint) = doctor::startup_doctor_hint(&cli.model) {
        println!("{hint}\n");
    }

    loop {
        match editor.read_line()? {
            input::ReadOutcome::Submit(input) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if matches!(trimmed, "/exit" | "/quit") {
                    cli.persist_session()?;
                    break;
                }
                if trimmed == "/verbose" {
                    cli.verbose = !cli.verbose;
                    VERBOSE_MODE.store(cli.verbose, std::sync::atomic::Ordering::Relaxed);
                    println!(
                        "\x1b[2mVerbose mode: {}\x1b[0m",
                        if cli.verbose {
                            "\x1b[33mON\x1b[2m (thinking tokens visible)"
                        } else {
                            "\x1b[32mOFF\x1b[2m (thinking tokens hidden)"
                        }
                    );
                    continue;
                }
                if let Some(command) = SlashCommand::parse(trimmed) {
                    if cli.handle_repl_command(command)? {
                        cli.persist_session()?;
                    }
                    continue;
                }
                editor.push_history(&input);
                cli.run_turn(&input)?;
            }
            input::ReadOutcome::Cancel => {}
            input::ReadOutcome::Exit => {
                cli.persist_session()?;
                break;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct SessionHandle {
    id: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct ManagedSessionSummary {
    id: String,
    path: PathBuf,
    modified_epoch_secs: u64,
    message_count: usize,
}

struct LiveCli {
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    render_turn_hud: bool,
    ui_config: RuntimeUiConfig,
    system_prompt: Vec<String>,
    runtime: ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>,
    session: SessionHandle,
    tracer: telemetry::SessionTracer,
    /// When true, show thinking/reasoning tokens during generation.
    verbose: bool,
    cold_start_hint: bool,
    /// Current reasoning effort level.
    effort: runtime::EffortLevel,
    /// Fast mode — prioritize speed over thoroughness.
    fast_mode: bool,
}

struct ScopedTaskSessionEnv {
    previous_ember_session_id: Option<String>,
    previous_claw_session_id: Option<String>,
}

impl ScopedTaskSessionEnv {
    fn new(session_id: &str) -> Self {
        let previous_ember_session_id = env::var("EMBER_SESSION_ID").ok();
        let previous_claw_session_id = env::var("CLAW_SESSION_ID").ok();
        env::set_var("EMBER_SESSION_ID", session_id);
        env::set_var("CLAW_SESSION_ID", session_id);
        Self {
            previous_ember_session_id,
            previous_claw_session_id,
        }
    }
}

impl Drop for ScopedTaskSessionEnv {
    fn drop(&mut self) {
        match self.previous_ember_session_id.as_deref() {
            Some(value) => env::set_var("EMBER_SESSION_ID", value),
            None => env::remove_var("EMBER_SESSION_ID"),
        }
        match self.previous_claw_session_id.as_deref() {
            Some(value) => env::set_var("CLAW_SESSION_ID", value),
            None => env::remove_var("CLAW_SESSION_ID"),
        }
    }
}

impl LiveCli {
    fn new(
        model: String,
        enable_tools: bool,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
        render_turn_hud: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        let runtime_config = ConfigLoader::default_for(&cwd).load()?;
        let system_prompt = build_system_prompt()?;
        let session = create_managed_session_handle()?;
        let runtime = build_runtime(
            Session::new(),
            model.clone(),
            system_prompt.clone(),
            enable_tools,
            true,
            allowed_tools.clone(),
            permission_mode,
            None,
        )?;
        // Set up telemetry: write events to .ember/telemetry/<session>.jsonl
        let telemetry_dir = env::current_dir()
            .unwrap_or_default()
            .join(".ember")
            .join("telemetry");
        let telemetry_sink: Arc<dyn telemetry::TelemetrySink> =
            match telemetry::JsonlTelemetrySink::new(
                telemetry_dir.join(format!("{}.jsonl", &session.id)),
            ) {
                Ok(sink) => Arc::new(sink),
                Err(_) => Arc::new(telemetry::MemoryTelemetrySink::default()),
            };
        let tracer = telemetry::SessionTracer::new(&session.id, telemetry_sink);
        tracer.record("session_start", {
            let mut attrs = serde_json::Map::new();
            attrs.insert("model".into(), serde_json::Value::String(model.clone()));
            attrs
        });

        let cold_start_hint = matches!(api::detect_provider_kind(&model), api::ProviderKind::Ollama);
        let cli = Self {
            model,
            allowed_tools,
            permission_mode,
            render_turn_hud,
            ui_config: runtime_config.ui().clone(),
            system_prompt,
            runtime,
            session,
            tracer,
            verbose: false,
            cold_start_hint,
            effort: runtime::EffortLevel::default(),
            fast_mode: false,
        };
        cli.persist_session()?;

        // Pre-warm the model by sending a minimal probe request.
        // This loads the model into VRAM before the user starts typing,
        // eliminating the cold-start delay on the first real query.
        cli.prewarm_model();

        Ok(cli)
    }

    fn prewarm_model(&self) {
        use api::{InputContentBlock, InputMessage, MessageRequest, ProviderClient};

        let Ok(client) = ProviderClient::from_model(&self.model) else {
            return;
        };
        let request = MessageRequest {
            model: self.model.clone(),
            max_tokens: 1,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: ".".to_string(),
                }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        };

        // Fire-and-forget in background — don't block startup.
        let model_name = self.model.clone();
        thread::spawn(move || {
            if matches!(api::detect_provider_kind(&model_name), api::ProviderKind::Ollama) {
                runtime::model_profiles::warm_profile_cache(&model_name);
            }
            if let Ok(rt) = tokio::runtime::Runtime::new() {
                let _ = rt.block_on(client.send_message(&request));
                // Silently loaded — don't print to avoid interrupting the spinner.
                let _ = model_name;
            }
        });
    }

    fn startup_banner_with_capabilities(&self, capabilities: &TerminalCapabilities) -> String {
        let cwd = env::current_dir().ok();
        let workspace_name = cwd
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("workspace");
        let git_branch = status_context(Some(&self.session.path))
            .ok()
            .and_then(|context| context.git_branch);
        let workspace_summary = git_branch.as_deref().map_or_else(
            || workspace_name.to_string(),
            |branch| format!("{workspace_name} - {branch}"),
        );
        let has_project_guidance = cwd.as_ref().is_some_and(|path| {
            path.join("EMBER.md").is_file() || path.join("CLAW.md").is_file()
        });
        let quick_start = if has_project_guidance {
            "/help | /status"
        } else {
            "/init | /help"
        };
        let context = StartupBannerContext {
            app_name: String::from("Emberforge"),
            version: VERSION.to_string(),
            workspace_summary,
            model: self.model.clone(),
            provider_label: provider_label_for_model(&self.model).to_string(),
            session_id: self.session.id.clone(),
            quick_start: quick_start.to_string(),
            show_setup_hint: !has_project_guidance,
        };
        render_startup_banner(&context, capabilities, &self.ui_config)
    }

    fn render_status_report(&self) -> Result<String, Box<dyn std::error::Error>> {
        let cumulative = self.runtime.usage().cumulative_usage();
        let latest = self.runtime.usage().current_turn_usage();
        Ok(format_status_report(
            &self.model,
            StatusUsage {
                message_count: self.runtime.session().messages.len(),
                turns: self.runtime.usage().turns(),
                latest,
                cumulative,
                estimated_tokens: self.runtime.estimated_tokens(),
            },
            self.permission_mode.as_str(),
            &status_context(Some(&self.session.path))?,
        ))
    }

    fn builtin_text_response(
        &self,
        input: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        if is_builtin_status_query(input) {
            return self.render_status_report().map(Some);
        }
        Ok(None)
    }

    fn run_turn(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(response) = self.builtin_text_response(input)? {
            println!("{response}");
            self.print_turn_hud_if_enabled()?;
            return Ok(());
        }

        // Classify task size for orchestration decisions.
        let task_size = keywords::classify_task_size(input);
        if task_size == keywords::TaskSize::Heavy {
            eprintln!(
                "\x1b[38;5;208m[task]\x1b[0m \x1b[2mdetected heavy task — consider /ultraplan\x1b[0m"
            );
        }

        // Detect magic keywords and apply mode activations.
        let keyword_matches = keywords::detect_keywords(input);
        for mode in keywords::extract_mode_activations(&keyword_matches) {
            if let Some(level) = runtime::EffortLevel::from_str(mode) {
                if level != self.effort {
                    self.effort = level;
                    eprintln!(
                        "\x1b[38;5;208m[keyword]\x1b[0m \x1b[2meffort → {}\x1b[0m",
                        level.as_str()
                    );
                }
            }
            if mode == "plan" && !self.runtime.session().plan_mode {
                eprintln!("\x1b[38;5;208m[keyword]\x1b[0m \x1b[2mplan mode activated\x1b[0m");
            }
        }

        // Build enhanced input with keyword context if detected.
        let enhanced_input = match keywords::build_keyword_context(&keyword_matches) {
            Some(context) => {
                eprintln!(
                    "\x1b[38;5;208m[keyword]\x1b[0m \x1b[2mdetected: {}\x1b[0m",
                    keyword_matches
                        .iter()
                        .map(|m| m.keyword)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                format!("{context}\n\n{input}")
            }
            None => input.to_string(),
        };

        // Record turn start in telemetry
        self.tracer.record("turn_start", {
            let mut attrs = serde_json::Map::new();
            attrs.insert(
                "input_length".into(),
                serde_json::Value::from(input.len() as u64),
            );
            attrs
        });
        let turn_start = Instant::now();

        // Fire-pixel spinner on stderr while waiting for the model.
        let spinner_label = if self.cold_start_hint {
            "loading local model"
        } else {
            "thinking"
        };
        let capabilities = detect_terminal_capabilities();
        let spinner_handle = start_fire_spinner(spinner_label, capabilities.color_enabled());

        let _task_session_env = ScopedTaskSessionEnv::new(&self.session.id);
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let result = self.runtime.run_turn(&enhanced_input, Some(&mut permission_prompter));

        // Ensure spinner stops and line is cleared
        stop_fire_spinner();
        let _ = spinner_handle.join();

        let elapsed_ms = turn_start.elapsed().as_millis() as u64;
        match result {
            Ok(_) => {
                self.cold_start_hint = false;
                self.tracer.record("turn_complete", {
                    let mut attrs = serde_json::Map::new();
                    attrs.insert("elapsed_ms".into(), serde_json::Value::from(elapsed_ms));
                    attrs.insert("status".into(), serde_json::Value::String("ok".into()));
                    attrs
                });
                let secs = elapsed_ms as f64 / 1000.0;
                eprintln!(
                    "\x1b[38;5;208m[ember]\x1b[0m \x1b[32m[done]\x1b[0m \x1b[2m({secs:.1}s)\x1b[0m"
                );
                self.persist_session()?;
                self.print_turn_hud_if_enabled()?;
                Ok(())
            }
            Err(error) => {
                self.cold_start_hint = false;
                self.tracer.record("turn_failed", {
                    let mut attrs = serde_json::Map::new();
                    attrs.insert("elapsed_ms".into(), serde_json::Value::from(elapsed_ms));
                    attrs.insert(
                        "error".into(),
                        serde_json::Value::String(error.to_string()),
                    );
                    attrs
                });
                let secs = elapsed_ms as f64 / 1000.0;
                eprintln!(
                    "\x1b[38;5;208m[ember]\x1b[0m \x1b[31m[failed]\x1b[0m \x1b[2m({secs:.1}s)\x1b[0m"
                );
                Err(Box::new(error))
            }
        }
    }

    fn print_turn_hud_if_enabled(&self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.render_turn_hud {
            return Ok(());
        }
        let capabilities = detect_terminal_capabilities();
        if let Some(line) = self.turn_hud_line(&capabilities)? {
            println!("{line}");
        }
        Ok(())
    }

    fn turn_hud_line(
        &self,
        capabilities: &TerminalCapabilities,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let context = self.turn_hud_context()?;
        Ok(render_turn_hud(&context, capabilities, &self.ui_config))
    }

    fn turn_hud_context(&self) -> Result<TurnHudContext, Box<dyn std::error::Error>> {
        let context = status_context(Some(&self.session.path))?;
        let task_counts = count_running_background_tasks(Some(&self.session.id))?;
        let cumulative = self.runtime.usage().cumulative_usage();
        Ok(TurnHudContext {
            git_branch: context.git_branch,
            model: self.model.clone(),
            provider_label: provider_label_for_model(&self.model).to_string(),
            permission_mode: self.permission_mode.as_str().to_string(),
            turns: self.runtime.usage().turns(),
            estimated_tokens: self.runtime.estimated_tokens(),
            background_task_count: task_counts.total_running,
            session_task_count: task_counts.session_running,
            session_id: self.session.id.clone(),
            effort: self.effort.as_str().to_string(),
            cumulative_input_tokens: cumulative.input_tokens,
            cumulative_output_tokens: cumulative.output_tokens,
            thinking_visible: self.verbose,
        })
    }

    fn run_turn_with_output(
        &mut self,
        input: &str,
        output_format: CliOutputFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match output_format {
            CliOutputFormat::Text => self.run_turn(input),
            CliOutputFormat::Json => self.run_prompt_json(input),
            CliOutputFormat::Ndjson => self.run_prompt_ndjson(input),
        }
    }

    fn run_machine_readable_turn(
        &mut self,
        input: &str,
        output_format: CliOutputFormat,
    ) -> Result<MachineReadableTurnResult, Box<dyn std::error::Error>> {
        if let Some(response) = self.builtin_text_response(input)? {
            return Ok(MachineReadableTurnResult::Builtin { message: response });
        }

        let session = self.runtime.session().clone();
        let mut runtime = build_runtime(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            false,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        let _task_session_env = ScopedTaskSessionEnv::new(&self.session.id);
        let mut permission_prompter = MachineReadablePermissionPrompter::new(output_format);
        let summary = runtime.run_turn(input, Some(&mut permission_prompter))?;
        self.runtime = runtime;
        self.persist_session()?;
        Ok(MachineReadableTurnResult::Summary(summary))
    }

    fn run_prompt_json(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        let payload = match self.run_machine_readable_turn(input, CliOutputFormat::Json)? {
            MachineReadableTurnResult::Builtin { message } => {
                prompt_builtin_payload(&self.model, &message)
            }
            MachineReadableTurnResult::Summary(summary) => {
                prompt_summary_payload(&self.model, &summary)
            }
        };
        write_structured_json_line(&payload)?;
        Ok(())
    }

    fn run_prompt_ndjson(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        let events = match self.run_machine_readable_turn(input, CliOutputFormat::Ndjson)? {
            MachineReadableTurnResult::Builtin { message } => {
                prompt_builtin_ndjson_events(&self.model, &message)
            }
            MachineReadableTurnResult::Summary(summary) => {
                prompt_summary_ndjson_events(&self.model, &summary)
            }
        };
        for event in events {
            write_structured_json_line(&event)?;
        }
        Ok(())
    }

    fn handle_repl_command(
        &mut self,
        command: SlashCommand,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(match command {
            SlashCommand::Help => {
                println!("{}", render_repl_help());
                false
            }
            SlashCommand::Status => {
                self.print_status();
                false
            }
            SlashCommand::Bughunter { scope } => {
                self.run_bughunter(scope.as_deref())?;
                false
            }
            SlashCommand::Commit => {
                self.run_commit()?;
                true
            }
            SlashCommand::Pr { context } => {
                self.run_pr(context.as_deref())?;
                false
            }
            SlashCommand::Issue { context } => {
                self.run_issue(context.as_deref())?;
                false
            }
            SlashCommand::Ultraplan { task } => {
                self.run_ultraplan(task.as_deref())?;
                false
            }
            SlashCommand::Teleport { target } => {
                self.run_teleport(target.as_deref())?;
                false
            }
            SlashCommand::DebugToolCall => {
                self.run_debug_tool_call()?;
                false
            }
            SlashCommand::Doctor { mode } => {
                println!("{}", doctor::run_doctor(mode.as_deref(), &self.model)?);
                false
            }
            SlashCommand::Compact => {
                self.compact()?;
                false
            }
            SlashCommand::Model { model } => self.set_model(model)?,
            SlashCommand::Permissions { mode } => self.set_permissions(mode)?,
            SlashCommand::Clear { confirm } => self.clear_session(confirm)?,
            SlashCommand::Cost => {
                self.print_cost();
                false
            }
            SlashCommand::Resume { session_path } => self.resume_session(session_path)?,
            SlashCommand::Config { section } => {
                Self::print_config(section.as_deref())?;
                false
            }
            SlashCommand::Memory => {
                Self::print_memory()?;
                false
            }
            SlashCommand::Init => {
                run_init()?;
                false
            }
            SlashCommand::Diff => {
                Self::print_diff()?;
                false
            }
            SlashCommand::Version => {
                Self::print_version();
                false
            }
            SlashCommand::Export { path } => {
                self.export_session(path.as_deref())?;
                false
            }
            SlashCommand::Session { action, target } => {
                self.handle_session_command(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Plugins { action, target } => {
                self.handle_plugins_command(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Agents { args } => {
                Self::print_agents(args.as_deref())?;
                false
            }
            SlashCommand::Skills { args } => {
                Self::print_skills(args.as_deref())?;
                false
            }
            SlashCommand::Branch { .. } => {
                eprintln!(
                    "{}",
                    render_mode_unavailable("branch", "git branch commands")
                );
                false
            }
            SlashCommand::Worktree { .. } => {
                eprintln!(
                    "{}",
                    render_mode_unavailable("worktree", "git worktree commands")
                );
                false
            }
            SlashCommand::CommitPushPr { .. } => {
                eprintln!(
                    "{}",
                    render_mode_unavailable("commit-push-pr", "commit + push + PR automation")
                );
                false
            }
            // ── Hooks ──
            SlashCommand::Hooks => {
                let cwd = env::current_dir().unwrap_or_default();
                let loader = runtime::ConfigLoader::default_for(&cwd);
                match loader.load() {
                    Ok(config) => {
                        let hooks = config.hooks();
                        let pre = hooks.pre_tool_use();
                        let post = hooks.post_tool_use();
                        if pre.is_empty() && post.is_empty() {
                            println!("\x1b[2mNo hooks configured.\x1b[0m");
                            println!("\x1b[2mAdd hooks in .ember.json under \"hooks\".\x1b[0m");
                        } else {
                            println!("\x1b[1mConfigured hooks\x1b[0m");
                            if !pre.is_empty() {
                                println!("  \x1b[36mPreToolUse\x1b[0m:");
                                for cmd in pre {
                                    println!("    {cmd}");
                                }
                            }
                            if !post.is_empty() {
                                println!("  \x1b[36mPostToolUse\x1b[0m:");
                                for cmd in post {
                                    println!("    {cmd}");
                                }
                            }
                        }
                    }
                    Err(_) => println!("\x1b[2mNo hooks configured (no .ember.json found).\x1b[0m"),
                }
                false
            }
            // ── MCP ──
            SlashCommand::Mcp { action } => {
                let cwd = env::current_dir().unwrap_or_default();
                let loader = runtime::ConfigLoader::default_for(&cwd);
                match loader.load() {
                    Ok(config) => {
                        let mcp = config.mcp();
                        let servers = mcp.servers();
                        if servers.is_empty() {
                            println!("\x1b[2mNo MCP servers configured.\x1b[0m");
                            println!("\x1b[2mAdd servers in .ember.json under \"mcpServers\".\x1b[0m");
                        } else {
                            println!("\x1b[1mMCP servers\x1b[0m ({} configured)", servers.len());
                            for (name, _server) in servers {
                                println!("  \x1b[36m{name}\x1b[0m");
                            }
                            if let Some(action) = &action {
                                println!("\x1b[2mAction: {action} (connect/disconnect requires server restart)\x1b[0m");
                            }
                        }
                    }
                    Err(_) => println!("\x1b[2mNo MCP servers configured (no .ember.json found).\x1b[0m"),
                }
                false
            }
            // ── Plan mode ──
            SlashCommand::Plan => {
                let session = self.runtime.session();
                let new_mode = !session.plan_mode;
                if new_mode {
                    println!("\x1b[33m[plan]\x1b[0m Plan mode ON - tools disabled, design-only conversation");
                } else {
                    println!("\x1b[32m[plan]\x1b[0m Plan mode OFF - tools re-enabled");
                }
                false
            }
            // ── Tasks ──
            SlashCommand::Tasks { action } => {
                Self::print_tasks(action.as_deref(), Some(&self.session.id))?;
                false
            }
            // ── Review ──
            SlashCommand::Review { scope } => {
                let scope_desc = scope.as_deref().unwrap_or("recent changes");
                println!("\x1b[1m[review]\x1b[0m Code review - scope: {scope_desc}");
                println!("\x1b[2mSending review prompt to model...\x1b[0m");
                let review_prompt = format!(
                    "Review the following code changes for bugs, security issues, and improvements. \
                     Scope: {scope_desc}. Use the bash tool to run `git diff` and analyze the output."
                );
                if let Err(e) = self.run_turn(&review_prompt) {
                    eprintln!("\x1b[31mReview failed: {e}\x1b[0m");
                }
                true // session changed
            }
            // ── Fast mode ──
            SlashCommand::Fast => {
                self.fast_mode = !self.fast_mode;
                println!(
                    "\x1b[2mFast mode: {}\x1b[0m",
                    if self.fast_mode {
                        "\x1b[33mON\x1b[2m (prioritize speed)"
                    } else {
                        "\x1b[32mOFF\x1b[2m (normal mode)"
                    }
                );
                false
            }
            // ── Effort level ──
            SlashCommand::Effort { level } => {
                if let Some(level_str) = level.as_deref() {
                    if let Some(new_level) = runtime::EffortLevel::from_str(level_str) {
                        self.effort = new_level;
                        println!(
                            "\x1b[2mEffort level: \x1b[33m{}\x1b[0m",
                            new_level.as_str()
                        );
                    } else {
                        eprintln!(
                            "Unknown effort level '{}'. Use: relaxed, balanced, thorough",
                            level_str
                        );
                    }
                } else {
                    println!(
                        "Effort level\n  Current          \x1b[33m{}\x1b[0m\n  Available        relaxed | balanced | thorough",
                        self.effort.as_str()
                    );
                }
                false
            }
            // ── Theme ──
            SlashCommand::Theme { mode } => {
                if let Some(mode_str) = mode.as_deref() {
                    if let Some(new_theme) = runtime::ThemeMode::from_str(mode_str) {
                        self.ui_config = self.ui_config.clone().with_theme(new_theme);
                        println!(
                            "\x1b[2mTheme: \x1b[33m{}\x1b[0m",
                            new_theme.as_str()
                        );
                    } else {
                        eprintln!(
                            "Unknown theme '{}'. Use: dark, light",
                            mode_str
                        );
                    }
                } else {
                    println!(
                        "Theme\n  Current          \x1b[33m{}\x1b[0m\n  Available        dark | light",
                        self.ui_config.theme().as_str()
                    );
                }
                false
            }
            SlashCommand::Unknown(name) => {
                eprintln!("{}", render_unknown_repl_command(&name));
                false
            }
        })
    }

    fn persist_session(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime.session().save_to_path(&self.session.path)?;
        Ok(())
    }

    fn print_status(&self) {
        println!(
            "{}",
            self.render_status_report()
                .expect("status report should render")
        );
    }

    fn set_model(&mut self, model: Option<String>) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(model) = model else {
            println!(
                "{}",
                render_model_report(
                    &self.model,
                    self.runtime.session().messages.len(),
                    self.runtime.usage().turns(),
                )
            );
            return Ok(false);
        };

        if matches!(model.as_str(), "list" | "ls" | "available" | "models") {
            println!(
                "{}",
                format_available_models_report(
                    &self.model,
                    &discover_available_models(&self.model)
                )
            );
            return Ok(false);
        }

        // Handle routing strategies: /model auto, /model hybrid
        let strategy = runtime::model_router::parse_strategy(&model);
        let model = match &strategy {
            runtime::model_router::RoutingStrategy::Auto {
                fast_model,
                capable_model,
            } => {
                println!(
                    "\x1b[33m[route]\x1b[0m auto: simple -> {fast_model}, complex -> {capable_model}"
                );
                capable_model.clone()
            }
            runtime::model_router::RoutingStrategy::Hybrid {
                local_model,
                cloud_model,
            } => {
                println!(
                    "\x1b[33m[route]\x1b[0m hybrid: local -> {local_model}, cloud -> {cloud_model}"
                );
                local_model.clone()
            }
            runtime::model_router::RoutingStrategy::Fixed(m) => {
                resolve_model_alias(m).to_string()
            }
        };

        if model == self.model {
            println!(
                "{}",
                render_model_report(
                    &self.model,
                    self.runtime.session().messages.len(),
                    self.runtime.usage().turns(),
                )
            );
            return Ok(false);
        }

        let previous = self.model.clone();
        let session = self.runtime.session().clone();
        let message_count = session.messages.len();
        self.runtime = build_runtime(
            session,
            model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.model.clone_from(&model);
        self.cold_start_hint = matches!(api::detect_provider_kind(&self.model), api::ProviderKind::Ollama);
        println!(
            "{}",
            format_model_switch_report(&previous, &model, message_count)
        );
        Ok(true)
    }

    fn set_permissions(
        &mut self,
        mode: Option<String>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(mode) = mode else {
            println!(
                "{}",
                format_permissions_report(self.permission_mode.as_str())
            );
            return Ok(false);
        };

        let normalized = normalize_permission_mode(&mode).ok_or_else(|| {
            format!(
                "unsupported permission mode '{mode}'. Use read-only, workspace-write, or danger-full-access."
            )
        })?;

        if normalized == self.permission_mode.as_str() {
            println!("{}", format_permissions_report(normalized));
            return Ok(false);
        }

        let previous = self.permission_mode.as_str().to_string();
        let session = self.runtime.session().clone();
        self.permission_mode = permission_mode_from_label(normalized);
        self.runtime = build_runtime(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        println!(
            "{}",
            format_permissions_switch_report(&previous, normalized)
        );
        Ok(true)
    }

    fn clear_session(&mut self, confirm: bool) -> Result<bool, Box<dyn std::error::Error>> {
        if !confirm {
            println!(
                "clear: confirmation required; run /clear --confirm to start a fresh session."
            );
            return Ok(false);
        }

        self.session = create_managed_session_handle()?;
        self.runtime = build_runtime(
            Session::new(),
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        println!(
            "Session cleared\n  Mode             fresh session\n  Preserved model  {}\n  Permission mode  {}\n  Session          {}",
            self.model,
            self.permission_mode.as_str(),
            self.session.id,
        );
        Ok(true)
    }

    fn print_cost(&self) {
        let cumulative = self.runtime.usage().cumulative_usage();
        println!("{}", format_cost_report(cumulative));
    }

    fn resume_session(
        &mut self,
        session_path: Option<String>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(session_ref) = session_path else {
            println!("Usage: /resume <session-path>");
            return Ok(false);
        };

        let handle = resolve_session_reference(&session_ref)?;
        let session = Session::load_from_path(&handle.path)?;
        let message_count = session.messages.len();
        self.runtime = build_runtime(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.session = handle;
        println!(
            "{}",
            format_resume_report(
                &self.session.path.display().to_string(),
                message_count,
                self.runtime.usage().turns(),
            )
        );
        Ok(true)
    }

    fn print_config(section: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_config_report(section)?);
        Ok(())
    }

    fn print_memory() -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_memory_report()?);
        Ok(())
    }

    fn print_agents(args: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        // "/agents status" shows running background agents
        if args.map(str::trim) == Some("status") {
            let tasks = load_task_manifests(&env::current_dir()?)?;
            if tasks.is_empty() {
                println!("\x1b[2mNo background agents.\x1b[0m");
                return Ok(());
            }
            println!("\x1b[1mBackground agents\x1b[0m ({} total)", tasks.len());
            for task in &tasks {
                println!(
                    "  [{label:<4}] {id:<12} \x1b[2m{status:<10}\x1b[0m {desc:.50}",
                    label = task_status_label(task.status()),
                    id = shorten_task_id(task.id()),
                    status = task.status(),
                    desc = task.description(),
                );
            }
            return Ok(());
        }

        let cwd = env::current_dir()?;
        println!("{}", handle_agents_slash_command(args, &cwd)?);
        Ok(())
    }

    fn print_tasks(
        args: Option<&str>,
        current_session_id: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        let tasks = load_task_manifests(&cwd)?;
        let command = args.map(str::trim).filter(|value| !value.is_empty()).unwrap_or("list");
        let mut parts = command.split_whitespace();
        match parts.next().unwrap_or("list") {
            "" | "list" => println!("{}", render_task_list_report(&tasks, current_session_id)),
            "show" | "inspect" => {
                let Some(task_id) = parts.next() else {
                    println!("Usage: /tasks show <task-id>");
                    return Ok(());
                };
                let task = find_task_by_prefix(&tasks, task_id)?;
                println!("{}", render_task_show_report(task, current_session_id));
            }
            "logs" => {
                let Some(task_id) = parts.next() else {
                    println!("Usage: /tasks logs <task-id>");
                    return Ok(());
                };
                let task = find_task_by_prefix(&tasks, task_id)?;
                println!("{}", render_task_logs_report(task)?);
            }
            "attach" => {
                let Some(task_id) = parts.next() else {
                    println!("Usage: /tasks attach <task-id>");
                    return Ok(());
                };
                attach_to_task(&cwd, task_id)?;
            }
            "stop" => {
                let Some(task_id) = parts.next() else {
                    println!("Usage: /tasks stop <task-id>");
                    return Ok(());
                };
                println!("{}", request_task_stop(&cwd, task_id)?);
            }
            other => {
                println!(
                    "Tasks\n  Unsupported      {other}\n  Try              /tasks list | /tasks show <id> | /tasks logs <id> | /tasks attach <id> | /tasks stop <id>"
                );
            }
        }
        Ok(())
    }

    fn print_skills(args: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        println!("{}", handle_skills_slash_command(args, &cwd)?);
        Ok(())
    }

    fn print_diff() -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_diff_report()?);
        Ok(())
    }

    fn print_version() {
        println!("{}", render_version_report());
    }

    fn export_session(
        &self,
        requested_path: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let export_path = resolve_export_path(requested_path, self.runtime.session())?;
        fs::write(&export_path, render_export_text(self.runtime.session()))?;
        println!(
            "Export\n  Result           wrote transcript\n  File             {}\n  Messages         {}",
            export_path.display(),
            self.runtime.session().messages.len(),
        );
        Ok(())
    }

    fn handle_session_command(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match action {
            None | Some("list") => {
                println!("{}", render_session_list(&self.session.id)?);
                Ok(false)
            }
            Some("switch") => {
                let Some(target) = target else {
                    println!("Usage: /session switch <session-id>");
                    return Ok(false);
                };
                let handle = resolve_session_reference(target)?;
                let session = Session::load_from_path(&handle.path)?;
                let message_count = session.messages.len();
                self.runtime = build_runtime(
                    session,
                    self.model.clone(),
                    self.system_prompt.clone(),
                    true,
                    true,
                    self.allowed_tools.clone(),
                    self.permission_mode,
                    None,
                )?;
                self.session = handle;
                println!(
                    "Session switched\n  Active session   {}\n  File             {}\n  Messages         {}",
                    self.session.id,
                    self.session.path.display(),
                    message_count,
                );
                Ok(true)
            }
            Some("search") => {
                let query = target.unwrap_or("").trim();
                if query.is_empty() {
                    println!("Usage: /session search <query>");
                    return Ok(false);
                }
                let sessions = list_managed_sessions()?;
                let mut found = 0usize;
                let query_lower = query.to_ascii_lowercase();
                for summary in &sessions {
                    if let Ok(session) = Session::load_from_path(&summary.path) {
                        for (index, message) in session.messages.iter().enumerate() {
                            for block in &message.blocks {
                                let text = match block {
                                    ContentBlock::Text { text } => text.as_str(),
                                    ContentBlock::ToolResult { output, .. } => output.as_str(),
                                    _ => continue,
                                };
                                if text.to_ascii_lowercase().contains(&query_lower) {
                                    if found == 0 {
                                        println!("\x1b[1mSession search results for\x1b[0m \x1b[33m{query}\x1b[0m\n");
                                    }
                                    found += 1;
                                    let role = match message.role {
                                        runtime::MessageRole::User => "user",
                                        runtime::MessageRole::Assistant => "assistant",
                                        runtime::MessageRole::System => "system",
                                        runtime::MessageRole::Tool => "tool",
                                    };
                                    let preview = text
                                        .lines()
                                        .find(|line| line.to_ascii_lowercase().contains(&query_lower))
                                        .unwrap_or(text)
                                        .trim();
                                    let preview = if preview.len() > 120 {
                                        format!("{}...", &preview[..120])
                                    } else {
                                        preview.to_string()
                                    };
                                    println!(
                                        "  \x1b[36m{}\x1b[0m msg#{} \x1b[2m({role})\x1b[0m {}",
                                        summary.id, index, preview
                                    );
                                    if found >= 20 {
                                        println!("\x1b[2m  ... (showing first 20 matches)\x1b[0m");
                                        return Ok(false);
                                    }
                                    break; // one match per message
                                }
                            }
                        }
                    }
                }
                if found == 0 {
                    println!("No matches found for '{query}' across {} session(s).", sessions.len());
                } else {
                    println!("\n\x1b[2m{found} match(es) across {} session(s)\x1b[0m", sessions.len());
                }
                Ok(false)
            }
            Some(other) => {
                println!("Unknown /session action '{other}'. Use: /session list | switch <id> | search <query>");
                Ok(false)
            }
        }
    }

    fn handle_plugins_command(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        let loader = ConfigLoader::default_for(&cwd);
        let runtime_config = loader.load()?;
        let mut manager = build_plugin_manager(&cwd, &loader, &runtime_config);
        let result = handle_plugins_slash_command(action, target, &mut manager)?;
        println!("{}", result.message);
        if result.reload_runtime {
            self.reload_runtime_features()?;
        }
        Ok(false)
    }

    fn reload_runtime_features(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime = build_runtime(
            self.runtime.session().clone(),
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.persist_session()
    }

    fn compact(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let result = self.runtime.compact(CompactionConfig::default());
        let removed = result.removed_message_count;
        let kept = result.compacted_session.messages.len();
        let skipped = removed == 0;
        self.runtime = build_runtime(
            result.compacted_session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.persist_session()?;
        println!("{}", format_compact_report(removed, kept, skipped));
        Ok(())
    }

    fn run_internal_prompt_text_with_progress(
        &self,
        prompt: &str,
        enable_tools: bool,
        progress: Option<InternalPromptProgressReporter>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let session = self.runtime.session().clone();
        let mut runtime = build_runtime(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            enable_tools,
            false,
            self.allowed_tools.clone(),
            self.permission_mode,
            progress,
        )?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let summary = runtime.run_turn(prompt, Some(&mut permission_prompter))?;
        Ok(final_assistant_text(&summary).trim().to_string())
    }

    fn run_internal_prompt_text(
        &self,
        prompt: &str,
        enable_tools: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.run_internal_prompt_text_with_progress(prompt, enable_tools, None)
    }

    fn run_bughunter(&self, scope: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let scope = scope.unwrap_or("the current repository");
        let prompt = format!(
            "You are /bughunter. Inspect {scope} and identify the most likely bugs or correctness issues. Prioritize concrete findings with file paths, severity, and suggested fixes. Use tools if needed."
        );
        println!("{}", self.run_internal_prompt_text(&prompt, true)?);
        Ok(())
    }

    fn run_ultraplan(&self, task: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let task = task.unwrap_or("the current repo work");
        let prompt = format!(
            "You are /ultraplan. Produce a deep multi-step execution plan for {task}. Include goals, risks, implementation sequence, verification steps, and rollback considerations. Use tools if needed."
        );
        let mut progress = InternalPromptProgressRun::start_ultraplan(task);
        match self.run_internal_prompt_text_with_progress(&prompt, true, Some(progress.reporter()))
        {
            Ok(plan) => {
                progress.finish_success();
                println!("{plan}");
                Ok(())
            }
            Err(error) => {
                progress.finish_failure(&error.to_string());
                Err(error)
            }
        }
    }

    #[allow(clippy::unused_self)]
    fn run_teleport(&self, target: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) else {
            println!("Usage: /teleport <symbol-or-path>");
            return Ok(());
        };

        println!("{}", render_teleport_report(target)?);
        Ok(())
    }

    fn run_debug_tool_call(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_last_tool_debug_report(self.runtime.session())?);
        Ok(())
    }

    fn run_commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let status = git_output(&["status", "--short"])?;
        if status.trim().is_empty() {
            println!("Commit\n  Result           skipped\n  Reason           no workspace changes");
            return Ok(());
        }

        git_status_ok(&["add", "-A"])?;
        let staged_stat = git_output(&["diff", "--cached", "--stat"])?;
        let prompt = format!(
            "Generate a git commit message in plain text Lore format only. Base it on this staged diff summary:\n\n{}\n\nRecent conversation context:\n{}",
            truncate_for_prompt(&staged_stat, 8_000),
            recent_user_context(self.runtime.session(), 6)
        );
        let message = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        if message.trim().is_empty() {
            return Err("generated commit message was empty".into());
        }

        let path = write_temp_text_file("ember-commit-message.txt", &message)?;
        let output = Command::new("git")
            .args(["commit", "--file"])
            .arg(&path)
            .current_dir(env::current_dir()?)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("git commit failed: {stderr}").into());
        }

        println!(
            "Commit\n  Result           created\n  Message file     {}\n\n{}",
            path.display(),
            message.trim()
        );
        Ok(())
    }

    fn run_pr(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let staged = git_output(&["diff", "--stat"])?;
        let prompt = format!(
            "Generate a pull request title and body from this conversation and diff summary. Output plain text in this format exactly:\nTITLE: <title>\nBODY:\n<body markdown>\n\nContext hint: {}\n\nDiff summary:\n{}",
            context.unwrap_or("none"),
            truncate_for_prompt(&staged, 10_000)
        );
        let draft = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        let (title, body) = parse_titled_body(&draft)
            .ok_or_else(|| "failed to parse generated PR title/body".to_string())?;

        if command_exists("gh") {
            let body_path = write_temp_text_file("ember-pr-body.md", &body)?;
            let output = Command::new("gh")
                .args(["pr", "create", "--title", &title, "--body-file"])
                .arg(&body_path)
                .current_dir(env::current_dir()?)
                .output()?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!(
                    "PR\n  Result           created\n  Title            {title}\n  URL              {}",
                    if stdout.is_empty() { "<unknown>" } else { &stdout }
                );
                return Ok(());
            }
        }

        println!("PR draft\n  Title            {title}\n\n{body}");
        Ok(())
    }

    fn run_issue(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let prompt = format!(
            "Generate a GitHub issue title and body from this conversation. Output plain text in this format exactly:\nTITLE: <title>\nBODY:\n<body markdown>\n\nContext hint: {}\n\nConversation context:\n{}",
            context.unwrap_or("none"),
            truncate_for_prompt(&recent_user_context(self.runtime.session(), 10), 10_000)
        );
        let draft = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        let (title, body) = parse_titled_body(&draft)
            .ok_or_else(|| "failed to parse generated issue title/body".to_string())?;

        if command_exists("gh") {
            let body_path = write_temp_text_file("ember-issue-body.md", &body)?;
            let output = Command::new("gh")
                .args(["issue", "create", "--title", &title, "--body-file"])
                .arg(&body_path)
                .current_dir(env::current_dir()?)
                .output()?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!(
                    "Issue\n  Result           created\n  Title            {title}\n  URL              {}",
                    if stdout.is_empty() { "<unknown>" } else { &stdout }
                );
                return Ok(());
            }
        }

        println!("Issue draft\n  Title            {title}\n\n{body}");
        Ok(())
    }
}

fn sessions_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let path = cwd.join(".ember").join("sessions");
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn create_managed_session_handle() -> Result<SessionHandle, Box<dyn std::error::Error>> {
    let id = generate_session_id();
    let path = sessions_dir()?.join(format!("{id}.json"));
    Ok(SessionHandle { id, path })
}

fn generate_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("session-{millis}")
}

fn resolve_session_reference(reference: &str) -> Result<SessionHandle, Box<dyn std::error::Error>> {
    let direct = PathBuf::from(reference);
    let path = if direct.exists() {
        direct
    } else {
        sessions_dir()?.join(format!("{reference}.json"))
    };
    if !path.exists() {
        return Err(format!("session not found: {reference}").into());
    }
    let id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(reference)
        .to_string();
    Ok(SessionHandle { id, path })
}

fn list_managed_sessions() -> Result<Vec<ManagedSessionSummary>, Box<dyn std::error::Error>> {
    let mut sessions = Vec::new();
    for entry in fs::read_dir(sessions_dir()?)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified_epoch_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let message_count = Session::load_from_path(&path)
            .map(|session| session.messages.len())
            .unwrap_or_default();
        let id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown")
            .to_string();
        sessions.push(ManagedSessionSummary {
            id,
            path,
            modified_epoch_secs,
            message_count,
        });
    }
    sessions.sort_by(|left, right| right.modified_epoch_secs.cmp(&left.modified_epoch_secs));
    Ok(sessions)
}

fn format_relative_timestamp(epoch_secs: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(epoch_secs);
    let elapsed = now.saturating_sub(epoch_secs);
    match elapsed {
        0..=59 => format!("{elapsed}s ago"),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

fn render_session_list(active_session_id: &str) -> Result<String, Box<dyn std::error::Error>> {
    let sessions = list_managed_sessions()?;
    let mut lines = vec![
        "Sessions".to_string(),
        format!("  Directory         {}", sessions_dir()?.display()),
    ];
    if sessions.is_empty() {
        lines.push("  No managed sessions saved yet.".to_string());
        return Ok(lines.join("\n"));
    }
    for session in sessions {
        let marker = if session.id == active_session_id {
            "[current]"
        } else {
            "[saved]"
        };
        lines.push(format!(
            "  {id:<20} {marker:<10} {msgs:>3} msgs | updated {modified}",
            id = session.id,
            msgs = session.message_count,
            modified = format_relative_timestamp(session.modified_epoch_secs),
        ));
        lines.push(format!("    {}", session.path.display()));
    }
    Ok(lines.join("\n"))
}

fn render_repl_help() -> String {
    [
        "Interactive REPL".to_string(),
        "  Quick start          Ask a task in plain English or use one of the core commands below."
            .to_string(),
        "  Core commands        /help | /status | /doctor | /model | /permissions".to_string(),
        "  Quick checks         /doctor quick | /doctor full (cached)".to_string(),
        "  Background tasks     /tasks list | /tasks attach <id> | /tasks logs <id>".to_string(),
        "  Exit                 /exit or /quit".to_string(),
        "  Vim mode             /vim toggles modal editing".to_string(),
        "  History              Up/Down recalls previous prompts".to_string(),
        "  Completion           Tab cycles slash command matches".to_string(),
        "  Cancel               Ctrl-C clears input (or exits on an empty prompt)".to_string(),
        "  Multiline            Shift+Enter or Ctrl+J inserts a newline".to_string(),
        String::new(),
        render_slash_command_help(),
    ]
    .join(
        "
",
    )
}

fn append_slash_command_suggestions(lines: &mut Vec<String>, name: &str) {
    let suggestions = suggest_slash_commands(name, 3);
    if suggestions.is_empty() {
        lines.push("  Try              /help shows the full slash command map".to_string());
        return;
    }

    lines.push("  Try              /help shows the full slash command map".to_string());
    lines.push("Suggestions".to_string());
    lines.extend(
        suggestions
            .into_iter()
            .map(|suggestion| format!("  {suggestion}")),
    );
}

fn render_unknown_repl_command(name: &str) -> String {
    let mut lines = vec![
        "Unknown slash command".to_string(),
        format!("  Command          /{name}"),
    ];
    append_repl_command_suggestions(&mut lines, name);
    lines.join("\n")
}

fn append_repl_command_suggestions(lines: &mut Vec<String>, name: &str) {
    let suggestions = suggest_repl_commands(name);
    if suggestions.is_empty() {
        lines.push("  Try              /help shows the full slash command map".to_string());
        return;
    }

    lines.push("  Try              /help shows the full slash command map".to_string());
    lines.push("Suggestions".to_string());
    lines.extend(
        suggestions
            .into_iter()
            .map(|suggestion| format!("  {suggestion}")),
    );
}

fn render_mode_unavailable(command: &str, label: &str) -> String {
    [
        "Command unavailable in this REPL mode".to_string(),
        format!("  Command          /{command}"),
        format!("  Feature          {label}"),
        "  Tip              Use /help to find currently wired REPL commands".to_string(),
    ]
    .join("\n")
}

fn normalize_builtin_query(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut previous_was_space = false;

    for ch in input.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            ' '
        };

        if mapped == ' ' {
            if !previous_was_space {
                normalized.push(' ');
                previous_was_space = true;
            }
        } else {
            normalized.push(mapped);
            previous_was_space = false;
        }
    }

    normalized.trim().to_string()
}

fn is_builtin_status_query(input: &str) -> bool {
    matches!(
        normalize_builtin_query(input).as_str(),
        "status"
            | "project status"
            | "workspace status"
            | "current project status"
            | "current workspace status"
            | "check project status"
            | "check workspace status"
            | "check the project status"
            | "check the workspace status"
            | "check current project status"
            | "check current workspace status"
            | "check the current project status"
            | "check the current workspace status"
            | "show project status"
            | "show workspace status"
            | "show current project status"
            | "show current workspace status"
            | "show the current project status"
            | "show the current workspace status"
            | "what is the current project status"
            | "what is the current workspace status"
            | "what s the current project status"
            | "what s the current workspace status"
            | "whats the current project status"
            | "whats the current workspace status"
    )
}

fn status_context(
    session_path: Option<&Path>,
) -> Result<StatusContext, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let discovered_config_files = loader.discover().len();
    let runtime_config = loader.load()?;
    let project_context = ProjectContext::discover_with_git(&cwd, DEFAULT_DATE)?;
    let (project_root, git_branch) =
        parse_git_status_metadata(project_context.git_status.as_deref());
    Ok(StatusContext {
        cwd,
        session_path: session_path.map(Path::to_path_buf),
        loaded_config_files: runtime_config.loaded_entries().len(),
        discovered_config_files,
        memory_file_count: project_context.instruction_files.len(),
        project_root,
        git_branch,
    })
}

fn format_status_report(
    model: &str,
    usage: StatusUsage,
    permission_mode: &str,
    context: &StatusContext,
) -> String {
    [
        format!(
            "Session
  Model            {model}
  Permissions      {permission_mode}
    Activity         {} messages | {} turns
    Tokens           est {} | latest {} | total {}",
            usage.message_count,
            usage.turns,
            usage.estimated_tokens,
            usage.latest.total_tokens(),
            usage.cumulative.total_tokens(),
        ),
        format!(
            "Usage
  Cumulative input {}
  Cumulative output {}
  Cache create     {}
  Cache read       {}",
            usage.cumulative.input_tokens,
            usage.cumulative.output_tokens,
            usage.cumulative.cache_creation_input_tokens,
            usage.cumulative.cache_read_input_tokens,
        ),
        format!(
            "Workspace
  Folder           {}
  Project root     {}
  Git branch       {}
  Session file     {}
  Config files     loaded {}/{}
  Memory files     {}

Next
  /help            Browse commands
  /session list    Inspect saved sessions
  /diff            Review current workspace changes",
            context.cwd.display(),
            context
                .project_root
                .as_ref()
                .map_or_else(|| "unknown".to_string(), |path| path.display().to_string()),
            context.git_branch.as_deref().unwrap_or("unknown"),
            context.session_path.as_ref().map_or_else(
                || "live-repl".to_string(),
                |path| path.display().to_string()
            ),
            context.loaded_config_files,
            context.discovered_config_files,
            context.memory_file_count,
        ),
    ]
    .join(
        "

",
    )
}

fn render_config_report(section: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let discovered = loader.discover();
    let runtime_config = loader.load()?;

    let mut lines = vec![
        format!(
            "Config
  Working directory {}
  Loaded files      {}
  Merged keys       {}",
            cwd.display(),
            runtime_config.loaded_entries().len(),
            runtime_config.merged().len()
        ),
        "Discovered files".to_string(),
    ];
    for entry in discovered {
        let source = match entry.source {
            ConfigSource::User => "user",
            ConfigSource::Project => "project",
            ConfigSource::Local => "local",
        };
        let status = if runtime_config
            .loaded_entries()
            .iter()
            .any(|loaded_entry| loaded_entry.path == entry.path)
        {
            "loaded"
        } else {
            "missing"
        };
        lines.push(format!(
            "  {source:<7} {status:<7} {}",
            entry.path.display()
        ));
    }

    if let Some(section) = section {
        lines.push(format!("Merged section: {section}"));
        let value = match section {
            "env" => runtime_config.get("env"),
            "hooks" => runtime_config.get("hooks"),
            "model" => runtime_config.get("model"),
            "ui" => runtime_config.get("ui"),
            "plugins" => runtime_config
                .get("plugins")
                .or_else(|| runtime_config.get("enabledPlugins")),
            other => {
                lines.push(format!(
                    "  Unsupported config section '{other}'. Use env, hooks, model, ui, or plugins."
                ));
                return Ok(lines.join(
                    "
",
                ));
            }
        };
        lines.push(format!(
            "  {}",
            match value {
                Some(value) => value.render(),
                None => "<unset>".to_string(),
            }
        ));
        return Ok(lines.join(
            "
",
        ));
    }

    lines.push("Merged JSON".to_string());
    lines.push(format!("  {}", runtime_config.as_json().render()));
    Ok(lines.join(
        "
",
    ))
}

fn render_memory_report() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let project_context = ProjectContext::discover(&cwd, DEFAULT_DATE)?;
    let mut lines = vec![format!(
        "Memory
  Working directory {}
  Instruction files {}",
        cwd.display(),
        project_context.instruction_files.len()
    )];
    if project_context.instruction_files.is_empty() {
        lines.push("Discovered files".to_string());
        lines.push(
            "  No EMBER.md (or legacy CLAW.md) instruction files were discovered in the current directory ancestry.".to_string(),
        );
    } else {
        lines.push("Discovered files".to_string());
        for (index, file) in project_context.instruction_files.iter().enumerate() {
            let preview = file.content.lines().next().unwrap_or("").trim();
            let preview = if preview.is_empty() {
                "<empty>"
            } else {
                preview
            };
            lines.push(format!("  {}. {}", index + 1, file.path.display(),));
            lines.push(format!(
                "     lines={} preview={}",
                file.content.lines().count(),
                preview
            ));
        }
    }
    Ok(lines.join(
        "
",
    ))
}

fn init_ember_md() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    Ok(initialize_repo(&cwd)?.render())
}

fn run_init() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", init_ember_md()?);
    Ok(())
}

fn normalize_permission_mode(mode: &str) -> Option<&'static str> {
    match mode.trim() {
        "read-only" => Some("read-only"),
        "workspace-write" => Some("workspace-write"),
        "danger-full-access" => Some("danger-full-access"),
        _ => None,
    }
}

fn render_diff_report() -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["diff", "--", ":(exclude).omx"])
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git diff failed: {stderr}").into());
    }
    let diff = String::from_utf8(output.stdout)?;
    if diff.trim().is_empty() {
        return Ok(
            "Diff\n  Result           clean working tree\n  Detail           no current changes"
                .to_string(),
        );
    }
    Ok(format!("Diff\n\n{}", diff.trim_end()))
}

fn render_teleport_report(target: &str) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;

    let file_list = Command::new("rg")
        .args(["--files"])
        .current_dir(&cwd)
        .output()?;
    let file_matches = if file_list.status.success() {
        String::from_utf8(file_list.stdout)?
            .lines()
            .filter(|line| line.contains(target))
            .take(10)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let content_output = Command::new("rg")
        .args(["-n", "-S", "--color", "never", target, "."])
        .current_dir(&cwd)
        .output()?;

    let mut lines = vec![format!("Teleport\n  Target           {target}")];
    if !file_matches.is_empty() {
        lines.push(String::new());
        lines.push("File matches".to_string());
        lines.extend(file_matches.into_iter().map(|path| format!("  {path}")));
    }

    if content_output.status.success() {
        let matches = String::from_utf8(content_output.stdout)?;
        if !matches.trim().is_empty() {
            lines.push(String::new());
            lines.push("Content matches".to_string());
            lines.push(truncate_for_prompt(&matches, 4_000));
        }
    }

    if lines.len() == 1 {
        lines.push("  Result           no matches found".to_string());
    }

    Ok(lines.join("\n"))
}

fn render_last_tool_debug_report(session: &Session) -> Result<String, Box<dyn std::error::Error>> {
    let last_tool_use = session
        .messages
        .iter()
        .rev()
        .find_map(|message| {
            message.blocks.iter().rev().find_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
        })
        .ok_or_else(|| "no prior tool call found in session".to_string())?;

    let tool_result = session.messages.iter().rev().find_map(|message| {
        message.blocks.iter().rev().find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } if tool_use_id == &last_tool_use.0 => {
                Some((tool_name.clone(), output.clone(), *is_error))
            }
            _ => None,
        })
    });

    let mut lines = vec![
        "Debug tool call".to_string(),
        format!("  Tool id          {}", last_tool_use.0),
        format!("  Tool name        {}", last_tool_use.1),
        "  Input".to_string(),
        indent_block(&last_tool_use.2, 4),
    ];

    match tool_result {
        Some((tool_name, output, is_error)) => {
            lines.push("  Result".to_string());
            lines.push(format!("    name           {tool_name}"));
            lines.push(format!(
                "    status         {}",
                if is_error { "error" } else { "ok" }
            ));
            lines.push(indent_block(&output, 4));
        }
        None => lines.push("  Result           missing tool result".to_string()),
    }

    Ok(lines.join("\n"))
}

fn indent_block(value: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn git_output(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_status_ok(args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(())
}

fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn write_temp_text_file(
    filename: &str,
    contents: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = env::temp_dir().join(filename);
    fs::write(&path, contents)?;
    Ok(path)
}

fn recent_user_context(session: &Session, limit: usize) -> String {
    let requests = session
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .filter_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.trim().to_string()),
                _ => None,
            })
        })
        .rev()
        .take(limit)
        .collect::<Vec<_>>();

    if requests.is_empty() {
        "<no prior user messages>".to_string()
    } else {
        requests
            .into_iter()
            .rev()
            .enumerate()
            .map(|(index, text)| format!("{}. {}", index + 1, text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn truncate_for_prompt(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.trim().to_string()
    } else {
        let truncated = value.chars().take(limit).collect::<String>();
        format!("{}\n...[truncated]", truncated.trim_end())
    }
}

fn sanitize_generated_message(value: &str) -> String {
    value.trim().trim_matches('`').trim().replace("\r\n", "\n")
}

fn parse_titled_body(value: &str) -> Option<(String, String)> {
    let normalized = sanitize_generated_message(value);
    let title = normalized
        .lines()
        .find_map(|line| line.strip_prefix("TITLE:").map(str::trim))?;
    let body_start = normalized.find("BODY:")?;
    let body = normalized[body_start + "BODY:".len()..].trim();
    Some((title.to_string(), body.to_string()))
}

fn render_version_report() -> String {
    let git_sha = GIT_SHA.unwrap_or("unknown");
    let target = BUILD_TARGET.unwrap_or("unknown");
    format!(
        "Emberforge\n  Version          {VERSION}\n  Git SHA          {git_sha}\n  Target           {target}\n  Build date       {DEFAULT_DATE}\n\nSupport\n  Help             ember --help\n  REPL             /help"
    )
}

fn render_export_text(session: &Session) -> String {
    let mut lines = vec!["# Conversation Export".to_string(), String::new()];
    for (index, message) in session.messages.iter().enumerate() {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        lines.push(format!("## {}. {role}", index + 1));
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => lines.push(text.clone()),
                ContentBlock::ToolUse { id, name, input } => {
                    lines.push(format!("[tool_use id={id} name={name}] {input}"));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    tool_name,
                    output,
                    is_error,
                } => {
                    lines.push(format!(
                        "[tool_result id={tool_use_id} name={tool_name} error={is_error}] {output}"
                    ));
                }
            }
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

fn default_export_filename(session: &Session) -> String {
    let stem = session
        .messages
        .iter()
        .find_map(|message| match message.role {
            MessageRole::User => message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .map_or("conversation", |text| {
            text.lines().next().unwrap_or("conversation")
        })
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    let fallback = if stem.is_empty() {
        "conversation"
    } else {
        &stem
    };
    format!("{fallback}.txt")
}

fn resolve_export_path(
    requested_path: Option<&str>,
    session: &Session,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let file_name =
        requested_path.map_or_else(|| default_export_filename(session), ToOwned::to_owned);
    let final_name = if Path::new(&file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
    {
        file_name
    } else {
        format!("{file_name}.txt")
    };
    Ok(cwd.join(final_name))
}

pub(crate) fn build_system_prompt() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let mut sections = load_system_prompt(&cwd, DEFAULT_DATE, env::consts::OS, "unknown")?;

    // Inject dynamic context: codebase map, project rules, README.
    let snippets = context::collect_session_context(&cwd);
    if let Some(context_section) = context::render_context_section(&snippets) {
        sections.push(context_section);
    }

    Ok(sections)
}

fn build_runtime_plugin_state(
) -> Result<(runtime::RuntimeFeatureConfig, GlobalToolRegistry), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader.load()?;
    let plugin_manager = build_plugin_manager(&cwd, &loader, &runtime_config);
    let tool_registry = GlobalToolRegistry::with_plugin_tools(plugin_manager.aggregated_tools()?)?;
    Ok((runtime_config.feature_config().clone(), tool_registry))
}

fn build_plugin_manager(
    cwd: &Path,
    loader: &ConfigLoader,
    runtime_config: &runtime::RuntimeConfig,
) -> PluginManager {
    let plugin_settings = runtime_config.plugins();
    let mut plugin_config = PluginManagerConfig::new(loader.config_home().to_path_buf());
    plugin_config.enabled_plugins = plugin_settings.enabled_plugins().clone();
    plugin_config.external_dirs = plugin_settings
        .external_directories()
        .iter()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path))
        .collect();
    plugin_config.install_root = plugin_settings
        .install_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.registry_path = plugin_settings
        .registry_path()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.bundled_root = plugin_settings
        .bundled_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    PluginManager::new(plugin_config)
}

fn resolve_plugin_path(cwd: &Path, config_home: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else if value.starts_with('.') {
        cwd.join(path)
    } else {
        config_home.join(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalPromptProgressState {
    command_label: &'static str,
    task_label: String,
    step: usize,
    phase: String,
    detail: Option<String>,
    saw_final_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalPromptProgressEvent {
    Started,
    Update,
    Heartbeat,
    Complete,
    Failed,
}

#[derive(Debug)]
struct InternalPromptProgressShared {
    state: Mutex<InternalPromptProgressState>,
    output_lock: Mutex<()>,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct InternalPromptProgressReporter {
    shared: Arc<InternalPromptProgressShared>,
}

#[derive(Debug)]
struct InternalPromptProgressRun {
    reporter: InternalPromptProgressReporter,
    heartbeat_stop: Option<mpsc::Sender<()>>,
    heartbeat_handle: Option<thread::JoinHandle<()>>,
}

impl InternalPromptProgressReporter {
    fn ultraplan(task: &str) -> Self {
        Self {
            shared: Arc::new(InternalPromptProgressShared {
                state: Mutex::new(InternalPromptProgressState {
                    command_label: "Ultraplan",
                    task_label: task.to_string(),
                    step: 0,
                    phase: "planning started".to_string(),
                    detail: Some(format!("task: {task}")),
                    saw_final_text: false,
                }),
                output_lock: Mutex::new(()),
                started_at: Instant::now(),
            }),
        }
    }

    fn emit(&self, event: InternalPromptProgressEvent, error: Option<&str>) {
        let snapshot = self.snapshot();
        let line = format_internal_prompt_progress_line(event, &snapshot, self.elapsed(), error);
        self.write_line(&line);
    }

    fn mark_model_phase(&self) {
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = if state.step == 1 {
                "analyzing request".to_string()
            } else {
                "reviewing findings".to_string()
            };
            state.detail = Some(format!("task: {}", state.task_label));
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_tool_phase(&self, name: &str, input: &str) {
        let detail = describe_tool_progress(name, input);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = format!("running {name}");
            state.detail = Some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_text_phase(&self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let detail = truncate_for_summary(first_visible_line(trimmed), 120);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            if state.saw_final_text {
                return;
            }
            state.saw_final_text = true;
            state.step += 1;
            state.phase = "drafting final plan".to_string();
            state.detail = (!detail.is_empty()).then_some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn emit_heartbeat(&self) {
        let snapshot = self.snapshot();
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Heartbeat,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn snapshot(&self) -> InternalPromptProgressState {
        self.shared
            .state
            .lock()
            .expect("internal prompt progress state poisoned")
            .clone()
    }

    fn elapsed(&self) -> Duration {
        self.shared.started_at.elapsed()
    }

    fn write_line(&self, line: &str) {
        let _guard = self
            .shared
            .output_lock
            .lock()
            .expect("internal prompt progress output lock poisoned");
        let mut stdout = io::stdout();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

impl InternalPromptProgressRun {
    fn start_ultraplan(task: &str) -> Self {
        let reporter = InternalPromptProgressReporter::ultraplan(task);
        reporter.emit(InternalPromptProgressEvent::Started, None);

        let (heartbeat_stop, heartbeat_rx) = mpsc::channel();
        let heartbeat_reporter = reporter.clone();
        let heartbeat_handle = thread::spawn(move || loop {
            match heartbeat_rx.recv_timeout(INTERNAL_PROGRESS_HEARTBEAT_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => heartbeat_reporter.emit_heartbeat(),
            }
        });

        Self {
            reporter,
            heartbeat_stop: Some(heartbeat_stop),
            heartbeat_handle: Some(heartbeat_handle),
        }
    }

    fn reporter(&self) -> InternalPromptProgressReporter {
        self.reporter.clone()
    }

    fn finish_success(&mut self) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Complete, None);
    }

    fn finish_failure(&mut self, error: &str) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Failed, Some(error));
    }

    fn stop_heartbeat(&mut self) {
        if let Some(sender) = self.heartbeat_stop.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.heartbeat_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for InternalPromptProgressRun {
    fn drop(&mut self) {
        self.stop_heartbeat();
    }
}

fn format_internal_prompt_progress_line(
    event: InternalPromptProgressEvent,
    snapshot: &InternalPromptProgressState,
    elapsed: Duration,
    error: Option<&str>,
) -> String {
    let elapsed_seconds = elapsed.as_secs();
    let step_label = if snapshot.step == 0 {
        "current step pending".to_string()
    } else {
        format!("current step {}", snapshot.step)
    };
    let mut status_bits = vec![step_label, format!("phase {}", snapshot.phase)];
    if let Some(detail) = snapshot
        .detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
    {
        status_bits.push(detail.to_string());
    }
    let status = status_bits.join(" | ");
    match event {
        InternalPromptProgressEvent::Started => {
            format!(
                "[plan] {} status | planning started | {status}",
                snapshot.command_label
            )
        }
        InternalPromptProgressEvent::Update => {
            format!("[plan] {} status | {status}", snapshot.command_label)
        }
        InternalPromptProgressEvent::Heartbeat => format!(
            "[plan] {} heartbeat | {elapsed_seconds}s elapsed | {status}",
            snapshot.command_label
        ),
        InternalPromptProgressEvent::Complete => format!(
            "[done] {} status | completed | {elapsed_seconds}s elapsed | {} steps total",
            snapshot.command_label, snapshot.step
        ),
        InternalPromptProgressEvent::Failed => format!(
            "[failed] {} status | failed | {elapsed_seconds}s elapsed | {}",
            snapshot.command_label,
            error.unwrap_or("unknown error")
        ),
    }
}

fn describe_tool_progress(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));
    match name {
        "bash" | "Bash" => {
            let command = parsed
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if command.is_empty() {
                "running shell command".to_string()
            } else {
                format!("command {}", truncate_for_summary(command.trim(), 100))
            }
        }
        "read_file" | "Read" => format!("reading {}", extract_tool_path(&parsed)),
        "write_file" | "Write" => format!("writing {}", extract_tool_path(&parsed)),
        "edit_file" | "Edit" => format!("editing {}", extract_tool_path(&parsed)),
        "glob_search" | "Glob" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("glob `{pattern}` in {scope}")
        }
        "grep_search" | "Grep" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("grep `{pattern}` in {scope}")
        }
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .map_or_else(
                || "running web search".to_string(),
                |query| format!("query {}", truncate_for_summary(query, 100)),
            ),
        _ => {
            let summary = summarize_tool_payload(input);
            if summary.is_empty() {
                format!("running {name}")
            } else {
                format!("{name}: {summary}")
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_runtime(
    session: Session,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
) -> Result<ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>, Box<dyn std::error::Error>>
{
    let (feature_config, tool_registry) = build_runtime_plugin_state()?;
    Ok(ConversationRuntime::new_with_features(
        session,
        DefaultRuntimeClient::new(
            model,
            enable_tools,
            emit_output,
            allowed_tools.clone(),
            tool_registry.clone(),
            progress_reporter,
        )?,
        CliToolExecutor::new(allowed_tools.clone(), emit_output, tool_registry.clone()),
        permission_policy(permission_mode, &tool_registry),
        system_prompt,
        feature_config,
    )
    .with_max_iterations(32))
}

struct CliPermissionPrompter {
    current_mode: PermissionMode,
}

impl CliPermissionPrompter {
    fn new(current_mode: PermissionMode) -> Self {
        Self { current_mode }
    }
}

impl runtime::PermissionPrompter for CliPermissionPrompter {
    fn decide(
        &mut self,
        request: &runtime::PermissionRequest,
    ) -> runtime::PermissionPromptDecision {
        println!();
        println!("Permission approval required");
        println!("  Tool             {}", request.tool_name);
        println!("  Current mode     {}", self.current_mode.as_str());
        println!("  Required mode    {}", request.required_mode.as_str());
        println!("  Input            {}", request.input);
        print!("Approve this tool call? [y/N]: ");
        let _ = io::stdout().flush();

        let mut response = String::new();
        match io::stdin().read_line(&mut response) {
            Ok(_) => {
                let normalized = response.trim().to_ascii_lowercase();
                if matches!(normalized.as_str(), "y" | "yes") {
                    runtime::PermissionPromptDecision::Allow
                } else {
                    runtime::PermissionPromptDecision::Deny {
                        reason: format!(
                            "tool '{}' denied by user approval prompt",
                            request.tool_name
                        ),
                    }
                }
            }
            Err(error) => runtime::PermissionPromptDecision::Deny {
                reason: format!("permission approval failed: {error}"),
            },
        }
    }
}

struct MachineReadablePermissionPrompter {
    output_format: CliOutputFormat,
}

impl MachineReadablePermissionPrompter {
    fn new(output_format: CliOutputFormat) -> Self {
        Self { output_format }
    }
}

impl runtime::PermissionPrompter for MachineReadablePermissionPrompter {
    fn decide(
        &mut self,
        request: &runtime::PermissionRequest,
    ) -> runtime::PermissionPromptDecision {
        runtime::PermissionPromptDecision::Deny {
            reason: format!(
                "tool '{}' requires approval to escalate from {} to {}; machine-readable {} mode cannot prompt interactively. Do not retry this tool in the current turn. Instead, answer the user with this denial reason or rerun with text output / a higher permission mode.",
                request.tool_name,
                request.current_mode.as_str(),
                request.required_mode.as_str(),
                self.output_format.as_str(),
            ),
        }
    }
}

struct DefaultRuntimeClient {
    runtime: tokio::runtime::Runtime,
    client: ProviderClient,
    prompt_cache: api::prompt_cache::PromptCache,
    model: String,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    progress_reporter: Option<InternalPromptProgressReporter>,
}

impl DefaultRuntimeClient {
    fn new(
        model: String,
        enable_tools: bool,
        emit_output: bool,
        allowed_tools: Option<AllowedToolSet>,
        tool_registry: GlobalToolRegistry,
        progress_reporter: Option<InternalPromptProgressReporter>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Use ProviderClient::from_model() which routes Ollama models to the
        // OpenAI-compat client (no API key needed) and Anthropic/xAI models
        // to their respective clients.
        let client = match ProviderClient::from_model(&model) {
            Ok(client) => client,
            Err(_) => {
                // Fall back to Anthropic client with explicit auth resolution
                let auth = resolve_cli_auth_source()?;
                ProviderClient::from_model_with_default_auth(&model, Some(auth))?
            }
        };

        // Create prompt cache for this session — caches completion responses
        // to avoid redundant API calls on identical requests (30s TTL).
        let session_id = format!("ember-{}", now_unix_millis());
        let prompt_cache = api::prompt_cache::PromptCache::new(&session_id);

        Ok(Self {
            runtime: tokio::runtime::Runtime::new()?,
            client,
            prompt_cache,
            model,
            enable_tools,
            emit_output,
            allowed_tools,
            tool_registry,
            progress_reporter,
        })
    }
}

fn now_unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

pub(crate) fn chrono_now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    OffsetDateTime::from_unix_timestamp(secs as i64)
        .ok()
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_else(|| format!("{secs}"))
}

fn resolve_cli_auth_source() -> Result<AuthSource, Box<dyn std::error::Error>> {
    Ok(resolve_startup_auth_source(|| {
        let cwd = env::current_dir().map_err(api::ApiError::from)?;
        let config = ConfigLoader::default_for(&cwd).load().map_err(|error| {
            api::ApiError::Auth(format!("failed to load runtime OAuth config: {error}"))
        })?;
        Ok(config.oauth().cloned())
    })?)
}

impl ApiClient for DefaultRuntimeClient {
    #[allow(clippy::too_many_lines)]
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if let Some(progress_reporter) = &self.progress_reporter {
            progress_reporter.mark_model_phase();
        }
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: max_tokens_for_model(&self.model),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n")),
            tools: self
                .enable_tools
                .then(|| filter_tool_specs(&self.tool_registry, self.allowed_tools.as_ref())),
            tool_choice: self.enable_tools.then_some(ToolChoice::Auto),
            stream: true,
        };

        // Check prompt cache for a recent identical request.
        if let Some(cached_response) = self.prompt_cache.lookup_completion(&message_request) {
            let mut events = Vec::new();
            for block in &cached_response.content {
                match block {
                    OutputContentBlock::Text { text } => {
                        if self.emit_output {
                            print!("{text}");
                        }
                        events.push(AssistantEvent::TextDelta(text.clone()));
                    }
                    OutputContentBlock::ToolUse { id, name, input } => {
                        events.push(AssistantEvent::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            events.push(AssistantEvent::Usage(runtime::TokenUsage {
                input_tokens: cached_response.usage.input_tokens,
                output_tokens: cached_response.usage.output_tokens,
                cache_creation_input_tokens: cached_response.usage.cache_creation_input_tokens,
                cache_read_input_tokens: cached_response.usage.cache_read_input_tokens,
            }));
            events.push(AssistantEvent::MessageStop);
            return Ok(events);
        }

        let prompt_cache = self.prompt_cache.clone();
        let message_request_for_cache = message_request.clone();

        self.runtime.block_on(async {
            let stream_result = self.client.stream_message(&message_request).await;

            // If the model doesn't support tools (Ollama 400 error), retry without tools.
            let mut message_request = message_request;
            let mut stream = match stream_result {
                Ok(s) => s,
                Err(ref err) if err.to_string().contains("does not support tools") => {
                    message_request.tools = None;
                    message_request.tool_choice = None;
                    self.client
                        .stream_message(&message_request)
                        .await
                        .map_err(|error| RuntimeError::new(error.to_string()))?
                }
                // Context overflow: compact session and retry
                Err(ref err)
                    if err.to_string().contains("context length")
                        || err.to_string().contains("too many tokens")
                        || err.to_string().contains("413") =>
                {
                    eprintln!(
                        "\x1b[33m[note]\x1b[0m Context overflow - compacting session and retrying..."
                    );
                    // Compact won't help the current request directly since we're
                    // in the API client, but signal the runtime to compact on next turn.
                    return Err(RuntimeError::new(format!(
                        "context overflow: {} - run /compact to reduce history",
                        err
                    )));
                }
                Err(err) => return Err(RuntimeError::new(err.to_string())),
            };
            let mut stdout = io::stdout();
            let mut sink = io::sink();
            let out: &mut dyn Write = if self.emit_output {
                &mut stdout
            } else {
                &mut sink
            };
            let renderer = TerminalRenderer::new();
            let mut markdown_stream = MarkdownStreamState::default();
            let mut events = Vec::new();
            let mut pending_tool: Option<(String, String, String)> = None;
            let mut saw_stop = false;
            let mut saw_first_content = false;
            let mut saw_thinking = false;
            let mut pending_thinking = String::new();

            while let Some(event) = stream
                .next_event()
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?
            {
                match event {
                    ApiStreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            push_output_block(block, out, &mut events, &mut pending_tool, true)?;
                        }
                    }
                    ApiStreamEvent::ContentBlockStart(start) => {
                        push_output_block(
                            start.content_block,
                            out,
                            &mut events,
                            &mut pending_tool,
                            true,
                        )?;
                    }
                    ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                        ContentBlockDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                if let Some(progress_reporter) = &self.progress_reporter {
                                    progress_reporter.mark_text_phase(&text);
                                }
                                if !pending_thinking.trim().is_empty() {
                                    stop_stream_spinner_once(&mut saw_first_content);
                                    clear_thinking_preview_if_needed(&mut saw_thinking);
                                    flush_thinking_section_if_needed(
                                        &renderer,
                                        out,
                                        &mut pending_thinking,
                                    )?;
                                }
                                if let Some(rendered) = markdown_stream.push(&renderer, &text) {
                                    stop_stream_spinner_once(&mut saw_first_content);
                                    clear_thinking_preview_if_needed(&mut saw_thinking);
                                    write!(out, "{rendered}")
                                        .and_then(|()| out.flush())
                                        .map_err(|error| RuntimeError::new(error.to_string()))?;
                                }
                                events.push(AssistantEvent::TextDelta(text));
                            }
                        }
                        ContentBlockDelta::InputJsonDelta { partial_json } => {
                            if let Some((_, _, input)) = &mut pending_tool {
                                input.push_str(&partial_json);
                            }
                        }
                        ContentBlockDelta::ThinkingDelta { thinking } => {
                            // Always accumulate thinking tokens so the full
                            // section is rendered when the block completes.
                            if self.emit_output {
                                pending_thinking.push_str(&thinking);
                            }
                            // The inline preview line is only shown when
                            // verbose mode is active.
                            if should_render_thinking_preview(self.emit_output) {
                                if let Some(preview) = format_thinking_preview(&thinking) {
                                    stop_stream_spinner_once(&mut saw_first_content);
                                    saw_thinking = true;
                                    write_thinking_preview_line(&preview);
                                }
                            }
                        }
                        ContentBlockDelta::SignatureDelta { .. } => {}
                    },
                    ApiStreamEvent::ContentBlockStop(_) => {
                        clear_thinking_preview_if_needed(&mut saw_thinking);
                        if !pending_thinking.trim().is_empty() {
                            stop_stream_spinner_once(&mut saw_first_content);
                            flush_thinking_section_if_needed(
                                &renderer,
                                out,
                                &mut pending_thinking,
                            )?;
                        }
                        if !markdown_stream.pending.trim().is_empty() {
                            stop_stream_spinner_once(&mut saw_first_content);
                        }
                        if let Some(rendered) = markdown_stream.flush(&renderer) {
                            write!(out, "{rendered}")
                                .and_then(|()| out.flush())
                                .map_err(|error| RuntimeError::new(error.to_string()))?;
                        }
                        if let Some((id, name, input)) = pending_tool.take() {
                            stop_stream_spinner_once(&mut saw_first_content);
                            if let Some(progress_reporter) = &self.progress_reporter {
                                progress_reporter.mark_tool_phase(&name, &input);
                            }
                            // Display tool call now that input is fully accumulated
                            writeln!(out, "\n{}", format_tool_call_start(&name, &input))
                                .and_then(|()| out.flush())
                                .map_err(|error| RuntimeError::new(error.to_string()))?;
                            events.push(AssistantEvent::ToolUse { id, name, input });
                        }
                    }
                    ApiStreamEvent::MessageDelta(delta) => {
                        // Detect max_tokens hit — the model was cut off mid-response.
                        if delta.delta.stop_reason.as_deref()
                            == Some("max_tokens")
                            || delta.delta.stop_reason.as_deref() == Some("length")
                        {
                            eprintln!(
                                "\n\x1b[33m[note]\x1b[0m Response truncated (max_tokens reached). \
                                 Use /compact to free context, or increase with /config."
                            );
                        }
                        events.push(AssistantEvent::Usage(TokenUsage {
                            input_tokens: delta.usage.input_tokens,
                            output_tokens: delta.usage.output_tokens,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                        }));
                    }
                    ApiStreamEvent::MessageStop(_) => {
                        saw_stop = true;
                        clear_thinking_preview_if_needed(&mut saw_thinking);
                        if !pending_thinking.trim().is_empty() {
                            stop_stream_spinner_once(&mut saw_first_content);
                            flush_thinking_section_if_needed(
                                &renderer,
                                out,
                                &mut pending_thinking,
                            )?;
                        }
                        if !markdown_stream.pending.trim().is_empty() {
                            stop_stream_spinner_once(&mut saw_first_content);
                        }
                        if let Some(rendered) = markdown_stream.flush(&renderer) {
                            write!(out, "{rendered}")
                                .and_then(|()| out.flush())
                                .map_err(|error| RuntimeError::new(error.to_string()))?;
                        }
                        events.push(AssistantEvent::MessageStop);
                    }
                }
            }

            if !saw_stop
                && events.iter().any(|event| {
                    matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                        || matches!(event, AssistantEvent::ToolUse { .. })
                })
            {
                events.push(AssistantEvent::MessageStop);
            }

            if events
                .iter()
                .any(|event| matches!(event, AssistantEvent::MessageStop))
            {
                // Record the response in prompt cache for future lookups.
                // Build a synthetic MessageResponse from the collected events.
                let mut cached_content = Vec::new();
                let mut cached_usage = Usage {
                    input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    output_tokens: 0,
                };
                let mut full_text = String::new();
                for event in &events {
                    match event {
                        AssistantEvent::TextDelta(text) => full_text.push_str(text),
                        AssistantEvent::ToolUse { id, name, input } => {
                            cached_content.push(OutputContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: serde_json::from_str(input)
                                    .unwrap_or(serde_json::json!({})),
                            });
                        }
                        AssistantEvent::Usage(usage) => {
                            cached_usage.input_tokens = usage.input_tokens;
                            cached_usage.output_tokens = usage.output_tokens;
                            cached_usage.cache_creation_input_tokens =
                                usage.cache_creation_input_tokens;
                            cached_usage.cache_read_input_tokens =
                                usage.cache_read_input_tokens;
                        }
                        _ => {}
                    }
                }
                if !full_text.is_empty() {
                    cached_content.insert(
                        0,
                        OutputContentBlock::Text { text: full_text },
                    );
                }
                let cached_response = MessageResponse {
                    id: format!("cached-{}", now_unix_millis()),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: cached_content,
                    model: self.model.clone(),
                    stop_reason: Some("end_turn".to_string()),
                    stop_sequence: None,
                    usage: cached_usage,
                    request_id: None,
                };
                let _ = prompt_cache.record_response(
                    &message_request_for_cache,
                    &cached_response,
                );

                return Ok(events);
            }

            let response = self
                .client
                .send_message(&MessageRequest {
                    stream: false,
                    ..message_request.clone()
                })
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            response_to_events(response, out)
        })
    }
}

enum MachineReadableTurnResult {
    Builtin { message: String },
    Summary(runtime::TurnSummary),
}

fn write_structured_json_line(
    value: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn token_usage_json(usage: &TokenUsage) -> serde_json::Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
        "cache_read_input_tokens": usage.cache_read_input_tokens,
    })
}

fn prompt_builtin_payload(model: &str, message: &str) -> serde_json::Value {
    json!({
        "message": message,
        "model": model,
        "iterations": 0,
        "tool_uses": Vec::<serde_json::Value>::new(),
        "tool_results": Vec::<serde_json::Value>::new(),
        "usage": token_usage_json(&TokenUsage::default()),
    })
}

fn prompt_summary_payload(model: &str, summary: &runtime::TurnSummary) -> serde_json::Value {
    json!({
        "message": machine_readable_assistant_message(summary),
        "model": model,
        "iterations": summary.iterations,
        "tool_uses": collect_tool_uses(summary),
        "tool_results": collect_tool_results(summary),
        "usage": token_usage_json(&summary.usage),
    })
}

fn transport_event_with_payload(
    event_type: &str,
    mut payload: serde_json::Value,
) -> serde_json::Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert("type".to_string(), json!(event_type));
        payload
    } else {
        json!({
            "type": event_type,
            "payload": payload,
        })
    }
}

fn prompt_builtin_ndjson_events(model: &str, message: &str) -> Vec<serde_json::Value> {
    let mut events = vec![json!({
        "type": "turn_started",
        "model": model,
    })];
    if !message.is_empty() {
        events.push(json!({
            "type": "assistant_text",
            "text": message,
        }));
    }
    events.push(json!({
        "type": "usage",
        "usage": token_usage_json(&TokenUsage::default()),
    }));
    events.push(transport_event_with_payload(
        "turn_completed",
        prompt_builtin_payload(model, message),
    ));
    events
}

fn prompt_summary_ndjson_events(
    model: &str,
    summary: &runtime::TurnSummary,
) -> Vec<serde_json::Value> {
    let mut events = vec![json!({
        "type": "turn_started",
        "model": model,
    })];
    let mut tool_results_by_id = std::collections::BTreeMap::<String, Vec<serde_json::Value>>::new();

    for message in &summary.tool_results {
        for block in &message.blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } = block
            {
                tool_results_by_id
                    .entry(tool_use_id.clone())
                    .or_default()
                    .push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "tool_name": tool_name,
                        "output": output,
                        "is_error": is_error,
                    }));
            }
        }
    }

    for message in &summary.assistant_messages {
        let mut pending_tool_ids = Vec::new();
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => {
                    let text = sanitize_assistant_text(text);
                    if text.is_empty() {
                        continue;
                    }
                    events.push(json!({
                        "type": "assistant_text",
                        "text": text,
                    }));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    pending_tool_ids.push(id.clone());
                    events.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input,
                    }));
                }
                _ => {}
            }
        }

        for tool_use_id in pending_tool_ids {
            if let Some(tool_results) = tool_results_by_id.remove(&tool_use_id) {
                events.extend(tool_results);
            }
        }
    }

    for (_tool_use_id, tool_results) in tool_results_by_id {
        events.extend(tool_results);
    }

    events.push(json!({
        "type": "usage",
        "usage": token_usage_json(&summary.usage),
    }));
    events.push(transport_event_with_payload(
        "turn_completed",
        prompt_summary_payload(model, summary),
    ));
    events
}

pub(crate) fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    sanitize_assistant_text(
        &summary
        .assistant_messages
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default(),
    )
}

fn machine_readable_assistant_message(summary: &runtime::TurnSummary) -> String {
    let message = final_assistant_text(summary);
    if !message.is_empty() {
        return message;
    }

    fallback_tool_result_message(summary).unwrap_or_default()
}

fn sanitize_assistant_text(text: &str) -> String {
    strip_thinking_tags(&strip_terminal_escape_sequences_preserving_newlines(text))
        .trim()
        .to_string()
}

fn fallback_tool_result_message(summary: &runtime::TurnSummary) -> Option<String> {
    let mut unique_messages = Vec::new();
    let mut counts = std::collections::BTreeMap::<String, usize>::new();

    for tool_result in collect_tool_results(summary) {
        let output = tool_result
            .get("output")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let is_error = tool_result
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let Some(message) = summarize_tool_result_for_message(output, is_error) else {
            continue;
        };
        *counts.entry(message.clone()).or_default() += 1;
        if !unique_messages.iter().any(|existing| existing == &message) {
            unique_messages.push(message);
        }
    }

    match unique_messages.len() {
        0 => None,
        1 => {
            let message = unique_messages.pop().unwrap_or_default();
            let count = counts.get(&message).copied().unwrap_or(1);
            if count > 1 {
                Some(format!("{message} (repeated {count} times)"))
            } else {
                Some(message)
            }
        }
        _ => Some(unique_messages.join("\n")),
    }
}

fn summarize_tool_result_for_message(output: &str, is_error: bool) -> Option<String> {
    let structured = serde_json::from_str::<serde_json::Value>(output).ok();
    let candidate = structured
        .as_ref()
        .and_then(|value| summarize_structured_tool_result_for_message(value, is_error))
        .unwrap_or_else(|| output.to_string());
    let compact = strip_thinking_tags(&strip_terminal_escape_sequences(&candidate))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let compact = compact.trim();
    if compact.is_empty() {
        None
    } else {
        Some(truncate_for_summary(compact, 220))
    }
}

fn summarize_structured_tool_result_for_message(
    value: &serde_json::Value,
    is_error: bool,
) -> Option<String> {
    if is_error {
        if let Some(message) = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str)
            .filter(|message| !message.trim().is_empty())
        {
            return Some(message.to_string());
        }
    }

    for path in [
        &["stdout"][..],
        &["stderr"][..],
        &["message"][..],
        &["result", "message"][..],
        &["data", "message"][..],
    ] {
        if let Some(message) = json_string_at_path(value, &path) {
            if !message.trim().is_empty() {
                return Some(message.to_string());
            }
        }
    }

    if let Some(message) = summarize_lsp_tool_result_for_message(value) {
        return Some(message);
    }

    if let Some(message) = summarize_mcp_tool_result_for_message(value) {
        return Some(message);
    }

    None
}

fn summarize_lsp_tool_result_for_message(value: &serde_json::Value) -> Option<String> {
    let payload = value.get("data").unwrap_or(value);
    let action = payload.get("action").and_then(serde_json::Value::as_str);
    let hint = payload
        .get("hint")
        .or_else(|| payload.get("message"))
        .and_then(serde_json::Value::as_str)
        .filter(|hint| !hint.trim().is_empty());

    if action.is_none() && hint.is_none() {
        return None;
    }

    let file = payload
        .get("file")
        .or_else(|| payload.get("file_path"))
        .or_else(|| payload.get("filePath"))
        .and_then(serde_json::Value::as_str);
    let line = payload.get("line").and_then(serde_json::Value::as_u64);
    let character = payload
        .get("character")
        .and_then(serde_json::Value::as_u64);

    let mut parts = Vec::new();
    if let Some(action) = action {
        parts.push(format!("lsp {action}"));
    }
    if let Some(file) = file {
        parts.push(format!("at {}", summarize_location(file, line, character)));
    }
    if let Some(hint) = hint {
        parts.push(hint.to_string());
    }

    (!parts.is_empty()).then(|| parts.join(" — "))
}

fn summarize_mcp_tool_result_for_message(value: &serde_json::Value) -> Option<String> {
    let payload = value.get("result").unwrap_or(value);

    let text_blocks = payload
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if !text_blocks.is_empty() {
        return Some(text_blocks.join("\n"));
    }

    if let Some(contents) = payload
        .get("contents")
        .and_then(serde_json::Value::as_array)
    {
        let previews = contents
            .iter()
            .filter_map(|item| {
                let uri = item.get("uri").and_then(serde_json::Value::as_str)?;
                let text = item
                    .get("text")
                    .or_else(|| item.get("blob"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if text.trim().is_empty() {
                    Some(uri.to_string())
                } else {
                    Some(format!("{uri}: {text}"))
                }
            })
            .collect::<Vec<_>>();
        if !previews.is_empty() {
            return Some(previews.join("\n"));
        }
    }

    None
}

fn json_string_at_path<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str()
}

fn summarize_location(file: &str, line: Option<u64>, character: Option<u64>) -> String {
    match (line, character) {
        (Some(line), Some(character)) => format!("{file}:{line}:{character}"),
        (Some(line), None) => format!("{file}:{line}"),
        _ => file.to_string(),
    }
}

pub(crate) fn collect_tool_uses(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .assistant_messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "name": name,
                "input": input,
            })),
            _ => None,
        })
        .collect()
}

pub(crate) fn collect_tool_results(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .tool_results
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => Some(json!({
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "output": output,
                "is_error": is_error,
            })),
            _ => None,
        })
        .collect()
}

fn slash_command_completion_candidates() -> Vec<String> {
    let mut candidates = slash_command_specs()
        .iter()
        .flat_map(|spec| {
            std::iter::once(spec.name)
                .chain(spec.aliases.iter().copied())
                .map(|name| format!("/{name}"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    candidates.extend([
        String::from("/vim"),
        String::from("/exit"),
        String::from("/quit"),
    ]);
    candidates.sort();
    candidates.dedup();
    candidates
}

fn suggest_repl_commands(name: &str) -> Vec<String> {
    let normalized = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut ranked = slash_command_completion_candidates()
        .into_iter()
        .filter_map(|candidate| {
            let raw = candidate.trim_start_matches('/').to_ascii_lowercase();
            let distance = edit_distance(&normalized, &raw);
            let prefix_match = raw.starts_with(&normalized) || normalized.starts_with(&raw);
            let near_match = distance <= 2;
            (prefix_match || near_match).then_some((distance, candidate))
        })
        .collect::<Vec<_>>();
    ranked.sort();
    ranked.dedup_by(|left, right| left.1 == right.1);
    ranked
        .into_iter()
        .map(|(_, candidate)| candidate)
        .take(3)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    if left == right {
        return 0;
    }
    if left.is_empty() {
        return right.chars().count();
    }
    if right.is_empty() {
        return left.chars().count();
    }

    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != *right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution_cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_chars.len()]
}

const THINKING_PREVIEW_MAX_CHARS: usize = 88;

fn flush_thinking_section_if_needed(
    renderer: &TerminalRenderer,
    out: &mut (impl Write + ?Sized),
    pending_thinking: &mut String,
) -> Result<(), RuntimeError> {
    if pending_thinking.trim().is_empty() {
        pending_thinking.clear();
        return Ok(());
    }

    if let Some(section) = format_thinking_section(renderer, pending_thinking) {
        writeln!(out, "{section}")
            .and_then(|()| out.flush())
            .map_err(|error| RuntimeError::new(error.to_string()))?;
    }
    pending_thinking.clear();
    Ok(())
}

fn stop_stream_spinner_once(saw_first_content: &mut bool) {
    if *saw_first_content {
        return;
    }
    *saw_first_content = true;
    stop_fire_spinner();
    let deadline = Instant::now() + Duration::from_millis(250);
    while is_fire_spinner_running() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
}

fn clear_thinking_preview_line() {
    use crossterm::{cursor, execute, terminal};

    let mut stderr = io::stderr();
    let _ = execute!(
        stderr,
        cursor::MoveToColumn(0),
        terminal::Clear(terminal::ClearType::CurrentLine),
        cursor::Show,
    );
    let _ = stderr.flush();
}

fn clear_thinking_preview_if_needed(saw_thinking: &mut bool) {
    if !*saw_thinking {
        return;
    }
    clear_thinking_preview_line();
    *saw_thinking = false;
}

fn should_render_thinking_preview(emit_output: bool) -> bool {
    emit_output && VERBOSE_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

fn strip_terminal_escape_sequences_preserving_newlines(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if ch.is_control() {
            match ch {
                '\n' => output.push('\n'),
                '\r' | '\t' | '\u{7}' => output.push(' '),
                _ if ch.is_whitespace() => output.push(' '),
                _ => {}
            }
        } else {
            output.push(ch);
        }
    }

    output
}

fn strip_thinking_tags(value: &str) -> String {
    value
        .replace("<think>", "")
        .replace("</think>", "")
        .replace("<thinking>", "")
        .replace("</thinking>", "")
}

pub(crate) fn strip_terminal_escape_sequences(value: &str) -> String {
    strip_terminal_escape_sequences_preserving_newlines(value).replace('\n', " ")
}

fn format_thinking_preview(thinking: &str) -> Option<String> {
    let compact = strip_thinking_tags(&strip_terminal_escape_sequences(thinking))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        return None;
    }

    let char_count = compact.chars().count();
    if char_count <= THINKING_PREVIEW_MAX_CHARS {
        return Some(compact);
    }

    let visible_chars = THINKING_PREVIEW_MAX_CHARS.saturating_sub(5);
    let head_chars = visible_chars / 2;
    let tail_chars = visible_chars.saturating_sub(head_chars);
    let head = compact.chars().take(head_chars).collect::<String>();
    let tail = compact
        .chars()
        .skip(char_count.saturating_sub(tail_chars))
        .collect::<String>();
    Some(format!("{head} … {tail}"))
}

fn format_thinking_section(renderer: &TerminalRenderer, thinking: &str) -> Option<String> {
    let cleaned = strip_thinking_tags(&strip_terminal_escape_sequences_preserving_newlines(
        thinking,
    ))
    .lines()
    .map(str::trim_end)
    .collect::<Vec<_>>()
    .join("\n")
    .trim()
    .to_string();

    if cleaned.is_empty() {
        return None;
    }

    let rendered = renderer.markdown_to_ansi(&cleaned);
    Some(render_surface_card(
        "[thinking]",
        &rendered,
        SurfaceTone::Thinking,
    ))
}

fn write_thinking_preview_line(preview: &str) {
    use crossterm::{cursor, execute, terminal};

    let mut stderr = io::stderr();
    let _ = execute!(
        stderr,
        cursor::MoveToColumn(0),
        terminal::Clear(terminal::ClearType::CurrentLine),
    );
    let _ = write!(
        stderr,
        "\x1b[38;5;245m[thinking]\x1b[0m \x1b[2m{preview}\x1b[0m"
    );
    let _ = stderr.flush();
}

pub(crate) fn truncate_for_summary(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn tool_error_is_missing_path(message: &str) -> bool {
    message.contains("No such file or directory")
}

fn enrich_tool_error_for_model(tool_name: &str, input: &str, raw_message: &str) -> String {
    if tool_error_is_missing_path(raw_message) {
        let path = serde_json::from_str::<serde_json::Value>(input)
            .ok()
            .and_then(|value| {
                value
                    .get("path")
                    .and_then(|candidate| candidate.as_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        return format!(
            "{raw_message}\n<system-reminder>The requested path or directory `{path}` was not found for tool `{tool_name}`. Do not answer with generic filesystem troubleshooting yet. Infer the user's goal and continue by inspecting the workspace with another tool, such as glob_search, grep_search, or bash (for example `pwd` or `git status --short --branch`). Only answer once you have gathered the relevant project data.</system-reminder>"
        );
    }

    if raw_message.starts_with("tool input error:")
        || raw_message.starts_with("invalid tool input JSON:")
    {
        return format!(
            "{raw_message}\n<system-reminder>Your previous tool call was malformed. Fix the tool arguments and retry it yourself. Do not ask the user to provide raw tool JSON unless they explicitly ask for tool syntax.</system-reminder>"
        );
    }

    raw_message.to_string()
}

fn push_output_block(
    block: OutputContentBlock,
    out: &mut (impl Write + ?Sized),
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String)>,
    streaming_tool_input: bool,
) -> Result<(), RuntimeError> {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                let rendered = TerminalRenderer::new().markdown_to_ansi(&text);
                write!(out, "{rendered}")
                    .and_then(|()| out.flush())
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            // During streaming, the initial content_block_start has an empty input ({}).
            // The real input arrives via input_json_delta events. In
            // non-streaming responses, preserve a legitimate empty object.
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            *pending_tool = Some((id, name, initial_input));
        }
        OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {}
    }
    Ok(())
}

fn response_to_events(
    response: MessageResponse,
    out: &mut (impl Write + ?Sized),
) -> Result<Vec<AssistantEvent>, RuntimeError> {
    let mut events = Vec::new();
    let mut pending_tool = None;

    for block in response.content {
        push_output_block(block, out, &mut events, &mut pending_tool, false)?;
        if let Some((id, name, input)) = pending_tool.take() {
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    events.push(AssistantEvent::Usage(TokenUsage {
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
        cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
        cache_read_input_tokens: response.usage.cache_read_input_tokens,
    }));
    events.push(AssistantEvent::MessageStop);
    Ok(events)
}

struct CliToolExecutor {
    renderer: TerminalRenderer,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    /// MCP server manager for invoking MCP tools — None if no MCP servers configured.
    mcp_manager: Option<Arc<Mutex<runtime::McpServerManager>>>,
    /// Async runtime for MCP/LSP calls that need tokio.
    async_runtime: Option<Arc<tokio::runtime::Runtime>>,
}

impl CliToolExecutor {
    fn new(
        allowed_tools: Option<AllowedToolSet>,
        emit_output: bool,
        tool_registry: GlobalToolRegistry,
    ) -> Self {
        // Try to initialize MCP from config
        let cwd = env::current_dir().unwrap_or_default();
        let loader = runtime::ConfigLoader::default_for(&cwd);
        let mcp_manager = loader
            .load()
            .ok()
            .map(|config| {
                Arc::new(Mutex::new(runtime::McpServerManager::from_runtime_config(
                    &config,
                )))
            });
        let async_runtime = mcp_manager
            .as_ref()
            .map(|_| Arc::new(tokio::runtime::Runtime::new().expect("tokio runtime")));

        Self {
            renderer: TerminalRenderer::new(),
            emit_output,
            allowed_tools,
            tool_registry,
            mcp_manager,
            async_runtime,
        }
    }

    /// Handle MCP tool invocation via the McpServerManager.
    fn execute_mcp_tool(&self, input: &serde_json::Value) -> Result<String, ToolError> {
        let server_name = input["server_name"]
            .as_str()
            .ok_or_else(|| ToolError::new("missing server_name"))?;
        let tool_name = input["tool_name"]
            .as_str()
            .ok_or_else(|| ToolError::new("missing tool_name"))?;
        let arguments = input
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        let (manager, rt) = self
            .mcp_manager
            .as_ref()
            .zip(self.async_runtime.as_ref())
            .ok_or_else(|| ToolError::new("no MCP servers configured"))?;

        let mut mgr = manager.lock().unwrap_or_else(|e| e.into_inner());
        // MCP tool names are qualified as "server_name__tool_name"
        let qualified_name = format!("{server_name}__{tool_name}");
        let args = if arguments.is_null() {
            None
        } else {
            Some(arguments)
        };
        rt.block_on(async {
            mgr.call_tool(&qualified_name, args)
                .await
                .map(|result| {
                    serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|_| "MCP tool returned non-JSON result".to_string())
                })
                .map_err(|e| ToolError::new(e.to_string()))
        })
    }

    /// Handle LSP tool actions (diagnostics, go-to-definition, references).
    fn execute_lsp_tool(&self, input: &serde_json::Value) -> Result<String, ToolError> {
        let action = input["action"]
            .as_str()
            .ok_or_else(|| ToolError::new("missing action field"))?;

        match action {
            "diagnostics" => {
                // Use bash to run real lint/check commands
                Ok(serde_json::json!({
                    "action": "diagnostics",
                    "hint": "Use the bash tool to run linting commands directly",
                    "examples": [
                        "cargo clippy --workspace 2>&1 | head -50",
                        "python3 -m py_compile <file>",
                        "npx tsc --noEmit 2>&1 | head -50"
                    ]
                })
                .to_string())
            }
            "definition" => {
                let file_path = input["file_path"]
                    .as_str()
                    .ok_or_else(|| ToolError::new("missing file_path"))?;
                let line = input["line"].as_u64().unwrap_or(0);
                // Use grep as a fallback for go-to-definition
                Ok(serde_json::json!({
                    "action": "definition",
                    "file": file_path,
                    "line": line,
                    "hint": "Use grep_search to find the definition",
                    "example": format!("grep_search with pattern for the symbol at {}:{}", file_path, line)
                })
                .to_string())
            }
            "references" => {
                let file_path = input["file_path"]
                    .as_str()
                    .ok_or_else(|| ToolError::new("missing file_path"))?;
                Ok(serde_json::json!({
                    "action": "references",
                    "file": file_path,
                    "hint": "Use grep_search to find all references"
                })
                .to_string())
            }
            _ => Err(ToolError::new(format!("unknown LSP action: {action}"))),
        }
    }
}

fn tool_context_path(tool_name: &str, input: &serde_json::Value) -> Option<PathBuf> {
    let path = match tool_name {
        "read_file" | "Read" | "write_file" | "Write" | "edit_file" | "Edit" => input
            .get("path")
            .or_else(|| input.get("file_path"))
            .or_else(|| input.get("filePath"))
            .and_then(serde_json::Value::as_str),
        "NotebookEdit" => input
            .get("notebook_path")
            .or_else(|| input.get("notebookPath"))
            .or_else(|| input.get("path"))
            .and_then(serde_json::Value::as_str),
        _ => None,
    }?;

    let trimmed = path.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

fn inject_file_context_into_tool_output(
    tool_name: &str,
    input: &serde_json::Value,
    output: &str,
) -> String {
    let Some(file_path) = tool_context_path(tool_name, input) else {
        return output.to_string();
    };

    let snippets = context::collect_file_context(&file_path);
    let Some(injected_context) = context::render_context_section(&snippets)
        .filter(|context| !context.trim().is_empty())
    else {
        return output.to_string();
    };

    match serde_json::from_str::<serde_json::Value>(output) {
        Ok(mut parsed) => {
            if let Some(object) = parsed.as_object_mut() {
                object.insert(
                    "injectedContext".to_string(),
                    serde_json::Value::String(injected_context),
                );
                serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| output.to_string())
            } else {
                output.to_string()
            }
        }
        Err(_) => format!("{output}\n\n{injected_context}"),
    }
}

impl ToolExecutor for CliToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if self
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(tool_name))
        {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled by the current --allowedTools setting"
            )));
        }
        let value: serde_json::Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;

        // Intercept MCP/LSP tools — these need async runtime access
        // that the tools crate doesn't have.
        let result = match tool_name {
            "MCPTool" => self.execute_mcp_tool(&value),
            "LSPTool" => self.execute_lsp_tool(&value),
            "ListMcpResources" => {
                let server = value["server_name"].as_str().unwrap_or("unknown");
                match (&self.mcp_manager, &self.async_runtime) {
                    (Some(mgr), Some(rt)) => {
                        let mut m = mgr.lock().unwrap_or_else(|e| e.into_inner());
                        rt.block_on(async {
                            m.list_resources(server)
                                .await
                                .map(|result| {
                                    let resources = result.result
                                        .map(|r| r.resources)
                                        .unwrap_or_default();
                                    serde_json::json!({
                                        "server": server,
                                        "resources": resources.iter().map(|r| {
                                            serde_json::json!({
                                                "uri": r.uri,
                                                "name": r.name,
                                                "description": r.description,
                                            })
                                        }).collect::<Vec<_>>(),
                                        "count": resources.len(),
                                    })
                                    .to_string()
                                })
                                .map_err(|e| ToolError::new(e.to_string()))
                        })
                    }
                    _ => Err(ToolError::new(format!(
                        "no MCP servers configured for {server}"
                    ))),
                }
            }
            "ReadMcpResource" => {
                let server = value["server_name"].as_str().unwrap_or("unknown");
                let uri = value["resource_uri"].as_str().unwrap_or("");
                match (&self.mcp_manager, &self.async_runtime) {
                    (Some(mgr), Some(rt)) => {
                        let mut m = mgr.lock().unwrap_or_else(|e| e.into_inner());
                        rt.block_on(async {
                            m.read_resource(server, uri)
                                .await
                                .map(|result| {
                                    serde_json::to_string_pretty(&result)
                                        .unwrap_or_else(|_| "{}".to_string())
                                })
                                .map_err(|e| ToolError::new(e.to_string()))
                        })
                    }
                    _ => Err(ToolError::new(format!(
                        "no MCP servers configured for {server}"
                    ))),
                }
            }
            _ => self.tool_registry.execute(tool_name, &value).map_err(|e| ToolError::new(e.to_string())),
        };

        match result {
            Ok(output) => {
                let output = inject_file_context_into_tool_output(tool_name, &value, &output);
                if self.emit_output {
                    let markdown = format_tool_result(tool_name, &output, false);
                    self.renderer
                        .stream_markdown(&markdown, &mut io::stdout())
                        .map_err(|error| ToolError::new(error.to_string()))?;
                }
                Ok(output)
            }
            Err(error) => {
                let raw_message = error.to_string();
                if self.emit_output {
                    let markdown = format_tool_result(tool_name, &raw_message, true);
                    self.renderer
                        .stream_markdown(&markdown, &mut io::stdout())
                        .map_err(|stream_error| ToolError::new(stream_error.to_string()))?;
                }
                Err(ToolError::new(enrich_tool_error_for_model(
                    tool_name,
                    input,
                    &raw_message,
                )))
            }
        }
    }
}

fn permission_policy(mode: PermissionMode, tool_registry: &GlobalToolRegistry) -> PermissionPolicy {
    tool_registry.permission_specs(None).into_iter().fold(
        PermissionPolicy::new(mode),
        |policy, (name, required_permission)| {
            policy.with_tool_requirement(name, required_permission)
        },
    )
}

fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => InputContentBlock::Text { text: text.clone() },
                    ContentBlock::ToolUse { id, name, input } => InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    },
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

fn print_help_to(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "Emberforge CLI v{VERSION}")?;
    writeln!(
        out,
        "  Interactive coding assistant for the current workspace."
    )?;
    writeln!(out)?;
    writeln!(out, "Quick start")?;
    writeln!(
        out,
        "  ember                                  Start the interactive REPL"
    )?;
    writeln!(
        out,
        "  ember models                           List available local and shortcut models"
    )?;
    writeln!(
        out,
        "  ember \"summarize this repo\"            Run one prompt and exit"
    )?;
    writeln!(
        out,
        "  ember prompt \"explain src/main.rs\"     Explicit one-shot prompt"
    )?;
    writeln!(
        out,
        "  ember --resume SESSION.json /status    Inspect a saved session"
    )?;
    writeln!(out)?;
    writeln!(out, "Interactive essentials")?;
    writeln!(
        out,
        "  /help                                 Browse the full slash command map"
    )?;
    writeln!(
        out,
        "  /status                               Inspect session + workspace state"
    )?;
    writeln!(
        out,
        "  /doctor [quick|full|status|reset]    Run cached setup diagnostics"
    )?;
    writeln!(
        out,
        "  /model <name>                         Switch models mid-session"
    )?;
    writeln!(
        out,
        "  /model list                           List available local and shortcut models"
    )?;
    writeln!(
        out,
        "  /permissions <mode>                   Adjust tool access"
    )?;
    writeln!(
        out,
        "  Tab                                   Complete slash commands"
    )?;
    writeln!(
        out,
        "  /vim                                  Toggle modal editing"
    )?;
    writeln!(
        out,
        "  Shift+Enter / Ctrl+J                  Insert a newline"
    )?;
    writeln!(out)?;
    writeln!(out, "Commands")?;
    writeln!(
        out,
        "  ember dump-manifests                   Read upstream TS sources and print extracted counts"
    )?;
    writeln!(
        out,
        "  ember bootstrap-plan                   Print the bootstrap phase skeleton"
    )?;
    writeln!(
        out,
        "  ember doctor [quick|full|status|reset] Run cached setup diagnostics"
    )?;
    writeln!(
        out,
        "  ember agents                           List configured agents"
    )?;
    writeln!(
        out,
        "  ember tasks [list|show|logs|attach|stop] Manage background tasks"
    )?;
    writeln!(
        out,
        "  ember skills                           List installed skills"
    )?;
    writeln!(out, "  ember system-prompt [--cwd PATH] [--date YYYY-MM-DD]")?;
    writeln!(
        out,
        "  ember login                            Start the OAuth login flow"
    )?;
    writeln!(
        out,
        "  ember logout                           Clear saved OAuth credentials"
    )?;
    writeln!(
        out,
        "  ember init                             Scaffold EMBER.md + local files"
    )?;
    writeln!(out)?;
    writeln!(out, "Flags")?;
    writeln!(
        out,
        "  --model MODEL                         Override the active model"
    )?;
    writeln!(
        out,
        "  --output-format FORMAT                Prompt-mode output: text, json, or ndjson"
    )?;
    writeln!(
        out,
        "  --permission-mode MODE                Set read-only, workspace-write, or danger-full-access"
    )?;
    writeln!(
        out,
        "  --dangerously-skip-permissions        Skip all permission checks"
    )?;
    writeln!(
        out,
        "  --allowedTools TOOLS                  Restrict enabled tools (repeatable; comma-separated aliases supported)"
    )?;
    writeln!(
        out,
        "  --version, -V                         Print version and build information"
    )?;
    writeln!(out)?;
    writeln!(out, "Slash command reference")?;
    writeln!(out, "{}", render_slash_command_help())?;
    writeln!(out)?;
    let resume_commands = resume_supported_slash_commands()
        .into_iter()
        .map(|spec| match spec.argument_hint {
            Some(argument_hint) => format!("/{} {}", spec.name, argument_hint),
            None => format!("/{}", spec.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "Resume-safe commands: {resume_commands}")?;
    writeln!(out, "Examples")?;
    writeln!(out, "  ember --model opus \"summarize this repo\"")?;
    writeln!(
        out,
        "  ember --output-format json prompt \"explain src/main.rs\""
    )?;
    writeln!(
        out,
        "  ember --output-format ndjson -p \"status\""
    )?;
    writeln!(
        out,
        "  ember --allowedTools read,glob \"summarize Cargo.toml\""
    )?;
    writeln!(
        out,
        "  ember --resume session.json /status /diff /export notes.txt"
    )?;
    writeln!(out, "  ember agents")?;
    writeln!(out, "  ember tasks list")?;
    writeln!(out, "  ember tasks logs agent-123")?;
    writeln!(out, "  ember /skills")?;
    writeln!(out, "  ember doctor full")?;
    writeln!(out, "  ember login")?;
    writeln!(out, "  ember init")?;
    Ok(())
}

fn print_help() {
    let _ = print_help_to(&mut io::stdout());
}

#[cfg(test)]
mod tests {
    use super::{
        default_model_choice, describe_tool_progress, enrich_tool_error_for_model,
        filter_tool_specs, format_available_models_report, format_compact_report,
        format_cost_report, format_internal_prompt_progress_line,
        format_model_report, format_model_switch_report, format_permissions_report,
        format_permissions_switch_report, format_resume_report, format_status_report,
        inject_file_context_into_tool_output,
        prompt_summary_ndjson_events, prompt_summary_payload,
        format_thinking_preview, format_thinking_section, format_tool_call_start,
        format_tool_result, sanitize_assistant_text, strip_terminal_escape_sequences,
        initialize_process_env_from, is_builtin_status_query,
        looks_like_placeholder_secret, normalize_permission_mode, parse_args,
        parse_git_status_metadata, permission_policy,
        print_help_to, provider_label_for_model, push_output_block, render_config_report,
        render_memory_report, render_repl_help, render_unknown_repl_command,
        resolve_model_alias, response_to_events, resume_supported_slash_commands,
        slash_command_completion_candidates, status_context, CliAction, CliOutputFormat,
        AvailableModelCatalog, InternalPromptProgressEvent, InternalPromptProgressState, SlashCommand,
        StatusUsage, THINKING_PREVIEW_MAX_CHARS,
        ANTHROPIC_DEFAULT_MODEL, DEFAULT_MODEL, OLLAMA_DEFAULT_MODEL, XAI_DEFAULT_MODEL,
    };
    use crate::doctor::{
        DoctorCache, DoctorCheck, DoctorCheckStatus, DoctorMode, DoctorReport,
        format_doctor_status, parse_doctor_mode,
    };
    use api::{MessageResponse, OutputContentBlock, Usage};
    use plugins::{PluginTool, PluginToolDefinition, PluginToolPermission};
    use runtime::{
        AssistantEvent, ContentBlock, ConversationMessage, MessageRole, PermissionMode,
        TokenUsage, TurnSummary,
    };
    use serde_json::json;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tools::GlobalToolRegistry;

    fn registry_with_plugin_tool() -> GlobalToolRegistry {
        GlobalToolRegistry::with_plugin_tools(vec![PluginTool::new(
            "plugin-demo@external",
            "plugin-demo",
            PluginToolDefinition {
                name: "plugin_echo".to_string(),
                description: Some("Echo plugin payload".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                }),
            },
            "echo".to_string(),
            Vec::new(),
            PluginToolPermission::WorkspaceWrite,
            None,
        )])
        .expect("plugin tool registry should build")
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn temp_test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ember-cli-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ))
    }

    fn cleanup_temp_dir(path: &Path) {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cleanup temp dir: {error}"),
        }
    }

    #[test]
    fn default_model_choice_prefers_configured_backends_then_local() {
        assert_eq!(
            default_model_choice(true, false),
            ANTHROPIC_DEFAULT_MODEL
        );
        assert_eq!(default_model_choice(false, true), XAI_DEFAULT_MODEL);
        assert_eq!(
            default_model_choice(false, false),
            OLLAMA_DEFAULT_MODEL
        );
    }

    #[test]
    fn provider_label_matches_selected_backend() {
        assert_eq!(provider_label_for_model("claude-sonnet-4-6"), "Anthropic");
        assert_eq!(provider_label_for_model("grok-mini"), "xAI");
        assert_eq!(provider_label_for_model("qwen3:8b"), "Ollama");
    }

    #[test]
    fn builtin_status_query_matches_short_status_requests_only() {
        assert!(is_builtin_status_query("Check the current project status ?"));
        assert!(is_builtin_status_query("workspace status"));
        assert!(is_builtin_status_query("What's the current project status?"));
        assert!(!is_builtin_status_query(
            "Check the current project status and summarize recent code changes"
        ));
        assert!(!is_builtin_status_query("status of src/main.rs"));
    }

    #[test]
    fn placeholder_secret_detection_ignores_shipped_examples() {
        assert!(looks_like_placeholder_secret("your_anthropic_api_key_here"));
        assert!(looks_like_placeholder_secret("<your_xai_api_key_here>"));
        assert!(!looks_like_placeholder_secret("ollama"));
        assert!(!looks_like_placeholder_secret("sk-live-real-key"));
    }

    #[test]
    fn missing_path_tool_errors_gain_model_only_recovery_guidance() {
        let enriched = enrich_tool_error_for_model(
            "read_file",
            r#"{"path":"status.md"}"#,
            "No such file or directory (os error 2)",
        );

        assert!(enriched.contains("No such file or directory"));
        assert!(enriched.contains("status.md"));
        assert!(enriched.contains("<system-reminder>"));
        assert!(enriched.contains("Do not answer with generic filesystem troubleshooting"));
        assert!(enriched.contains("git status --short --branch"));
    }

    #[test]
    fn malformed_tool_calls_gain_retry_guidance_for_model() {
        let enriched = enrich_tool_error_for_model(
            "read_file",
            r#"{"path":{"type":"string"}}"#,
            "tool input error: invalid type: map, expected usize",
        );

        assert!(enriched.contains("tool input error"));
        assert!(enriched.contains("<system-reminder>"));
        assert!(enriched.contains("Fix the tool arguments and retry"));
        assert!(enriched.contains("Do not ask the user to provide raw tool JSON"));
    }

    #[test]
    fn initialize_process_env_loads_parent_dotenv_without_overwriting_real_env() {
        let _lock = env_lock();
        let root = temp_test_dir("dotenv");
        let nested = root.join("nested/project");
        fs::create_dir_all(&nested).expect("nested temp dir should exist");
        fs::write(
            root.join(".env"),
            r#"
# local sample config
ANTHROPIC_API_KEY=your_anthropic_api_key_here
XAI_API_KEY="xai-test-key"
XAI_BASE_URL=https://example.x.ai/v1
OLLAMA_BASE_URL=http://localhost:11434/v1
"#,
        )
        .expect("dotenv file should be written");

        let _anthropic_api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
        let _anthropic_auth_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", None);
        let _xai_api_key = EnvVarGuard::set("XAI_API_KEY", None);
        let _xai_base_url = EnvVarGuard::set("XAI_BASE_URL", Some("https://override.x.ai/v1"));
        let _openai_api_key = EnvVarGuard::set("OPENAI_API_KEY", None);
        let _ollama_base_url = EnvVarGuard::set("OLLAMA_BASE_URL", None);

        initialize_process_env_from(&nested);

        assert!(std::env::var("ANTHROPIC_API_KEY").is_err());
        assert_eq!(std::env::var("XAI_API_KEY").as_deref(), Ok("xai-test-key"));
        assert_eq!(
            std::env::var("XAI_BASE_URL").as_deref(),
            Ok("https://override.x.ai/v1")
        );
        assert_eq!(
            std::env::var("OLLAMA_BASE_URL").as_deref(),
            Ok("http://localhost:11434/v1")
        );

        cleanup_temp_dir(&root);
    }

    #[test]
    fn defaults_to_repl_when_no_args() {
        assert_eq!(
            parse_args(&[]).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn parses_prompt_subcommand() {
        let args = vec![
            "prompt".to_string(),
            "hello".to_string(),
            "world".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "hello world".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn parses_bare_prompt_and_json_output_flag() {
        let args = vec![
            "--output-format=json".to_string(),
            "--model".to_string(),
            "custom-opus".to_string(),
            "explain".to_string(),
            "this".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "explain this".to_string(),
                model: "custom-opus".to_string(),
                output_format: CliOutputFormat::Json,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn parses_bare_prompt_and_ndjson_output_flag() {
        let args = vec![
            "--output-format=ndjson".to_string(),
            "--model".to_string(),
            "custom-opus".to_string(),
            "explain".to_string(),
            "this".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "explain this".to_string(),
                model: "custom-opus".to_string(),
                output_format: CliOutputFormat::Ndjson,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn rejects_structured_output_outside_prompt_mode() {
        let repl_error = parse_args(&["--output-format=json".to_string()])
            .expect_err("structured output should require prompt mode");
        assert!(repl_error.contains("only supported with prompt mode"));

        let subcommand_error = parse_args(&[
            "--output-format=ndjson".to_string(),
            "models".to_string(),
        ])
        .expect_err("structured output should reject non-prompt subcommands");
        assert!(subcommand_error.contains("only supported with prompt mode"));
    }

    #[test]
    fn resolves_model_aliases_in_args() {
        let args = vec![
            "--model".to_string(),
            "opus".to_string(),
            "explain".to_string(),
            "this".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "explain this".to_string(),
                model: "claude-opus-4-6".to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn resolves_known_model_aliases() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-6");
        assert_eq!(resolve_model_alias("sonnet"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_alias("haiku"), "claude-haiku-4-5-20251213");
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
        assert_eq!(resolve_model_alias("custom-opus"), "custom-opus");
    }

    #[test]
    fn parses_version_flags_without_initializing_prompt_mode() {
        assert_eq!(
            parse_args(&["--version".to_string()]).expect("args should parse"),
            CliAction::Version
        );
        assert_eq!(
            parse_args(&["-V".to_string()]).expect("args should parse"),
            CliAction::Version
        );
    }

    #[test]
    fn parses_permission_mode_flag() {
        let args = vec!["--permission-mode=read-only".to_string()];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: None,
                permission_mode: PermissionMode::ReadOnly,
            }
        );
    }

    #[test]
    fn parses_allowed_tools_flags_with_aliases_and_lists() {
        let args = vec![
            "--allowedTools".to_string(),
            "read,glob".to_string(),
            "--allowed-tools=write_file".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: Some(
                    ["glob_search", "read_file", "write_file"]
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                ),
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn rejects_unknown_allowed_tools() {
        let error = parse_args(&["--allowedTools".to_string(), "teleport".to_string()])
            .expect_err("tool should be rejected");
        assert!(error.contains("unsupported tool in --allowedTools: teleport"));
    }

    #[test]
    fn parses_system_prompt_options() {
        let args = vec![
            "system-prompt".to_string(),
            "--cwd".to_string(),
            "/tmp/project".to_string(),
            "--date".to_string(),
            "2026-04-01".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::PrintSystemPrompt {
                cwd: PathBuf::from("/tmp/project"),
                date: "2026-04-01".to_string(),
            }
        );
    }

    #[test]
    fn parses_doctor_subcommand_and_modes() {
        assert_eq!(
            parse_args(&["doctor".to_string()]).expect("doctor should parse"),
            CliAction::Doctor {
                mode: None,
                model: DEFAULT_MODEL.to_string(),
            }
        );
        assert_eq!(
            parse_args(&["doctor".to_string(), "full".to_string()])
                .expect("doctor full should parse"),
            CliAction::Doctor {
                mode: Some("full".to_string()),
                model: DEFAULT_MODEL.to_string(),
            }
        );
        assert_eq!(parse_doctor_mode(None).expect("default mode"), DoctorMode::Quick);
        assert_eq!(
            parse_doctor_mode(Some("status")).expect("status mode"),
            DoctorMode::Status
        );
        assert!(parse_doctor_mode(Some("mystery")).is_err());
    }

    #[test]
    fn parses_login_and_logout_subcommands() {
        assert_eq!(
            parse_args(&["login".to_string()]).expect("login should parse"),
            CliAction::Login
        );
        assert_eq!(
            parse_args(&["models".to_string()]).expect("models should parse"),
            CliAction::Models
        );
        assert_eq!(
            parse_args(&["render-smoke".to_string(), "tool-success".to_string()])
                .expect("render-smoke should parse"),
            CliAction::RenderSmoke {
                scenario: Some("tool-success".to_string())
            }
        );
        assert_eq!(
            parse_args(&["logout".to_string()]).expect("logout should parse"),
            CliAction::Logout
        );
        assert_eq!(
            parse_args(&["init".to_string()]).expect("init should parse"),
            CliAction::Init
        );
        assert_eq!(
            parse_args(&["agents".to_string()]).expect("agents should parse"),
            CliAction::Agents { args: None }
        );
        assert_eq!(
            parse_args(&["tasks".to_string()]).expect("tasks should parse"),
            CliAction::Tasks { args: None }
        );
        assert_eq!(
            parse_args(&["skills".to_string()]).expect("skills should parse"),
            CliAction::Skills { args: None }
        );
        assert_eq!(
            parse_args(&["agents".to_string(), "--help".to_string()])
                .expect("agents help should parse"),
            CliAction::Agents {
                args: Some("--help".to_string())
            }
        );
    }

    #[test]
    fn parses_direct_agents_and_skills_slash_commands() {
        assert_eq!(
            parse_args(&["/agents".to_string()]).expect("/agents should parse"),
            CliAction::Agents { args: None }
        );
        assert_eq!(
            parse_args(&["/tasks".to_string(), "logs".to_string(), "agent-123".to_string()])
                .expect("/tasks logs should parse"),
            CliAction::Tasks {
                args: Some("logs agent-123".to_string())
            }
        );
        assert_eq!(
            parse_args(&["/skills".to_string()]).expect("/skills should parse"),
            CliAction::Skills { args: None }
        );
        assert_eq!(
            parse_args(&["/skills".to_string(), "help".to_string()])
                .expect("/skills help should parse"),
            CliAction::Skills {
                args: Some("help".to_string())
            }
        );
        let error = parse_args(&["/status".to_string()])
            .expect_err("/status should remain REPL-only when invoked directly");
        assert!(error.contains("Direct slash command unavailable"));
        assert!(error.contains("/status"));
    }

    #[test]
    fn parses_resume_flag_with_slash_command() {
        let args = vec![
            "--resume".to_string(),
            "session.json".to_string(),
            "/compact".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.json"),
                commands: vec!["/compact".to_string()],
            }
        );
    }

    #[test]
    fn parses_resume_flag_with_multiple_slash_commands() {
        let args = vec![
            "--resume".to_string(),
            "session.json".to_string(),
            "/status".to_string(),
            "/compact".to_string(),
            "/cost".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.json"),
                commands: vec![
                    "/status".to_string(),
                    "/compact".to_string(),
                    "/cost".to_string(),
                ],
            }
        );
    }

    #[test]
    fn filtered_tool_specs_respect_allowlist() {
        let allowed = ["read_file", "grep_search"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let filtered = filter_tool_specs(&GlobalToolRegistry::builtin(), Some(&allowed));
        let names = filtered
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file", "grep_search"]);
    }

    #[test]
    fn filtered_tool_specs_include_plugin_tools() {
        let filtered = filter_tool_specs(&registry_with_plugin_tool(), None);
        let names = filtered
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"plugin_echo".to_string()));
    }

    #[test]
    fn permission_policy_uses_plugin_tool_permissions() {
        let policy = permission_policy(PermissionMode::ReadOnly, &registry_with_plugin_tool());
        let required = policy.required_mode_for("plugin_echo");
        assert_eq!(required, PermissionMode::WorkspaceWrite);
    }

    #[test]
    fn shared_help_uses_resume_annotation_copy() {
        let help = commands::render_slash_command_help();
        assert!(help.contains("Slash commands"));
        assert!(help.contains("Tab completes commands inside the REPL."));
        assert!(help.contains("available via ember --resume SESSION.json"));
    }

    #[test]
    fn repl_help_includes_shared_commands_and_exit() {
        let help = render_repl_help();
        assert!(help.contains("Interactive REPL"));
        assert!(help.contains("/help"));
        assert!(help.contains("/status"));
        assert!(help.contains("/doctor"));
        assert!(help.contains("/model [model|list]"));
        assert!(help.contains("/permissions [read-only|workspace-write|danger-full-access]"));
        assert!(help.contains("/clear [--confirm]"));
        assert!(help.contains("/cost"));
        assert!(help.contains("/resume <session-path>"));
        assert!(help.contains("/config [env|hooks|model|plugins]"));
        assert!(help.contains("/memory"));
        assert!(help.contains("/init"));
        assert!(help.contains("/diff"));
        assert!(help.contains("/version"));
        assert!(help.contains("/export [file]"));
        assert!(help.contains("/session [list|switch <session-id>]"));
        assert!(help.contains(
            "/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]"
        ));
        assert!(help.contains("aliases: /plugins, /marketplace"));
        assert!(help.contains("/agents"));
        assert!(help.contains("/skills"));
        assert!(help.contains("/tasks [list|show <id>|logs <id>|attach <id>|stop <id>]"));
        assert!(help.contains("/exit"));
        assert!(help.contains("Tab cycles slash command matches"));
    }

    #[test]
    fn completion_candidates_include_repl_only_exit_commands() {
        let candidates = slash_command_completion_candidates();
        assert!(candidates.contains(&"/help".to_string()));
        assert!(candidates.contains(&"/vim".to_string()));
        assert!(candidates.contains(&"/exit".to_string()));
        assert!(candidates.contains(&"/quit".to_string()));
    }

    #[test]
    fn unknown_repl_command_suggestions_include_repl_shortcuts() {
        let rendered = render_unknown_repl_command("exi");
        assert!(rendered.contains("Unknown slash command"));
        assert!(rendered.contains("/exit"));
        assert!(rendered.contains("/help"));
    }

    #[test]
    fn resume_supported_command_list_matches_expected_surface() {
        let names = resume_supported_slash_commands()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "help", "status", "compact", "clear", "cost", "config", "memory", "init", "diff",
                "version", "export", "agents", "skills",
            ]
        );
    }

    #[test]
    fn resume_report_uses_sectioned_layout() {
        let report = format_resume_report("session.json", 14, 6);
        assert!(report.contains("Session resumed"));
        assert!(report.contains("Session file     session.json"));
        assert!(report.contains("History          14 messages | 6 turns"));
        assert!(report.contains("/status | /diff | /export"));
    }

    #[test]
    fn compact_report_uses_structured_output() {
        let compacted = format_compact_report(8, 5, false);
        assert!(compacted.contains("Compact"));
        assert!(compacted.contains("Result           compacted"));
        assert!(compacted.contains("Messages removed 8"));
        assert!(compacted.contains("Use /status"));
        let skipped = format_compact_report(0, 3, true);
        assert!(skipped.contains("Result           skipped"));
    }

    #[test]
    fn cost_report_uses_sectioned_layout() {
        let report = format_cost_report(runtime::TokenUsage {
            input_tokens: 20,
            output_tokens: 8,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 1,
        });
        assert!(report.contains("Cost"));
        assert!(report.contains("Input tokens     20"));
        assert!(report.contains("Output tokens    8"));
        assert!(report.contains("Cache create     3"));
        assert!(report.contains("Cache read       1"));
        assert!(report.contains("Total tokens     32"));
        assert!(report.contains("/compact"));
    }

    #[test]
    fn permissions_report_uses_sectioned_layout() {
        let report = format_permissions_report("workspace-write");
        assert!(report.contains("Permissions"));
        assert!(report.contains("Active mode      workspace-write"));
        assert!(report.contains("Effect           Editing tools can modify files in the workspace"));
        assert!(report.contains("Modes"));
        assert!(report.contains("read-only          - available Read/search tools only"));
        assert!(report.contains("workspace-write    * current   Edit files inside the workspace"));
        assert!(report.contains("danger-full-access - available Unrestricted tool access"));
    }

    #[test]
    fn permissions_switch_report_is_structured() {
        let report = format_permissions_switch_report("read-only", "workspace-write");
        assert!(report.contains("Permissions updated"));
        assert!(report.contains("Previous mode    read-only"));
        assert!(report.contains("Active mode      workspace-write"));
        assert!(report.contains("Applies to       Subsequent tool calls in this REPL"));
    }

    #[test]
    fn init_help_mentions_direct_subcommand() {
        let mut help = Vec::new();
        print_help_to(&mut help).expect("help should render");
        let help = String::from_utf8(help).expect("help should be utf8");
        assert!(help.contains("ember init"));
        assert!(help.contains("ember models"));
        assert!(help.contains("ember doctor"));
        assert!(help.contains("ember agents"));
        assert!(help.contains("ember tasks"));
        assert!(help.contains("ember skills"));
        assert!(help.contains("ember /skills"));
        assert!(help.contains("text, json, or ndjson"));
        assert!(help.contains("ember --output-format ndjson -p \"status\""));
    }

    #[test]
    fn task_reports_mark_current_session_and_reconcile_dead_workers() {
        let _lock = env_lock();
        let root = temp_test_dir("task-report");
        let task_dir = root.join(".ember-agents");
        fs::create_dir_all(&task_dir).expect("task dir should exist");
        let manifest_path = task_dir.join("agent-123.json");
        let output_path = task_dir.join("agent-123.md");
        fs::write(&output_path, "# Agent Task\n").expect("log should exist");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-123",
                "name": "ship-audit",
                "description": "Audit the branch",
                "status": "running",
                "outputFile": output_path.display().to_string(),
                "manifestFile": manifest_path.display().to_string(),
                "createdAt": "1710000000",
                "startedAt": "1710000000",
                "updatedAt": "1710000000",
                "parentSessionId": "session-current",
                "workerPid": 4294967295u64
            }))
            .expect("manifest json"),
        )
        .expect("manifest should exist");

        let tasks = super::task_mgmt::load_task_manifests(&root).expect("tasks should load");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status(), "interrupted");

        let report = super::task_mgmt::render_task_list_report(&tasks, Some("session-current"));
        assert!(report.contains("this-session"));
        assert!(report.contains("interrupted"));
        assert!(report.contains("/tasks attach <id>"));

        cleanup_temp_dir(&root);
    }

    #[test]
    fn task_stop_requests_persist_stopping_metadata() {
        let _lock = env_lock();
        let root = temp_test_dir("task-stop");
        let task_dir = root.join(".ember-agents");
        fs::create_dir_all(&task_dir).expect("task dir should exist");
        let manifest_path = task_dir.join("agent-456.json");
        let output_path = task_dir.join("agent-456.md");
        fs::write(&output_path, "# Agent Task\n").expect("log should exist");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-456",
                "name": "ship-review",
                "description": "Review the branch",
                "status": "running",
                "outputFile": output_path.display().to_string(),
                "manifestFile": manifest_path.display().to_string(),
                "createdAt": "1710000000",
                "startedAt": "1710000000",
                "updatedAt": "1710000000",
                "workerPid": std::process::id()
            }))
            .expect("manifest json"),
        )
        .expect("manifest should exist");

        let report = super::task_mgmt::request_task_stop(&root, "agent-456").expect("stop should succeed");
        assert!(report.contains("stop requested"));
        let persisted: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&manifest_path).expect("manifest should persist"),
        )
        .expect("persisted manifest json");
        assert_eq!(persisted["status"], "stopping");
        assert!(persisted["stopRequestedAt"].as_str().is_some());
        assert_eq!(persisted["stopReason"], "Requested from /tasks stop");
        assert_eq!(persisted["activity"].as_array().expect("activity array").len(), 1);
        assert_eq!(persisted["activity"][0]["kind"], "stop-requested");
        assert_eq!(persisted["activity"][0]["status"], "stopping");

        cleanup_temp_dir(&root);
    }

    #[test]
    fn agent_tool_results_render_as_task_cards() {
        let output = json!({
            "agentId": "agent-1234567890",
            "status": "running",
            "description": "Audit the branch",
            "outputFile": "/tmp/agent-1234567890.md",
            "parentSessionId": "session-1234567890abcdef",
            "statusDetail": "Queued for background execution"
        })
        .to_string();

        let rendered = format_tool_result("Agent", &output, false);

        assert!(rendered.contains("task agent-123456"));
        assert!(rendered.contains("Audit the branch"));
        assert!(rendered.contains("Queued for background execution"));
        assert!(rendered.contains("log: /tmp/agent-1234567890.md"));
        assert!(rendered.contains("follow: /tasks attach agent-123456"));
    }

    #[test]
    fn doctor_status_report_handles_missing_and_cached_entries() {
        let empty = format_doctor_status(&DoctorCache::default(), "qwen3:8b");
        assert!(empty.contains("Quick            not yet run"));
        assert!(empty.contains("Full             not yet run"));

        let cache = DoctorCache {
            quick: Some(DoctorReport {
                scope: "quick".to_string(),
                cache_key: "quick:0.1.0:qwen3:8b".to_string(),
                ran_at: "12345".to_string(),
                target: "qwen3:8b".to_string(),
                binary: "/tmp/ember".to_string(),
                status: DoctorCheckStatus::Warn,
                checks: vec![DoctorCheck {
                    name: "tool calling".to_string(),
                    status: DoctorCheckStatus::Warn,
                    detail: "no real tool call".to_string(),
                }],
            }),
            full: None,
        };
        let rendered = format_doctor_status(&cache, "qwen3:8b");
        assert!(rendered.contains("Diagnostics cache"));
        assert!(rendered.contains("Quick            WARN"));
    }

    #[test]
    fn model_report_uses_sectioned_layout() {
        let report = format_model_report("sonnet", 12, 4);
        assert!(report.contains("Model"));
        assert!(report.contains("Current          sonnet"));
        assert!(report.contains("Session          12 messages | 4 turns"));
        assert!(report.contains("Aliases"));
        assert!(report.contains("grok-mini        grok-3-mini"));
        assert!(report.contains("/model list      List available models"));
        assert!(report.contains("/model <name>    Switch models for this REPL session"));
    }

    #[test]
    fn available_models_report_uses_current_model_and_shortcuts() {
        let report = format_available_models_report(
            "qwen3:8b",
            &AvailableModelCatalog {
                ollama_models: vec!["qwen3:4b".to_string(), "qwen3:8b".to_string()],
                ollama_status: "reachable - 2 local model(s) detected".to_string(),
            },
        );

        assert!(report.contains("Available models"));
        assert!(report.contains("Ollama state     reachable - 2 local model(s) detected"));
        assert!(report.contains("- qwen3:4b"));
        assert!(report.contains("* qwen3:8b"));
        assert!(report.contains("Cloud shortcuts"));
        assert!(report.contains("Routing shortcuts"));
    }

    #[test]
    fn model_switch_report_preserves_context_summary() {
        let report = format_model_switch_report("sonnet", "opus", 9);
        assert!(report.contains("Model updated"));
        assert!(report.contains("Previous         sonnet"));
        assert!(report.contains("Current          opus"));
        assert!(report.contains("Preserved        9 messages"));
    }

    #[test]
    fn status_line_reports_model_and_token_totals() {
        let status = format_status_report(
            "sonnet",
            StatusUsage {
                message_count: 7,
                turns: 3,
                latest: runtime::TokenUsage {
                    input_tokens: 5,
                    output_tokens: 4,
                    cache_creation_input_tokens: 1,
                    cache_read_input_tokens: 0,
                },
                cumulative: runtime::TokenUsage {
                    input_tokens: 20,
                    output_tokens: 8,
                    cache_creation_input_tokens: 2,
                    cache_read_input_tokens: 1,
                },
                estimated_tokens: 128,
            },
            "workspace-write",
            &super::StatusContext {
                cwd: PathBuf::from("/tmp/project"),
                session_path: Some(PathBuf::from("session.json")),
                loaded_config_files: 2,
                discovered_config_files: 3,
                memory_file_count: 4,
                project_root: Some(PathBuf::from("/tmp")),
                git_branch: Some("main".to_string()),
            },
        );
        assert!(status.contains("Session"));
        assert!(status.contains("Model            sonnet"));
        assert!(status.contains("Permissions      workspace-write"));
        assert!(status.contains("Activity         7 messages | 3 turns"));
        assert!(status.contains("Tokens           est 128 | latest 10 | total 31"));
        assert!(status.contains("Folder           /tmp/project"));
        assert!(status.contains("Project root     /tmp"));
        assert!(status.contains("Git branch       main"));
        assert!(status.contains("Session file     session.json"));
        assert!(status.contains("Config files     loaded 2/3"));
        assert!(status.contains("Memory files     4"));
        assert!(status.contains("/session list"));
    }

    #[test]
    fn config_report_supports_section_views() {
        let report = render_config_report(Some("env")).expect("config report should render");
        assert!(report.contains("Merged section: env"));
        let ui_report = render_config_report(Some("ui")).expect("ui config report should render");
        assert!(ui_report.contains("Merged section: ui"));
        let plugins_report =
            render_config_report(Some("plugins")).expect("plugins config report should render");
        assert!(plugins_report.contains("Merged section: plugins"));
    }

    #[test]
    fn memory_report_uses_sectioned_layout() {
        let report = render_memory_report().expect("memory report should render");
        assert!(report.contains("Memory"));
        assert!(report.contains("Working directory"));
        assert!(report.contains("Instruction files"));
        assert!(report.contains("Discovered files"));
    }

    #[test]
    fn config_report_uses_sectioned_layout() {
        let report = render_config_report(None).expect("config report should render");
        assert!(report.contains("Config"));
        assert!(report.contains("Discovered files"));
        assert!(report.contains("Merged JSON"));
    }

    #[test]
    fn parses_git_status_metadata() {
        let (root, branch) = parse_git_status_metadata(Some(
            "## rcc/cli...origin/rcc/cli
 M src/main.rs",
        ));
        assert_eq!(branch.as_deref(), Some("rcc/cli"));
        let _ = root;
    }

    #[test]
    fn status_context_reads_real_workspace_metadata() {
        let context = status_context(None).expect("status context should load");
        assert!(context.cwd.is_absolute());
        // 8 config paths: 2 user + 3 ember (project/project-settings/local) + 3 claw (fallback)
        assert_eq!(context.discovered_config_files, 8);
        assert!(context.loaded_config_files <= context.discovered_config_files);
    }

    #[test]
    fn normalizes_supported_permission_modes() {
        assert_eq!(normalize_permission_mode("read-only"), Some("read-only"));
        assert_eq!(
            normalize_permission_mode("workspace-write"),
            Some("workspace-write")
        );
        assert_eq!(
            normalize_permission_mode("danger-full-access"),
            Some("danger-full-access")
        );
        assert_eq!(normalize_permission_mode("unknown"), None);
    }

    #[test]
    fn clear_command_requires_explicit_confirmation_flag() {
        assert_eq!(
            SlashCommand::parse("/clear"),
            Some(SlashCommand::Clear { confirm: false })
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Some(SlashCommand::Clear { confirm: true })
        );
    }

    #[test]
    fn parses_resume_and_config_slash_commands() {
        assert_eq!(
            SlashCommand::parse("/resume saved-session.json"),
            Some(SlashCommand::Resume {
                session_path: Some("saved-session.json".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Some(SlashCommand::Clear { confirm: true })
        );
        assert_eq!(
            SlashCommand::parse("/config"),
            Some(SlashCommand::Config { section: None })
        );
        assert_eq!(
            SlashCommand::parse("/config env"),
            Some(SlashCommand::Config {
                section: Some("env".to_string())
            })
        );
        assert_eq!(SlashCommand::parse("/memory"), Some(SlashCommand::Memory));
        assert_eq!(SlashCommand::parse("/init"), Some(SlashCommand::Init));
    }

    #[test]
    fn init_template_mentions_detected_rust_workspace() {
        let rendered = crate::init::render_init_ember_md(std::path::Path::new("."));
        assert!(rendered.contains("# EMBER.md"));
        assert!(rendered.contains("cargo clippy --workspace --all-targets -- -D warnings"));
    }

    #[test]
    fn converts_tool_roundtrip_messages() {
        let messages = vec![
            ConversationMessage::user_text("hello"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "bash".to_string(),
                input: "{\"command\":\"pwd\"}".to_string(),
            }]),
            ConversationMessage {
                role: MessageRole::Tool,
                blocks: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    tool_name: "bash".to_string(),
                    output: "ok".to_string(),
                    is_error: false,
                }],
                usage: None,
            },
        ];

        let converted = super::convert_messages(&messages);
        assert_eq!(converted.len(), 3);
        assert_eq!(converted[1].role, "assistant");
        assert_eq!(converted[2].role, "user");
    }
    #[test]
    fn repl_help_mentions_history_completion_and_multiline() {
        let help = render_repl_help();
        assert!(help.contains("Up/Down"));
        assert!(help.contains("Tab cycles"));
        assert!(help.contains("Shift+Enter or Ctrl+J"));
    }

    #[test]
    fn tool_rendering_helpers_compact_output() {
        let start = format_tool_call_start("read_file", r#"{"path":"src/main.rs"}"#);
        assert!(start.contains("read_file"));
        assert!(start.contains("src/main.rs"));
        assert!(start.contains("╭─ [tool] read_file"));
        assert!(start.contains("╰"));

        let done = format_tool_result(
            "read_file",
            r#"{"file":{"filePath":"src/main.rs","content":"hello","numLines":1,"startLine":1,"totalLines":1}}"#,
            false,
        );
        assert!(done.contains("read_file: read src/main.rs"));
        assert!(done.contains("hello"));
        assert!(done.contains("╭─ [ok] read_file"));
        assert!(done.contains("╰"));
    }

    #[test]
    fn tool_rendering_truncates_large_read_output_for_display_only() {
        let content = (0..200)
            .map(|index| format!("line {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let output = json!({
            "file": {
                "filePath": "src/main.rs",
                "content": content,
                "numLines": 200,
                "startLine": 1,
                "totalLines": 200
            }
        })
        .to_string();

        let rendered = format_tool_result("read_file", &output, false);

        assert!(rendered.contains("line 000"));
        assert!(rendered.contains("line 079"));
        assert!(!rendered.contains("line 199"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("line 199"));
    }

    #[test]
    fn tool_rendering_shows_injected_file_context() {
        let output = json!({
            "file": {
                "filePath": "src/main.rs",
                "content": "fn main() {}",
                "numLines": 1,
                "startLine": 1,
                "totalLines": 1
            },
            "injectedContext": "Context from src/README:\nKeep CLI-facing logic thin and prefer shared helpers."
        })
        .to_string();

        let rendered = format_tool_result("read_file", &output, false);

        assert!(rendered.contains("context from nearby README"));
        assert!(rendered.contains("Keep CLI-facing logic thin"));
    }

    #[test]
    fn tool_call_rendering_summarizes_mcp_and_lsp_inputs() {
        let mcp = format_tool_call_start(
            "MCPTool",
            r#"{"server_name":"alpha","tool_name":"echo","arguments":{"text":"hello"}}"#,
        );
        assert!(mcp.contains("mcp: alpha::echo"));
        assert!(mcp.contains(r#"{"text":"hello"}"#));

        let lsp = format_tool_call_start(
            "LSPTool",
            r#"{"action":"definition","file_path":"src/main.rs","line":12,"character":4}"#,
        );
        assert!(lsp.contains("lsp: definition"));
        assert!(lsp.contains("src/main.rs:12:4"));
    }

    #[test]
    fn mcp_tool_results_render_jsonrpc_success_cards() {
        let output = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": "alpha:hello"
                    }
                ],
                "structuredContent": {
                    "server": "alpha",
                    "echoed": "hello",
                    "initializeCount": 1
                },
                "isError": false
            }
        })
        .to_string();

        let rendered = format_tool_result("MCPTool", &output, false);

        assert!(rendered.contains("server: alpha"));
        assert!(rendered.contains("alpha:hello"));
        assert!(rendered.contains("structured content"));
        assert!(rendered.contains("\"echoed\": \"hello\""));
    }

    #[test]
    fn mcp_resource_tools_render_resource_summaries() {
        let list_output = json!({
            "server": "alpha",
            "resources": [
                {
                    "uri": "file://guide.txt",
                    "name": "guide",
                    "description": "Guide text"
                }
            ],
            "count": 1
        })
        .to_string();

        let list_rendered = format_tool_result("ListMcpResources", &list_output, false);
        assert!(list_rendered.contains("server: alpha"));
        assert!(list_rendered.contains("1 resource available"));
        assert!(list_rendered.contains("guide — file://guide.txt"));

        let read_output = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "result": {
                "contents": [
                    {
                        "uri": "file://guide.txt",
                        "mimeType": "text/plain",
                        "text": "contents for file://guide.txt"
                    }
                ]
            }
        })
        .to_string();

        let read_rendered = format_tool_result("ReadMcpResource", &read_output, false);
        assert!(read_rendered.contains("1 resource entry read"));
        assert!(read_rendered.contains("file://guide.txt · text/plain"));
        assert!(read_rendered.contains("contents for file://guide.txt"));
    }

    #[test]
    fn lsp_tool_results_render_action_hints_and_examples() {
        let output = json!({
            "action": "diagnostics",
            "hint": "Use the bash tool to run linting commands directly",
            "examples": [
                "cargo clippy --workspace 2>&1 | head -50",
                "npx tsc --noEmit 2>&1 | head -50"
            ]
        })
        .to_string();

        let rendered = format_tool_result("LSPTool", &output, false);

        assert!(rendered.contains("action: diagnostics"));
        assert!(rendered.contains("Use the bash tool to run linting commands directly"));
        assert!(rendered.contains("examples"));
        assert!(rendered.contains("cargo clippy --workspace"));
    }

    #[test]
    fn read_file_output_is_enriched_with_nearby_readme_context() {
        let root = temp_test_dir("file-context");
        fs::create_dir_all(root.join("src")).expect("create src dir");
        fs::write(
            root.join("src/README.md"),
            "# Source notes\nPrefer the README guidance when reading files in this directory.\n",
        )
        .expect("write readme");
        let file_path = root.join("src/main.rs");
        fs::write(&file_path, "fn main() {}\n").expect("write file");

        let output = json!({
            "type": "text",
            "file": {
                "filePath": file_path.to_string_lossy(),
                "content": "fn main() {}",
                "numLines": 1,
                "startLine": 1,
                "totalLines": 1
            }
        })
        .to_string();

        let enriched = inject_file_context_into_tool_output(
            "read_file",
            &json!({ "path": file_path.to_string_lossy().to_string() }),
            &output,
        );

        let parsed: serde_json::Value =
            serde_json::from_str(&enriched).expect("enriched output should stay valid json");
        let injected = parsed
            .get("injectedContext")
            .and_then(serde_json::Value::as_str)
            .expect("file context should be injected");
        assert!(injected.contains("Source notes"));
        assert!(injected.contains("Prefer the README guidance"));

        cleanup_temp_dir(&root);
    }

    #[test]
    fn tool_rendering_truncates_large_bash_output_for_display_only() {
        let stdout = (0..120)
            .map(|index| format!("stdout {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let output = json!({
            "stdout": stdout,
            "stderr": "",
            "returnCodeInterpretation": "completed successfully"
        })
        .to_string();

        let rendered = format_tool_result("bash", &output, false);

        assert!(rendered.contains("stdout 000"));
        assert!(rendered.contains("stdout 059"));
        assert!(!rendered.contains("stdout 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("stdout 119"));
    }

    #[test]
    fn tool_rendering_truncates_generic_long_output_for_display_only() {
        let items = (0..120)
            .map(|index| format!("payload {index:03}"))
            .collect::<Vec<_>>();
        let output = json!({
            "summary": "plugin payload",
            "items": items,
        })
        .to_string();

        let rendered = format_tool_result("plugin_echo", &output, false);

        assert!(rendered.contains("plugin_echo"));
        assert!(rendered.contains("payload 000"));
        assert!(rendered.contains("payload 040"));
        assert!(!rendered.contains("payload 080"));
        assert!(!rendered.contains("payload 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("payload 119"));
    }

    #[test]
    fn tool_rendering_truncates_raw_generic_output_for_display_only() {
        let output = (0..120)
            .map(|index| format!("raw {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");

        let rendered = format_tool_result("plugin_echo", &output, false);

        assert!(rendered.contains("plugin_echo"));
        assert!(rendered.contains("raw 000"));
        assert!(rendered.contains("raw 059"));
        assert!(!rendered.contains("raw 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("raw 119"));
    }

    #[test]
    fn ultraplan_progress_lines_include_phase_step_and_elapsed_status() {
        let snapshot = InternalPromptProgressState {
            command_label: "Ultraplan",
            task_label: "ship plugin progress".to_string(),
            step: 3,
            phase: "running read_file".to_string(),
            detail: Some("reading rust/crates/claw-cli/src/main.rs".to_string()),
            saw_final_text: false,
        };

        let started = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Started,
            &snapshot,
            Duration::from_secs(0),
            None,
        );
        let heartbeat = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Heartbeat,
            &snapshot,
            Duration::from_secs(9),
            None,
        );
        let completed = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Complete,
            &snapshot,
            Duration::from_secs(12),
            None,
        );
        let failed = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Failed,
            &snapshot,
            Duration::from_secs(12),
            Some("network timeout"),
        );

        assert!(started.contains("planning started"));
        assert!(started.contains("current step 3"));
        assert!(heartbeat.contains("heartbeat"));
        assert!(heartbeat.contains("9s elapsed"));
        assert!(heartbeat.contains("phase running read_file"));
        assert!(completed.contains("completed"));
        assert!(completed.contains("3 steps total"));
        assert!(failed.contains("failed"));
        assert!(failed.contains("network timeout"));
    }

    #[test]
    fn describe_tool_progress_summarizes_known_tools() {
        assert_eq!(
            describe_tool_progress("read_file", r#"{"path":"src/main.rs"}"#),
            "reading src/main.rs"
        );
        assert!(
            describe_tool_progress("bash", r#"{"command":"cargo test -p claw-cli"}"#)
                .contains("cargo test -p claw-cli")
        );
        assert_eq!(
            describe_tool_progress("grep_search", r#"{"pattern":"ultraplan","path":"rust"}"#),
            "grep `ultraplan` in rust"
        );
    }

    #[test]
    fn push_output_block_renders_markdown_text() {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut pending_tool = None;

        push_output_block(
            OutputContentBlock::Text {
                text: "# Heading".to_string(),
            },
            &mut out,
            &mut events,
            &mut pending_tool,
            false,
        )
        .expect("text block should render");

        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("Heading"));
        assert!(rendered.contains('\u{1b}'));
    }

    #[test]
    fn push_output_block_skips_empty_object_prefix_for_tool_streams() {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut pending_tool = None;

        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({}),
            },
            &mut out,
            &mut events,
            &mut pending_tool,
            true,
        )
        .expect("tool block should accumulate");

        assert!(events.is_empty());
        assert_eq!(
            pending_tool,
            Some(("tool-1".to_string(), "read_file".to_string(), String::new(),))
        );
    }

    #[test]
    fn response_to_events_preserves_empty_object_json_input_outside_streaming() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-1".to_string(),
                kind: "message".to_string(),
                model: "claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "read_file".to_string(),
                    input: json!({}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::ToolUse { name, input, .. }
                if name == "read_file" && input == "{}"
        ));
    }

    #[test]
    fn response_to_events_preserves_non_empty_json_input_outside_streaming() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-2".to_string(),
                kind: "message".to_string(),
                model: "claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::ToolUse {
                    id: "tool-2".to_string(),
                    name: "read_file".to_string(),
                    input: json!({ "path": "rust/Cargo.toml" }),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::ToolUse { name, input, .. }
                if name == "read_file" && input == "{\"path\":\"rust/Cargo.toml\"}"
        ));
    }

    #[test]
    fn ndjson_prompt_events_preserve_tool_roundtrip_order() {
        let summary = TurnSummary {
            assistant_messages: vec![
                ConversationMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![
                        ContentBlock::Text {
                            text: "Need a tool".to_string(),
                        },
                        ContentBlock::ToolUse {
                            id: "tool-1".to_string(),
                            name: "bash".to_string(),
                            input: "{\"command\":\"printf TOOL_OK\"}".to_string(),
                        },
                    ],
                    usage: None,
                },
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "Done".to_string(),
                }]),
            ],
            tool_results: vec![ConversationMessage::tool_result(
                "tool-1".to_string(),
                "bash".to_string(),
                "TOOL_OK".to_string(),
                false,
            )],
            iterations: 2,
            usage: TokenUsage {
                input_tokens: 12,
                output_tokens: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };

        let events = prompt_summary_ndjson_events("qwen3:4b", &summary);
        let event_types = events
            .iter()
            .map(|event| event["type"].as_str().unwrap_or(""))
            .collect::<Vec<_>>();

        assert_eq!(
            event_types,
            vec![
                "turn_started",
                "assistant_text",
                "tool_use",
                "tool_result",
                "assistant_text",
                "usage",
                "turn_completed",
            ]
        );
        assert_eq!(events[2]["name"], "bash");
        assert_eq!(events[3]["tool_use_id"], "tool-1");
        assert_eq!(events[6]["message"], "Done");
        assert_eq!(events[6]["iterations"], 2);
    }

    #[test]
    fn prompt_summary_payload_sanitizes_and_falls_back_to_tool_errors() {
        let denial = "tool 'bash' requires approval to escalate from workspace-write to danger-full-access; machine-readable json mode cannot prompt interactively.";
        let summary = TurnSummary {
            assistant_messages: vec![ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "</think>\n\n".to_string(),
            }])],
            tool_results: vec![
                ConversationMessage::tool_result(
                    "tool-1".to_string(),
                    "bash".to_string(),
                    denial.to_string(),
                    true,
                ),
                ConversationMessage::tool_result(
                    "tool-2".to_string(),
                    "bash".to_string(),
                    denial.to_string(),
                    true,
                ),
            ],
            iterations: 2,
            usage: TokenUsage::default(),
        };

        let payload = prompt_summary_payload("qwen3:4b", &summary);
        let message = payload["message"].as_str().expect("message should be present");

        assert!(!message.contains("</think>"));
        assert!(message.contains("cannot prompt interactively"));
        assert!(message.contains("repeated 2 times"));
    }

    #[test]
    fn ndjson_prompt_events_skip_empty_assistant_text_after_sanitizing() {
        let denial = "tool 'bash' requires approval to escalate from workspace-write to danger-full-access; machine-readable json mode cannot prompt interactively.";
        let summary = TurnSummary {
            assistant_messages: vec![ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "</think>\n\n".to_string(),
            }])],
            tool_results: vec![ConversationMessage::tool_result(
                "tool-1".to_string(),
                "bash".to_string(),
                denial.to_string(),
                true,
            )],
            iterations: 1,
            usage: TokenUsage::default(),
        };

        let events = prompt_summary_ndjson_events("qwen3:4b", &summary);
        let event_types = events
            .iter()
            .map(|event| event["type"].as_str().unwrap_or(""))
            .collect::<Vec<_>>();

        assert_eq!(event_types, vec!["turn_started", "tool_result", "usage", "turn_completed"]);
        assert!(events[3]["message"]
            .as_str()
            .expect("turn_completed message should exist")
            .contains("cannot prompt interactively"));
    }

    #[test]
    fn sanitize_assistant_text_strips_terminal_sequences_and_thinking_tags() {
        let sanitized = sanitize_assistant_text("<think>\u{1b}[31mHello\u{7}</think>\n");
        assert_eq!(sanitized, "Hello");
    }

    #[test]
    fn prompt_summary_payload_prefers_mcp_text_over_raw_json_blob() {
        let summary = TurnSummary {
            assistant_messages: vec![ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "</think>\n\n".to_string(),
            }])],
            tool_results: vec![ConversationMessage::tool_result(
                "tool-1".to_string(),
                "MCPTool".to_string(),
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": "alpha:hello"
                            }
                        ],
                        "structuredContent": {
                            "server": "alpha",
                            "echoed": "hello"
                        },
                        "isError": false
                    }
                })
                .to_string(),
                false,
            )],
            iterations: 1,
            usage: TokenUsage::default(),
        };

        let payload = prompt_summary_payload("qwen3:4b", &summary);
        let message = payload["message"].as_str().expect("message should exist");

        assert_eq!(message, "alpha:hello");
        assert!(!message.contains("jsonrpc"));
        assert!(!message.contains("structuredContent"));
    }

    #[test]
    fn prompt_summary_payload_prefers_lsp_hint_and_sanitizes_it() {
        let summary = TurnSummary {
            assistant_messages: vec![ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "</think>\n\n".to_string(),
            }])],
            tool_results: vec![ConversationMessage::tool_result(
                "tool-1".to_string(),
                "LSPTool".to_string(),
                json!({
                    "action": "definition",
                    "file": "src/main.rs",
                    "line": 12,
                    "character": 4,
                    "hint": "\u{1b}[32mUse grep_search to find the definition\u{1b}[0m"
                })
                .to_string(),
                false,
            )],
            iterations: 1,
            usage: TokenUsage::default(),
        };

        let payload = prompt_summary_payload("qwen3:4b", &summary);
        let message = payload["message"].as_str().expect("message should exist");

        assert!(message.contains("lsp definition"));
        assert!(message.contains("src/main.rs:12:4"));
        assert!(message.contains("Use grep_search to find the definition"));
        assert!(!message.contains("\u{1b}"));
    }

    #[test]
    fn response_to_events_ignores_thinking_blocks() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-3".to_string(),
                kind: "message".to_string(),
                model: "claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    OutputContentBlock::Thinking {
                        thinking: "step 1".to_string(),
                        signature: Some("sig_123".to_string()),
                    },
                    OutputContentBlock::Text {
                        text: "Final answer".to_string(),
                    },
                ],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::TextDelta(text) if text == "Final answer"
        ));
        assert!(!String::from_utf8(out).expect("utf8").contains("step 1"));
    }

    #[test]
    fn format_thinking_preview_sanitizes_and_truncates() {
        assert_eq!(format_thinking_preview("\n\t\r"), None);

        let sanitized = format_thinking_preview("<think>step\u{1b}[31m 1\n\u{7}done</think>")
            .expect("sanitized preview should exist");
        assert_eq!(sanitized, "step 1 done");

        let long_preview = format_thinking_preview(&"x".repeat(THINKING_PREVIEW_MAX_CHARS + 24))
            .expect("long preview should exist");
        assert!(long_preview.contains(" … "));
        assert!(long_preview.chars().count() <= THINKING_PREVIEW_MAX_CHARS);
    }

    #[test]
    fn format_thinking_section_renders_boxed_verbose_block() {
        let renderer = crate::render::TerminalRenderer::new();
        let section = format_thinking_section(
            &renderer,
            "<think>## Plan\n\n- inspect files\n- render output</think>",
        )
        .expect("section should render");

        let plain = strip_terminal_escape_sequences(&section);
        assert!(plain.contains("╭─ [thinking]"));
        assert!(plain.contains("Plan"));
        assert!(plain.contains("• inspect files"));
        assert!(plain.contains("• render output"));
        assert!(!plain.contains("<think>"));
        assert!(plain.contains("╰"));
    }
}
