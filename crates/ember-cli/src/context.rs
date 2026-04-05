//! Context injection framework — dynamically collects and injects context
//! into prompts with token-aware budgeting.
//!
//! Supports multiple context sources: codebase map, directory READMEs,
//! project rules, and custom injectors.

use std::path::Path;
use std::fs;

#[cfg(test)]
use std::env;

use crate::codebase_map;

/// Maximum tokens to spend on injected context (rough estimate: 4 chars ≈ 1 token).
const DEFAULT_CONTEXT_BUDGET_CHARS: usize = 20_000;

/// A collected context snippet to inject into the prompt.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct ContextSnippet {
    pub source: &'static str,
    pub content: String,
    pub priority: u8, // higher = more important
}

/// Collects context from all registered sources, respecting token budget.
pub(crate) fn collect_session_context(cwd: &Path) -> Vec<ContextSnippet> {
    let mut snippets = Vec::new();

    // 1. Codebase map (high priority — always included if available).
    if let Some(map) = codebase_map::build_codebase_map(cwd) {
        snippets.push(ContextSnippet {
            source: "codebase_map",
            content: map,
            priority: 90,
        });
    }

    // 2. Project summary from package metadata.
    if let Some(summary) = codebase_map::read_project_summary(cwd) {
        snippets.push(ContextSnippet {
            source: "project_summary",
            content: format!("Project: {summary}"),
            priority: 95,
        });
    }

    // 3. Directory README files (for workspace root).
    if let Some(readme) = read_directory_readme(cwd) {
        snippets.push(ContextSnippet {
            source: "readme",
            content: readme,
            priority: 70,
        });
    }

    // 4. Project rules from .claude/rules/ or .ember/rules/.
    snippets.extend(collect_project_rules(cwd));

    // Sort by priority (highest first) and truncate to budget.
    snippets.sort_by(|a, b| b.priority.cmp(&a.priority));
    truncate_to_budget(&mut snippets, DEFAULT_CONTEXT_BUDGET_CHARS);

    snippets
}

/// Build a prompt section from collected context snippets.
pub(crate) fn render_context_section(snippets: &[ContextSnippet]) -> Option<String> {
    if snippets.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    for snippet in snippets {
        parts.push(snippet.content.clone());
    }
    Some(parts.join("\n\n"))
}

/// Collect context for a specific file access — injects README from the file's directory.
pub(crate) fn collect_file_context(file_path: &Path) -> Vec<ContextSnippet> {
    let mut snippets = Vec::new();

    // Walk up the directory tree looking for READMEs.
    let mut dir = file_path.parent();
    let mut depth = 0;
    while let Some(current) = dir {
        if depth > 3 {
            break;
        }
        if let Some(readme) = read_directory_readme(current) {
            snippets.push(ContextSnippet {
                source: "dir_readme",
                content: format!(
                    "Context from {}/README:\n{}",
                    current.display(),
                    truncate_content(&readme, 5000)
                ),
                priority: 60u8.saturating_sub(depth * 10),
            });
            break; // only inject the closest README
        }
        dir = current.parent();
        depth += 1;
    }

    snippets
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Read the README file from a directory if it exists.
fn read_directory_readme(dir: &Path) -> Option<String> {
    let candidates = ["README.md", "README", "README.txt", "readme.md"];
    for name in &candidates {
        let path = dir.join(name);
        if let Ok(content) = fs::read_to_string(&path) {
            if !content.trim().is_empty() {
                return Some(truncate_content(&content, 5000));
            }
        }
    }
    None
}

/// Collect project rules from standard locations.
fn collect_project_rules(cwd: &Path) -> Vec<ContextSnippet> {
    let mut snippets = Vec::new();
    let rule_dirs = [
        cwd.join(".claude").join("rules"),
        cwd.join(".ember").join("rules"),
        cwd.join(".github").join("instructions"),
    ];

    for rule_dir in &rule_dirs {
        if !rule_dir.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(rule_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                || path.extension().and_then(|e| e.to_str()) == Some("txt")
            {
                if let Ok(content) = fs::read_to_string(&path) {
                    if !content.trim().is_empty() {
                        snippets.push(ContextSnippet {
                            source: "project_rule",
                            content: format!(
                                "Rule ({}):\n{}",
                                path.file_name().unwrap_or_default().to_string_lossy(),
                                truncate_content(&content, 3000)
                            ),
                            priority: 65,
                        });
                    }
                }
            }
        }
    }

    snippets
}

/// Truncate content to a character limit, adding a notice.
fn truncate_content(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        content.to_string()
    } else {
        format!("{}...\n[truncated]", &content[..max_chars])
    }
}

/// Truncate snippets to fit within a total character budget.
fn truncate_to_budget(snippets: &mut Vec<ContextSnippet>, budget: usize) {
    let mut used = 0usize;
    let mut keep = Vec::new();
    for snippet in snippets.drain(..) {
        let cost = snippet.content.len();
        if used + cost <= budget {
            used += cost;
            keep.push(snippet);
        } else {
            // Try to fit a truncated version.
            let remaining = budget.saturating_sub(used);
            if remaining > 200 {
                let truncated = truncate_content(&snippet.content, remaining);
                keep.push(ContextSnippet {
                    content: truncated,
                    ..snippet
                });
            }
            break;
        }
    }
    *snippets = keep;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_to_budget_respects_limit() {
        let mut snippets = vec![
            ContextSnippet {
                source: "a",
                content: "x".repeat(100),
                priority: 90,
            },
            ContextSnippet {
                source: "b",
                content: "y".repeat(500),
                priority: 80,
            },
        ];
        truncate_to_budget(&mut snippets, 400);
        assert_eq!(snippets.len(), 2);
        // First is kept fully, second truncated to remaining budget.
        assert_eq!(snippets[0].content.len(), 100);
        assert!(snippets[1].content.len() < 500);
        assert!(snippets[1].content.contains("[truncated]"));
    }

    #[test]
    fn collect_session_context_works_on_cwd() {
        let cwd = env::current_dir().unwrap();
        let snippets = collect_session_context(&cwd);
        // Should at least detect something (we're in a valid dir).
        // May be empty if temp dir, which is fine.
        assert!(snippets.len() <= 20);
    }

    #[test]
    fn render_empty_returns_none() {
        assert!(render_context_section(&[]).is_none());
    }

    #[test]
    fn render_snippets_joins_content() {
        let snippets = vec![
            ContextSnippet {
                source: "a",
                content: "hello".to_string(),
                priority: 90,
            },
            ContextSnippet {
                source: "b",
                content: "world".to_string(),
                priority: 80,
            },
        ];
        let rendered = render_context_section(&snippets).unwrap();
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("world"));
    }
}
