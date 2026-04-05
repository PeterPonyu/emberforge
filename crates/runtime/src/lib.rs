pub mod agent_loader;
pub mod bash_security;
mod bash;
mod bootstrap;
mod compact;
pub mod cron;
mod config;
mod conversation;
mod file_ops;
pub mod git;
mod hooks;
pub mod model_profiles;
pub mod model_router;
mod json;
pub mod memory;
mod mcp;
mod mcp_client;
mod mcp_stdio;
mod oauth;
mod permissions;
mod prompt;
mod remote;
pub mod sandbox;
mod session;
pub mod cost_tracker;
mod usage;

pub use lsp::{
    FileDiagnostics, LspContextEnrichment, LspError, LspManager, LspServerConfig,
    SymbolLocation, WorkspaceDiagnostics,
};
pub use bash::{execute_bash, BashCommandInput, BashCommandOutput};
pub use git::{
    create_worktree, find_git_root, find_merge_base, get_branch, get_changed_files,
    get_default_branch, get_git_state, get_github_repo, get_head, get_remote_url,
    get_worktree_count, has_unpushed_commits, is_at_git_root, is_bare_repo, is_clean,
    is_in_git_repo, is_shallow_clone, list_worktrees, parse_github_remote, remove_worktree,
    safe_stash, stash_pop, FileChangeType, FileStatus, GitState, WorktreeInfo,
};
pub use agent_loader::{
    discover_agents, find_agent, list_agent_summaries, load_agents_from_dir, resolve_agent_tools,
    build_agent_prompt, project_agents_dir, user_agents_dir,
    AgentDefinition, AgentMcpServerSpec, AgentSource, AgentSummary, IsolationMode, MemoryScope,
};
pub use bash_security::{validate_bash_command, SecurityVerdict};
pub use cron::{
    create_task, delete_task, describe_schedule, format_task_summary, load_durable_tasks,
    parse_cron, save_durable_tasks, schedule_matches, tick, CronParseError, CronSchedule,
    ScheduledTask, SchedulerTickResult,
};
pub use memory::{
    build_memory_manifest, build_memory_prompt, ensure_memory_dir, load_entrypoint,
    parse_frontmatter, project_memory_dir, scan_memory_dir, user_memory_dir, MemoryConfig,
    MemoryFile, MemoryFrontmatter, MemoryIndex, MemoryType,
};
pub use bootstrap::{BootstrapPhase, BootstrapPlan};
pub use compact::{
    auto_compact_session, calculate_token_warning, compact_session, create_pre_compact_checkpoint,
    estimate_session_tokens, format_compact_summary, get_compact_continuation_message,
    micro_compact_session, post_compact_restore_file_hints, render_checkpoint_context,
    should_auto_compact, should_compact, AutoCompactConfig, AutoCompactState, CompactionConfig,
    CompactionResult, PreCompactCheckpoint, TokenWarningLevel, TokenWarningState,
};
pub use config::{
    ConfigEntry, ConfigError, ConfigLoader, ConfigSource, McpManagedProxyServerConfig,
    McpConfigCollection, McpOAuthConfig, McpRemoteServerConfig, McpSdkServerConfig,
    McpServerConfig, McpStdioServerConfig, McpTransport, McpWebSocketServerConfig, OAuthConfig,
    ResolvedPermissionMode, RuntimeConfig, RuntimeFeatureConfig, RuntimeHookConfig,
    RuntimePluginConfig, RuntimeUiAnimationConfig, RuntimeUiAnimationMode,
    RuntimeUiBannerConfig, RuntimeUiBannerMode, RuntimeUiBannerVariant, RuntimeUiConfig,
    RuntimeUiHudConfig, RuntimeUiHudPreset, RuntimeUiMotionConfig, ScopedMcpServerConfig,
    EffortLevel, ThemeMode, CLAW_SETTINGS_SCHEMA_NAME,
};
pub use conversation::{
    ApiClient, ApiRequest, AssistantEvent, ConversationRuntime, RuntimeError, StaticToolExecutor,
    ToolError, ToolExecutor, TurnSummary,
};
pub use file_ops::{
    edit_file, glob_search, grep_search, read_file, write_file, EditFileOutput, GlobSearchOutput,
    GrepSearchInput, GrepSearchOutput, ReadFileOutput, StructuredPatchHunk, TextFilePayload,
    WriteFileOutput,
};
pub use hooks::{
    HookBackend, HookDefinition, HookEvent, HookMatchRule, HookRunResult, HookRunner,
};
pub use mcp::{
    mcp_server_signature, mcp_tool_name, mcp_tool_prefix, normalize_name_for_mcp,
    scoped_mcp_config_hash, unwrap_ccr_proxy_url,
};
pub use mcp_client::{
    McpManagedProxyTransport, McpClientAuth, McpClientBootstrap, McpClientTransport,
    McpRemoteTransport, McpSdkTransport, McpStdioTransport,
};
pub use mcp_stdio::{
    spawn_mcp_stdio_process, JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    ManagedMcpTool, McpInitializeClientInfo, McpInitializeParams, McpInitializeResult,
    McpInitializeServerInfo, McpListResourcesParams, McpListResourcesResult, McpListToolsParams,
    McpListToolsResult, McpReadResourceParams, McpReadResourceResult, McpResource,
    McpResourceContents, McpServerManager, McpServerManagerError, McpServerStatus,
    McpStdioProcess, McpTool,
    McpToolCallContent, McpToolCallParams, McpToolCallResult, UnsupportedMcpServer,
};
pub use oauth::{
    clear_oauth_credentials, code_challenge_s256, credentials_path, generate_pkce_pair,
    generate_state, load_oauth_credentials, loopback_redirect_uri, parse_oauth_callback_query,
    parse_oauth_callback_request_target, save_oauth_credentials, OAuthAuthorizationRequest,
    OAuthCallbackParams, OAuthRefreshRequest, OAuthTokenExchangeRequest, OAuthTokenSet,
    PkceChallengeMethod, PkceCodePair,
};
pub use cost_tracker::{
    format_tokens, load_session_costs, save_session_costs, CodeMetrics, CostTracker, ModelUsage,
    TimingMetrics,
};
pub use permissions::{
    DenialTracker, PermissionMode, PermissionOutcome, PermissionPolicy, PermissionPromptDecision,
    PermissionPrompter, PermissionRequest, PermissionRule, RuleBehavior, RuleSource,
};
pub use prompt::{
    load_system_prompt, prepend_bullets, ContextFile, ProjectContext, PromptBuildError,
    SystemPromptBuilder, FRONTIER_MODEL_NAME, SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
};
pub use remote::{
    inherited_upstream_proxy_env, no_proxy_list, read_token, upstream_proxy_ws_url,
    RemoteSessionContext, UpstreamProxyBootstrap, UpstreamProxyState, DEFAULT_REMOTE_BASE_URL,
    DEFAULT_SESSION_TOKEN_PATH, DEFAULT_SYSTEM_CA_BUNDLE, NO_PROXY_HOSTS, UPSTREAM_PROXY_ENV_KEYS,
};
pub use session::{ContentBlock, ConversationMessage, MessageRole, Session, SessionError};
pub use usage::{
    format_usd, pricing_for_model, ModelPricing, TokenUsage, UsageCostEstimate, UsageTracker,
};

#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
