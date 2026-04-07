//! Agent definition loader.
//!
//! Discovers and loads custom agent definitions from `.ember/agents/*.json`
//! (project-level) and `~/.ember/agents/*.json` (user-level), providing a
//! rich, JSON-driven alternative to the hard-coded subagent types.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default tool set available to a general-purpose custom agent when neither
/// `tools` nor `disallowed_tools` is specified.
const DEFAULT_AGENT_TOOLS: &[&str] = &[
    "bash",
    "read_file",
    "write_file",
    "edit_file",
    "glob_search",
    "grep_search",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "Skill",
    "ToolSearch",
    "NotebookEdit",
    "Sleep",
    "SendUserMessage",
    "Config",
    "StructuredOutput",
    "REPL",
    "PowerShell",
];

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Effort level for agent execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    Speedy,
    Balanced,
    Thorough,
}

/// Isolation mode for agent execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IsolationMode {
    /// Run in a git worktree (isolated copy of the repo).
    Worktree,
}

/// Memory scope for agent-specific memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    User,
    Project,
    Local,
}

/// Where an agent definition originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSource {
    Builtin,
    Project,
    User,
}

// ---------------------------------------------------------------------------
// MCP server specification
// ---------------------------------------------------------------------------

/// MCP server specification for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMcpServerSpec {
    /// Reference an MCP server by name (from the project/user config).
    Named(String),
    /// Inline MCP server definition.
    Inline {
        name: String,
        command: String,
        args: Option<Vec<String>>,
    },
}

// ---------------------------------------------------------------------------
// Agent definition
// ---------------------------------------------------------------------------

/// A user-defined agent loaded from a JSON file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    /// Unique identifier for this agent type (used as `subagent_type`).
    #[serde(rename = "agentType")]
    pub agent_type: String,

    /// Human-readable display name.
    #[serde(default)]
    pub display_name: Option<String>,

    /// Description of when and why to use this agent.
    #[serde(default)]
    pub when_to_use: Option<String>,

    /// System prompt / instructions for the agent.
    #[serde(default)]
    pub instructions: Option<String>,

    /// Allowlist of tool names this agent can use.  If `None`, uses default set.
    #[serde(default)]
    pub tools: Option<Vec<String>>,

    /// Denylist of tool names to exclude from the default set.
    #[serde(default)]
    pub disallowed_tools: Option<Vec<String>>,

    /// Model override (e.g. `"claude-opus-4-6"`, `"qwen3-8b"`).
    #[serde(default)]
    pub model: Option<String>,

    /// Effort level override.
    #[serde(default)]
    pub effort: Option<EffortLevel>,

    /// Maximum number of conversation turns.
    #[serde(default)]
    pub max_turns: Option<u32>,

    /// Whether to run in background by default.
    #[serde(default)]
    pub background: Option<bool>,

    /// Isolation mode.
    #[serde(default)]
    pub isolation: Option<IsolationMode>,

    /// Memory scope for this agent.
    #[serde(default)]
    pub memory: Option<MemoryScope>,

    /// MCP servers to make available to this agent.
    #[serde(default)]
    pub mcp_servers: Option<Vec<AgentMcpServerSpec>>,

    /// Initial prompt to send when the agent starts (before user prompt).
    #[serde(default)]
    pub initial_prompt: Option<String>,

    /// Whether to skip loading CLAUDE.md / EMBER.md context.
    #[serde(default)]
    pub omit_project_context: Option<bool>,

    /// Color name for terminal display.
    #[serde(default)]
    pub color: Option<String>,

    /// Source file path (populated during loading, not from JSON).
    #[serde(skip)]
    pub source_file: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Agent summary (for listing)
// ---------------------------------------------------------------------------

/// Summary info for listing available agents.
#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub agent_type: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub source: AgentSource,
    pub source_file: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Built-in agent descriptors
// ---------------------------------------------------------------------------

fn builtin_summaries() -> Vec<AgentSummary> {
    vec![
        AgentSummary {
            agent_type: "general-purpose".into(),
            display_name: Some("General Purpose".into()),
            description: Some("General-purpose coding agent with full tool access.".into()),
            source: AgentSource::Builtin,
            source_file: None,
        },
        AgentSummary {
            agent_type: "Explore".into(),
            display_name: Some("Explore".into()),
            description: Some("Fast codebase exploration agent (read-only tools).".into()),
            source: AgentSource::Builtin,
            source_file: None,
        },
        AgentSummary {
            agent_type: "Plan".into(),
            display_name: Some("Plan".into()),
            description: Some("Software architect planning agent.".into()),
            source: AgentSource::Builtin,
            source_file: None,
        },
        AgentSummary {
            agent_type: "Verification".into(),
            display_name: Some("Verification".into()),
            description: Some("Test runner and verification agent.".into()),
            source: AgentSource::Builtin,
            source_file: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Directory helpers
// ---------------------------------------------------------------------------

/// Return the project-level agents directory (`.ember/agents/` relative to cwd).
pub fn project_agents_dir() -> io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    Ok(cwd.join(".ember").join("agents"))
}

/// Return the user-level agents directory (`~/.ember/agents/`).
pub fn user_agents_dir() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))?;
    Ok(PathBuf::from(home).join(".ember").join("agents"))
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load all agent definitions from a single directory.
///
/// Reads every `*.json` file, attempts to parse each as an [`AgentDefinition`],
/// and collects the successful results.  Invalid files are skipped with a
/// warning written to stderr.
pub fn load_agents_from_dir(dir: &Path) -> io::Result<Vec<AgentDefinition>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut agents = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: could not read {}: {e}", path.display());
                continue;
            }
        };

        match serde_json::from_str::<AgentDefinition>(&content) {
            Ok(mut def) => {
                def.source_file = Some(path);
                agents.push(def);
            }
            Err(e) => {
                eprintln!("warning: failed to parse {}: {e}", path.display());
            }
        }
    }

    Ok(agents)
}

/// Discover agents from both project-level and user-level directories.
///
/// Project agents (`.ember/agents/`) take precedence over user agents
/// (`~/.ember/agents/`) when both define the same `agent_type`.
pub fn discover_agents() -> io::Result<Vec<AgentDefinition>> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();

    // Project-level first (higher precedence).
    if let Ok(project_dir) = project_agents_dir() {
        for agent in load_agents_from_dir(&project_dir)? {
            seen.insert(agent.agent_type.clone());
            result.push(agent);
        }
    }

    // User-level second.
    if let Ok(user_dir) = user_agents_dir() {
        for agent in load_agents_from_dir(&user_dir)? {
            if !seen.contains(&agent.agent_type) {
                seen.insert(agent.agent_type.clone());
                result.push(agent);
            }
        }
    }

    Ok(result)
}

/// Find a specific agent definition by `agent_type` name.
pub fn find_agent(agent_type: &str) -> io::Result<Option<AgentDefinition>> {
    let agents = discover_agents()?;
    Ok(agents.into_iter().find(|a| a.agent_type == agent_type))
}

// ---------------------------------------------------------------------------
// Tool resolution
// ---------------------------------------------------------------------------

/// Resolve the allowed tools set for a custom agent definition.
///
/// * If `tools` is specified, use that exact set.
/// * If `disallowed_tools` is specified, start from the default set and remove
///   those entries.
/// * If neither is set, return the full default tool set.
#[must_use]
pub fn resolve_agent_tools(def: &AgentDefinition) -> BTreeSet<String> {
    if let Some(ref allow) = def.tools {
        return allow.iter().cloned().collect();
    }

    let mut set: BTreeSet<String> = DEFAULT_AGENT_TOOLS.iter().map(|s| String::from(*s)).collect();

    if let Some(ref deny) = def.disallowed_tools {
        for name in deny {
            set.remove(name);
        }
    }

    set
}

// ---------------------------------------------------------------------------
// Prompt building
// ---------------------------------------------------------------------------

/// Build the system prompt sections for a custom agent.
///
/// Returns a `Vec<String>` of prompt sections.  The first section is always
/// the agent role preamble; the second (if present) is the user-supplied
/// `instructions` field.
#[must_use]
pub fn build_agent_prompt(def: &AgentDefinition) -> Vec<String> {
    let mut sections = Vec::new();

    let display = def
        .display_name
        .as_deref()
        .unwrap_or(def.agent_type.as_str());

    let preamble = format!(
        "You are a sub-agent of type `{}` ({}).",
        def.agent_type, display,
    );
    sections.push(preamble);

    if let Some(ref instructions) = def.instructions {
        sections.push(instructions.clone());
    }

    if let Some(ref when) = def.when_to_use {
        sections.push(format!("When to use: {when}"));
    }

    sections
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// List available agent types with their descriptions.
///
/// Returns built-in agents followed by any custom agents discovered on disk.
/// Custom agents that share an `agent_type` with a built-in will shadow the
/// built-in entry.
pub fn list_agent_summaries() -> io::Result<Vec<AgentSummary>> {
    let custom = discover_agents()?;
    let custom_types: BTreeSet<String> = custom.iter().map(|a| a.agent_type.clone()).collect();

    let mut summaries: Vec<AgentSummary> = builtin_summaries()
        .into_iter()
        .filter(|b| !custom_types.contains(&b.agent_type))
        .collect();

    // Determine source for each custom agent.
    let project_dir = project_agents_dir().ok();
    for agent in custom {
        let source = match (&project_dir, &agent.source_file) {
            (Some(pd), Some(sf)) if sf.starts_with(pd) => AgentSource::Project,
            _ => AgentSource::User,
        };
        summaries.push(AgentSummary {
            agent_type: agent.agent_type,
            display_name: agent.display_name,
            description: agent.when_to_use,
            source,
            source_file: agent.source_file,
        });
    }

    Ok(summaries)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Helper: create a temporary directory with agent JSON files.
    /// Returns the PathBuf (caller is responsible for cleanup).
    fn temp_agents_dir(files: &[(&str, &str)]) -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = std::env::temp_dir()
            .join(format!("ember_agent_test_{}_{}", pid, id));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        for (name, content) in files {
            let path = dir.join(name);
            fs::write(&path, content).expect("write temp file");
        }
        dir
    }

    /// Helper: clean up a temp directory.
    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // 1. Parse a valid agent JSON (all fields).
    #[test]
    fn parse_full_agent_json() {
        let json = r#"{
            "agentType": "review",
            "displayName": "Code Review",
            "whenToUse": "Use for PR reviews",
            "instructions": "Review code carefully.",
            "tools": ["read_file", "grep_search"],
            "disallowedTools": null,
            "model": "claude-opus-4-6",
            "effort": "thorough",
            "maxTurns": 10,
            "background": true,
            "isolation": "worktree",
            "memory": "project",
            "mcpServers": ["github", {"name": "jira", "command": "jira-mcp", "args": ["--token", "x"]}],
            "initialPrompt": "Begin review.",
            "omitProjectContext": true,
            "color": "cyan"
        }"#;

        let def: AgentDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.agent_type, "review");
        assert_eq!(def.display_name.as_deref(), Some("Code Review"));
        assert_eq!(def.when_to_use.as_deref(), Some("Use for PR reviews"));
        assert_eq!(def.instructions.as_deref(), Some("Review code carefully."));
        assert_eq!(def.tools, Some(vec!["read_file".into(), "grep_search".into()]));
        assert_eq!(def.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(def.effort, Some(EffortLevel::Thorough));
        assert_eq!(def.max_turns, Some(10));
        assert_eq!(def.background, Some(true));
        assert_eq!(def.isolation, Some(IsolationMode::Worktree));
        assert_eq!(def.memory, Some(MemoryScope::Project));
        assert!(def.mcp_servers.is_some());
        let servers = def.mcp_servers.unwrap();
        assert_eq!(servers.len(), 2);
        assert!(matches!(&servers[0], AgentMcpServerSpec::Named(n) if n == "github"));
        assert!(matches!(&servers[1], AgentMcpServerSpec::Inline { name, .. } if name == "jira"));
        assert_eq!(def.initial_prompt.as_deref(), Some("Begin review."));
        assert_eq!(def.omit_project_context, Some(true));
        assert_eq!(def.color.as_deref(), Some("cyan"));
        assert!(def.source_file.is_none()); // skipped by serde
    }

    // 2. Parse minimal agent JSON (only required fields).
    #[test]
    fn parse_minimal_agent_json() {
        let json = r#"{"agentType": "quick"}"#;
        let def: AgentDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.agent_type, "quick");
        assert!(def.display_name.is_none());
        assert!(def.tools.is_none());
        assert!(def.model.is_none());
        assert!(def.effort.is_none());
    }

    // 3. Resolve tools with explicit allowlist.
    #[test]
    fn resolve_tools_explicit_allowlist() {
        let def = AgentDefinition {
            agent_type: "test".into(),
            tools: Some(vec!["bash".into(), "read_file".into()]),
            ..minimal_def()
        };
        let tools = resolve_agent_tools(&def);
        assert_eq!(tools.len(), 2);
        assert!(tools.contains("bash"));
        assert!(tools.contains("read_file"));
    }

    // 4. Resolve tools with disallowed_tools.
    #[test]
    fn resolve_tools_with_denylist() {
        let def = AgentDefinition {
            agent_type: "test".into(),
            disallowed_tools: Some(vec!["bash".into(), "PowerShell".into()]),
            ..minimal_def()
        };
        let tools = resolve_agent_tools(&def);
        assert!(!tools.contains("bash"));
        assert!(!tools.contains("PowerShell"));
        assert!(tools.contains("read_file"));
        assert!(tools.contains("grep_search"));
    }

    // 5. Resolve tools with defaults.
    #[test]
    fn resolve_tools_defaults() {
        let def = minimal_def();
        let tools = resolve_agent_tools(&def);
        assert_eq!(tools.len(), DEFAULT_AGENT_TOOLS.len());
        for t in DEFAULT_AGENT_TOOLS {
            assert!(tools.contains(*t), "missing default tool: {t}");
        }
    }

    // 6. Load agents from temp directory.
    #[test]
    fn load_agents_from_temp_dir() {
        let dir = temp_agents_dir(&[
            ("alpha.json", r#"{"agentType":"alpha","displayName":"Alpha"}"#),
            ("beta.json", r#"{"agentType":"beta"}"#),
            ("readme.txt", "not json"),
        ]);

        let agents = load_agents_from_dir(&dir).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].agent_type, "alpha");
        assert_eq!(agents[1].agent_type, "beta");
        // source_file should be populated.
        assert!(agents[0].source_file.is_some());
        assert!(agents[0].source_file.as_ref().unwrap().ends_with("alpha.json"));
        cleanup(&dir);
    }

    // 7. Agent deduplication (project overrides user with same agent_type).
    #[test]
    fn deduplication_project_overrides_user() {
        let project_dir = temp_agents_dir(&[
            ("review.json", r#"{"agentType":"review","displayName":"Project Review"}"#),
        ]);
        let user_dir = temp_agents_dir(&[
            ("review.json", r#"{"agentType":"review","displayName":"User Review"}"#),
            ("deploy.json", r#"{"agentType":"deploy"}"#),
        ]);

        let project_agents = load_agents_from_dir(&project_dir).unwrap();
        let user_agents = load_agents_from_dir(&user_dir).unwrap();

        // Simulate discover_agents logic.
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for a in project_agents {
            seen.insert(a.agent_type.clone());
            result.push(a);
        }
        for a in user_agents {
            if !seen.contains(&a.agent_type) {
                seen.insert(a.agent_type.clone());
                result.push(a);
            }
        }

        assert_eq!(result.len(), 2);
        let review = result.iter().find(|a| a.agent_type == "review").unwrap();
        assert_eq!(review.display_name.as_deref(), Some("Project Review"));
        assert!(result.iter().any(|a| a.agent_type == "deploy"));
        cleanup(&project_dir);
        cleanup(&user_dir);
    }

    // 8. Build agent prompt with instructions.
    #[test]
    fn build_prompt_with_instructions() {
        let def = AgentDefinition {
            agent_type: "review".into(),
            display_name: Some("Code Review".into()),
            instructions: Some("Be thorough.".into()),
            when_to_use: Some("For PRs".into()),
            ..minimal_def()
        };
        let sections = build_agent_prompt(&def);
        assert_eq!(sections.len(), 3);
        assert!(sections[0].contains("review"));
        assert!(sections[0].contains("Code Review"));
        assert_eq!(sections[1], "Be thorough.");
        assert!(sections[2].contains("For PRs"));
    }

    // 9. Build agent prompt without instructions (empty).
    #[test]
    fn build_prompt_without_instructions() {
        let def = minimal_def();
        let sections = build_agent_prompt(&def);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("test"));
    }

    // 10. List agent summaries includes builtins + custom.
    #[test]
    fn list_summaries_includes_builtins() {
        // With no custom agents on disk, we should still get builtins.
        // We call builtin_summaries directly to avoid filesystem dependency.
        let builtins = builtin_summaries();
        assert!(builtins.len() >= 4);
        let types: Vec<&str> = builtins.iter().map(|s| s.agent_type.as_str()).collect();
        assert!(types.contains(&"general-purpose"));
        assert!(types.contains(&"Explore"));
        assert!(types.contains(&"Plan"));
        assert!(types.contains(&"Verification"));
        for s in &builtins {
            assert_eq!(s.source, AgentSource::Builtin);
        }
    }

    // 11. find_agent returns correct definition (via load from dir).
    #[test]
    fn find_agent_in_dir() {
        let dir = temp_agents_dir(&[
            ("alpha.json", r#"{"agentType":"alpha","displayName":"Alpha Agent"}"#),
            ("beta.json", r#"{"agentType":"beta"}"#),
        ]);

        let agents = load_agents_from_dir(&dir).unwrap();
        let found = agents.into_iter().find(|a| a.agent_type == "alpha");
        assert!(found.is_some());
        assert_eq!(found.unwrap().display_name.as_deref(), Some("Alpha Agent"));
        cleanup(&dir);
    }

    // 12. Invalid JSON files are skipped gracefully.
    #[test]
    fn invalid_json_skipped() {
        let dir = temp_agents_dir(&[
            ("good.json", r#"{"agentType":"good"}"#),
            ("bad.json", r#"{"not_valid": true}"#),
            ("broken.json", "{{{{"),
        ]);

        let agents = load_agents_from_dir(&dir).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_type, "good");
        cleanup(&dir);
    }

    // Additional: non-existent directory returns empty vec.
    #[test]
    fn nonexistent_dir_returns_empty() {
        let agents = load_agents_from_dir(Path::new("/tmp/nonexistent_ember_test_dir")).unwrap();
        assert!(agents.is_empty());
    }

    // Additional: effort level serde round-trip.
    #[test]
    fn effort_level_serde() {
        let json = r#"{"agentType":"x","effort":"speedy"}"#;
        let def: AgentDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.effort, Some(EffortLevel::Speedy));

        let json = r#"{"agentType":"x","effort":"balanced"}"#;
        let def: AgentDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.effort, Some(EffortLevel::Balanced));
    }

    /// Minimal valid definition for test convenience.
    fn minimal_def() -> AgentDefinition {
        AgentDefinition {
            agent_type: "test".into(),
            display_name: None,
            when_to_use: None,
            instructions: None,
            tools: None,
            disallowed_tools: None,
            model: None,
            effort: None,
            max_turns: None,
            background: None,
            isolation: None,
            memory: None,
            mcp_servers: None,
            initial_prompt: None,
            omit_project_context: None,
            color: None,
            source_file: None,
        }
    }
}
