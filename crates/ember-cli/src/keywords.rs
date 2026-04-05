//! Magic keyword detection — auto-detects workflow keywords in user input
//! and injects corresponding skill context or mode activation.
//!
//! Inspired by oh-my-claudecode and oh-my-codex keyword systems.

/// A detected keyword match with its associated action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeywordMatch {
    /// The keyword that matched.
    pub keyword: &'static str,
    /// The action/skill to activate.
    pub action: KeywordAction,
    /// Priority (higher = takes precedence when multiple match).
    pub priority: u8,
}

/// What to do when a keyword is detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeywordAction {
    /// Inject a skill prompt as context before the user message.
    InjectSkill {
        name: &'static str,
        preamble: &'static str,
    },
    /// Activate a mode (effort, plan, verbose).
    ActivateMode(&'static str),
    /// Enhance the prompt with additional guidance.
    EnhancePrompt(&'static str),
}

/// Keyword definition table — maps trigger phrases to actions.
const KEYWORD_TABLE: &[(&[&str], KeywordAction, u8)] = &[
    // ── Thinking / reasoning ──
    (
        &["ultrathink", "think hard", "think deeply", "reason carefully"],
        KeywordAction::ActivateMode("thorough"),
        90,
    ),
    (
        &["think step by step", "step by step", "chain of thought"],
        KeywordAction::EnhancePrompt(
            "Think step by step. Show your reasoning before giving the final answer.",
        ),
        70,
    ),
    // ── Planning ──
    (
        &["ultraplan", "deep plan", "plan this out"],
        KeywordAction::InjectSkill {
            name: "ultraplan",
            preamble: "Entering deep planning mode. Create a comprehensive multi-step plan before executing.",
        },
        85,
    ),
    (
        &["make a plan", "create a plan", "let's plan"],
        KeywordAction::ActivateMode("plan"),
        60,
    ),
    // ── Code review ──
    (
        &["review this", "code review", "review my code", "review the code"],
        KeywordAction::InjectSkill {
            name: "review",
            preamble: "Review the code for bugs, security issues, performance, and best practices.",
        },
        75,
    ),
    // ── Bug hunting ──
    (
        &["find bugs", "hunt bugs", "bughunter", "bug hunt"],
        KeywordAction::InjectSkill {
            name: "bughunter",
            preamble: "Inspect the codebase systematically for likely bugs, edge cases, and error-prone patterns.",
        },
        75,
    ),
    // ── Quick / relaxed ──
    (
        &["quick", "briefly", "tl;dr", "short answer", "be concise"],
        KeywordAction::ActivateMode("relaxed"),
        50,
    ),
    // ── Thorough ──
    (
        &["thorough", "be thorough", "in depth", "in-depth", "detailed analysis"],
        KeywordAction::ActivateMode("thorough"),
        50,
    ),
    // ── Debugging ──
    (
        &["debug this", "help me debug", "why is this broken", "trace the issue"],
        KeywordAction::EnhancePrompt(
            "Debug systematically: reproduce the issue, form hypotheses, test each one, and explain root cause.",
        ),
        65,
    ),
    // ── Testing ──
    (
        &["write tests", "add tests", "test this", "tdd", "test first"],
        KeywordAction::EnhancePrompt(
            "Write comprehensive tests. Cover happy path, edge cases, and error conditions. Use test-driven approach.",
        ),
        60,
    ),
    // ── Security ──
    (
        &["security review", "security audit", "check for vulnerabilities"],
        KeywordAction::InjectSkill {
            name: "security-review",
            preamble: "Perform a security review. Check for OWASP top 10, injection vulnerabilities, auth issues, and data exposure.",
        },
        80,
    ),
];

/// Detect magic keywords in user input.
///
/// Returns all matches sorted by priority (highest first).
/// Skips detection inside code blocks (fenced with ```).
pub(crate) fn detect_keywords(input: &str) -> Vec<KeywordMatch> {
    let searchable = strip_code_blocks(input).to_ascii_lowercase();

    let mut matches = Vec::new();
    for (triggers, action, priority) in KEYWORD_TABLE {
        for &trigger in *triggers {
            if searchable.contains(trigger) {
                matches.push(KeywordMatch {
                    keyword: trigger,
                    action: action.clone(),
                    priority: *priority,
                });
                break; // one match per keyword group
            }
        }
    }

    matches.sort_by(|a, b| b.priority.cmp(&a.priority));
    matches
}

/// Build a context preamble from detected keyword matches.
///
/// Returns `None` if no actionable keywords were detected.
pub(crate) fn build_keyword_context(matches: &[KeywordMatch]) -> Option<String> {
    if matches.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    for m in matches {
        match &m.action {
            KeywordAction::InjectSkill { name, preamble } => {
                parts.push(format!("[keyword:{name}] {preamble}"));
            }
            KeywordAction::EnhancePrompt(guidance) => {
                parts.push(format!("[guidance] {guidance}"));
            }
            KeywordAction::ActivateMode(_) => {
                // Mode activations are handled by the caller, not injected as text.
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Extract mode activation requests from keyword matches.
pub(crate) fn extract_mode_activations(matches: &[KeywordMatch]) -> Vec<&'static str> {
    matches
        .iter()
        .filter_map(|m| match &m.action {
            KeywordAction::ActivateMode(mode) => Some(*mode),
            _ => None,
        })
        .collect()
}

// ── Task size detection ──────────────────────────────────────────────

/// Detected task complexity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskSize {
    /// Simple question or small change (<50 words).
    Light,
    /// Moderate task (50-200 words).
    Medium,
    /// Complex multi-step task (>200 words).
    Heavy,
}

impl TaskSize {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Medium => "medium",
            Self::Heavy => "heavy",
        }
    }
}

/// Escape prefixes that force lightweight mode regardless of word count.
const LIGHTWEIGHT_PREFIXES: &[&str] = &[
    "quick:", "simple:", "tiny:", "just:", "q:",
];

/// Classify task size based on input length and complexity signals.
pub(crate) fn classify_task_size(input: &str) -> TaskSize {
    let trimmed = input.trim();

    // Check escape prefixes first.
    let lower = trimmed.to_ascii_lowercase();
    for prefix in LIGHTWEIGHT_PREFIXES {
        if lower.starts_with(prefix) {
            return TaskSize::Light;
        }
    }

    // Count words (excluding code blocks).
    let searchable = strip_code_blocks(trimmed);
    let word_count = searchable.split_whitespace().count();

    // Check for complexity signals.
    let has_list = searchable.lines().any(|line| {
        let t = line.trim();
        t.starts_with("- ") || t.starts_with("* ") || t.starts_with("1.")
    });
    let has_multiple_questions = searchable.matches('?').count() > 1;

    if word_count < 50 && !has_list && !has_multiple_questions {
        TaskSize::Light
    } else if word_count > 200 || (has_list && word_count > 80) {
        TaskSize::Heavy
    } else {
        TaskSize::Medium
    }
}

/// Strip fenced code blocks from input to avoid false keyword matches.
fn strip_code_blocks(input: &str) -> String {
    let mut result = String::new();
    let mut in_code_block = false;
    for line in input.lines() {
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if !in_code_block {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_thinking_keyword() {
        let matches = detect_keywords("I need you to ultrathink about this problem");
        assert!(!matches.is_empty());
        assert_eq!(matches[0].keyword, "ultrathink");
        let modes = extract_mode_activations(&matches);
        assert!(modes.contains(&"thorough"));
    }

    #[test]
    fn detects_planning_keyword() {
        let matches = detect_keywords("Can you ultraplan the migration?");
        assert!(!matches.is_empty());
        assert_eq!(matches[0].keyword, "ultraplan");
        let context = build_keyword_context(&matches);
        assert!(context.unwrap().contains("deep planning"));
    }

    #[test]
    fn detects_review_keyword() {
        let matches = detect_keywords("Please review this code for issues");
        assert!(!matches.is_empty());
        assert_eq!(matches[0].keyword, "review this");
    }

    #[test]
    fn ignores_keywords_in_code_blocks() {
        let input = "Here's some code:\n```\nlet ultrathink = true;\n```\nWhat do you think?";
        let matches = detect_keywords(input);
        // "ultrathink" is inside a code block, should not match
        assert!(
            !matches.iter().any(|m| m.keyword == "ultrathink"),
            "should not detect keywords inside code blocks"
        );
    }

    #[test]
    fn multiple_keywords_sorted_by_priority() {
        let matches = detect_keywords("ultrathink about this and review this code");
        assert!(matches.len() >= 2);
        // highest priority first
        assert!(matches[0].priority >= matches[1].priority);
    }

    #[test]
    fn no_keywords_returns_empty() {
        let matches = detect_keywords("Hello, how are you?");
        assert!(matches.is_empty());
        assert!(build_keyword_context(&matches).is_none());
    }

    #[test]
    fn quick_mode_detected() {
        let matches = detect_keywords("Give me a quick answer");
        let modes = extract_mode_activations(&matches);
        assert!(modes.contains(&"relaxed"));
    }

    // ── Task size tests ──

    #[test]
    fn short_input_is_light() {
        assert_eq!(classify_task_size("Fix the typo"), TaskSize::Light);
    }

    #[test]
    fn long_input_is_heavy() {
        let long = "word ".repeat(250);
        assert_eq!(classify_task_size(&long), TaskSize::Heavy);
    }

    #[test]
    fn medium_input_detected() {
        let medium = "word ".repeat(80);
        assert_eq!(classify_task_size(&medium), TaskSize::Medium);
    }

    #[test]
    fn escape_prefix_forces_light() {
        let input = "quick: implement the entire authentication system with OAuth2, PKCE, token refresh, and session management across all endpoints";
        assert_eq!(classify_task_size(input), TaskSize::Light);
    }

    #[test]
    fn list_with_many_words_is_heavy() {
        let input = "Please do these things:\n- First implement the auth\n- Then add the database\n- ".to_string() + &"word ".repeat(100);
        assert_eq!(classify_task_size(&input), TaskSize::Heavy);
    }
}
