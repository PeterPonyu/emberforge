use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};

const COMPACT_CONTINUATION_PREAMBLE: &str =
    "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n";
const COMPACT_RECENT_MESSAGES_NOTE: &str = "Recent messages are preserved verbatim.";
const COMPACT_DIRECT_RESUME_INSTRUCTION: &str = "Continue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, and do not preface with continuation text.";

/// Token buffer thresholds for auto-compaction decisions.
const AUTOCOMPACT_BUFFER_TOKENS: usize = 13_000;
/// Token buffer for displaying a warning.
const WARNING_THRESHOLD_BUFFER_TOKENS: usize = 20_000;
/// Maximum consecutive auto-compaction failures before the circuit breaker trips.
const MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES: u32 = 3;
/// Maximum tool result content length (in chars) before micro-compaction truncates it.
const MICRO_COMPACT_TOOL_RESULT_LIMIT: usize = 8_000;
/// Number of most recent compactable tool results to keep during micro-compaction.
const MICRO_COMPACT_KEEP_RECENT: usize = 8;
/// Aggressive keep-recent count when time-based trigger fires.
const MICRO_COMPACT_KEEP_RECENT_AGGRESSIVE: usize = 2;
/// Token estimation padding factor (numerator / denominator = 4/3).
const TOKEN_ESTIMATION_PADDING_NUM: usize = 4;
const TOKEN_ESTIMATION_PADDING_DEN: usize = 3;

/// Tools whose results are safe to content-clear during micro-compaction.
const COMPACTABLE_TOOLS: &[&str] = &[
    "bash", "read_file", "grep_search", "glob_search",
    "WebFetch", "WebSearch", "edit_file", "write_file",
    "NotebookEdit", "LSPTool",
];
/// Number of recent files to restore after full compaction.
const POST_COMPACT_MAX_FILES: usize = 5;
/// Per-file token budget for post-compact restoration.
const POST_COMPACT_PER_FILE_TOKENS: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionConfig {
    pub preserve_recent_messages: usize,
    pub max_estimated_tokens: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            preserve_recent_messages: 4,
            max_estimated_tokens: 10_000,
        }
    }
}

/// Configuration for auto-compaction within the conversation loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoCompactConfig {
    /// Total context window size in tokens for the active model.
    pub context_window: usize,
    /// Token buffer to reserve before triggering auto-compaction.
    pub buffer_tokens: usize,
    /// Whether to attempt micro-compaction before full compaction.
    pub micro_compact_enabled: bool,
    /// Preservation config for full compaction.
    pub compaction: CompactionConfig,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        Self {
            context_window: 128_000,
            buffer_tokens: AUTOCOMPACT_BUFFER_TOKENS,
            micro_compact_enabled: true,
            compaction: CompactionConfig::default(),
        }
    }
}

/// Warning severity for context window usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TokenWarningLevel {
    /// Context usage is normal.
    Normal,
    /// Context is filling up — advisory for the user.
    Warning,
    /// Context is nearly full — compaction strongly recommended.
    Error,
    /// Context is critical — auto-compaction will fire.
    Critical,
}

/// Context window usage state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenWarningState {
    pub level: TokenWarningLevel,
    pub estimated_tokens: usize,
    pub context_window: usize,
    pub remaining: usize,
}

/// Tracks auto-compaction state across turns.
#[derive(Debug, Clone, Default)]
pub struct AutoCompactState {
    /// Consecutive auto-compaction failures (circuit breaker).
    pub consecutive_failures: u32,
    /// Number of auto-compactions performed this session.
    pub total_compactions: u32,
    /// Whether the circuit breaker has tripped.
    pub circuit_broken: bool,
}

impl AutoCompactState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful compaction, resetting the failure counter.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.total_compactions += 1;
    }

    /// Record a failed compaction attempt.
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES {
            self.circuit_broken = true;
        }
    }

    /// Whether auto-compaction should be suppressed.
    #[must_use]
    pub fn is_suppressed(&self) -> bool {
        self.circuit_broken
    }
}

/// Metadata about the compaction chain (for recompaction decisions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecompactionInfo {
    /// Whether a prior compaction exists in this session.
    pub is_recompaction: bool,
    /// Turns since the previous compaction (0 if first).
    pub turns_since_previous: usize,
    /// Auto-compact threshold that triggered this compaction.
    pub auto_compact_threshold: usize,
}

/// Result of a micro-compaction pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicroCompactResult {
    pub session: Session,
    /// Number of tool results that were content-cleared.
    pub cleared_count: usize,
    /// Estimated tokens freed by clearing.
    pub tokens_freed: usize,
    /// Whether a time-based aggressive trigger fired.
    pub time_triggered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub summary: String,
    pub formatted_summary: String,
    pub compacted_session: Session,
    pub removed_message_count: usize,
    pub recompaction_info: Option<RecompactionInfo>,
}

#[must_use]
pub fn estimate_session_tokens(session: &Session) -> usize {
    session.messages.iter().map(estimate_message_tokens).sum()
}

#[must_use]
pub fn should_compact(session: &Session, config: CompactionConfig) -> bool {
    let start = compacted_summary_prefix_len(session);
    let compactable = &session.messages[start..];

    compactable.len() > config.preserve_recent_messages
        && compactable
            .iter()
            .map(estimate_message_tokens)
            .sum::<usize>()
            >= config.max_estimated_tokens
}

#[must_use]
pub fn format_compact_summary(summary: &str) -> String {
    let without_analysis = strip_tag_block(summary, "analysis");
    let formatted = if let Some(content) = extract_tag_block(&without_analysis, "summary") {
        without_analysis.replace(
            &format!("<summary>{content}</summary>"),
            &format!("Summary:\n{}", content.trim()),
        )
    } else {
        without_analysis
    };

    collapse_blank_lines(&formatted).trim().to_string()
}

#[must_use]
pub fn get_compact_continuation_message(
    summary: &str,
    suppress_follow_up_questions: bool,
    recent_messages_preserved: bool,
) -> String {
    let mut base = format!(
        "{COMPACT_CONTINUATION_PREAMBLE}{}",
        format_compact_summary(summary)
    );

    if recent_messages_preserved {
        base.push_str("\n\n");
        base.push_str(COMPACT_RECENT_MESSAGES_NOTE);
    }

    if suppress_follow_up_questions {
        base.push('\n');
        base.push_str(COMPACT_DIRECT_RESUME_INSTRUCTION);
    }

    base
}

#[must_use]
pub fn compact_session(session: &Session, config: CompactionConfig) -> CompactionResult {
    if !should_compact(session, config) {
        return CompactionResult {
            summary: String::new(),
            formatted_summary: String::new(),
            compacted_session: session.clone(),
            removed_message_count: 0,
            recompaction_info: None,
        };
    }

    let existing_summary = session
        .messages
        .first()
        .and_then(extract_existing_compacted_summary);
    let checkpoint_context = render_checkpoint_context(&create_pre_compact_checkpoint(session));
    let compacted_prefix_len = usize::from(existing_summary.is_some());
    let keep_from = session
        .messages
        .len()
        .saturating_sub(config.preserve_recent_messages);
    let removed = &session.messages[compacted_prefix_len..keep_from];
    let preserved = session.messages[keep_from..].to_vec();
    let summary =
        merge_compact_summaries(existing_summary.as_deref(), &summarize_messages(removed));
    let formatted_summary = format_compact_summary(&summary);
    let mut continuation =
        get_compact_continuation_message(&summary, false, !preserved.is_empty());
    if let Some(checkpoint_context) = checkpoint_context {
        continuation.push_str("\n\n");
        continuation.push_str(&checkpoint_context);
    }
    continuation.push('\n');
    continuation.push_str(COMPACT_DIRECT_RESUME_INSTRUCTION);

    let mut compacted_messages = vec![ConversationMessage {
        role: MessageRole::System,
        blocks: vec![ContentBlock::Text { text: continuation }],
        usage: None,
    }];
    compacted_messages.extend(preserved);

    CompactionResult {
        summary,
        formatted_summary,
        compacted_session: Session {
            version: session.version,
            messages: compacted_messages,
            plan_mode: session.plan_mode,
        },
        removed_message_count: removed.len(),
        recompaction_info: None,
    }
}

// ── Auto-compaction decision logic ──────────────────────────────────────

/// Calculate the current token warning state for a session.
#[must_use]
pub fn calculate_token_warning(session: &Session, context_window: usize) -> TokenWarningState {
    let estimated = estimate_session_tokens(session);
    let remaining = context_window.saturating_sub(estimated);

    let level = if remaining <= AUTOCOMPACT_BUFFER_TOKENS {
        TokenWarningLevel::Critical
    } else if remaining <= WARNING_THRESHOLD_BUFFER_TOKENS {
        TokenWarningLevel::Error
    } else if remaining <= WARNING_THRESHOLD_BUFFER_TOKENS * 2 {
        TokenWarningLevel::Warning
    } else {
        TokenWarningLevel::Normal
    };

    TokenWarningState {
        level,
        estimated_tokens: estimated,
        context_window,
        remaining,
    }
}

/// Determine whether auto-compaction should trigger after a turn.
#[must_use]
pub fn should_auto_compact(
    session: &Session,
    config: &AutoCompactConfig,
    state: &AutoCompactState,
) -> bool {
    if state.is_suppressed() {
        return false;
    }

    let estimated = estimate_session_tokens(session);
    let threshold = config.context_window.saturating_sub(config.buffer_tokens);
    estimated >= threshold
}

// ── Micro-compaction ───────────────────────────────────────────────────

/// Attempt micro-compaction using content-clearing strategy (CC pattern).
///
/// Instead of just truncating large results, this:
/// 1. Identifies tool results from compactable tools (bash, `read_file`, grep, etc.)
/// 2. Keeps the N most recent results intact
/// 3. Content-clears older ones with `[Old tool result content cleared]`
/// 4. Falls back to truncation for non-compactable tools with oversized results
///
/// Time-based trigger: if gap since last assistant > 30min, clear aggressively (keep 2).
#[must_use]
pub fn micro_compact_session(session: &Session, preserve_recent: usize) -> MicroCompactResult {
    let boundary = session.messages.len().saturating_sub(preserve_recent);
    let mut messages = session.messages.clone();

    // Check for time-based aggressive trigger
    let time_triggered = check_time_based_trigger(&messages);
    let keep_recent = if time_triggered {
        MICRO_COMPACT_KEEP_RECENT_AGGRESSIVE
    } else {
        MICRO_COMPACT_KEEP_RECENT
    };

    // Collect all compactable tool result positions (message_idx, block_idx, tool_name)
    // from messages before the preservation boundary
    let mut compactable_positions: Vec<(usize, usize, String)> = Vec::new();
    for (msg_idx, message) in messages.iter().enumerate().take(boundary) {
        for (blk_idx, block) in message.blocks.iter().enumerate() {
            if let ContentBlock::ToolResult { tool_name, output, .. } = block {
                if is_compactable_tool(tool_name) && !output.is_empty() {
                    compactable_positions.push((msg_idx, blk_idx, tool_name.clone()));
                }
            }
        }
    }

    // Determine which to clear:
    // - If more than keep_recent: clear the oldest, keep N most recent
    // - Additionally: always clear oversized results (> MICRO_COMPACT_TOOL_RESULT_LIMIT)
    //   even if within the keep-recent window
    let clear_count = compactable_positions.len().saturating_sub(keep_recent);
    let mut positions_to_clear: Vec<(usize, usize)> = compactable_positions
        .iter()
        .take(clear_count)
        .map(|(m, b, _)| (*m, *b))
        .collect();

    // Also include any compactable results within keep-recent that exceed the size limit
    for &(m, b, _) in compactable_positions.iter().skip(clear_count) {
        if let ContentBlock::ToolResult { output, .. } = &messages[m].blocks[b] {
            if output.len() > MICRO_COMPACT_TOOL_RESULT_LIMIT {
                positions_to_clear.push((m, b));
            }
        }
    }

    let mut cleared_count = 0;
    let mut tokens_freed = 0;

    // Content-clear the selected compactable tool results
    for &(msg_idx, blk_idx) in &positions_to_clear {
        if let ContentBlock::ToolResult { output, .. } = &mut messages[msg_idx].blocks[blk_idx] {
            let old_tokens = estimate_text_tokens(output);
            *output = "[Old tool result content cleared]".to_string();
            let new_tokens = estimate_text_tokens(output);
            tokens_freed += old_tokens.saturating_sub(new_tokens);
            cleared_count += 1;
        }
    }

    // Also truncate any non-compactable oversized results (fallback)
    for message in messages.iter_mut().take(boundary) {
        for block in &mut message.blocks {
            if let ContentBlock::ToolResult { tool_name, output, .. } = block {
                if !is_compactable_tool(tool_name)
                    && output.len() > MICRO_COMPACT_TOOL_RESULT_LIMIT
                {
                    let old_tokens = estimate_text_tokens(output);
                    let truncated: String = output
                        .chars()
                        .take(MICRO_COMPACT_TOOL_RESULT_LIMIT)
                        .collect();
                    *output = format!(
                        "{truncated}\n\n[… truncated from {} to {} chars]",
                        output.len(),
                        MICRO_COMPACT_TOOL_RESULT_LIMIT
                    );
                    let new_tokens = estimate_text_tokens(output);
                    tokens_freed += old_tokens.saturating_sub(new_tokens);
                    cleared_count += 1;
                }
            }
        }
    }

    MicroCompactResult {
        session: Session {
            version: session.version,
            messages,
            plan_mode: session.plan_mode,
        },
        cleared_count,
        tokens_freed,
        time_triggered,
    }
}

/// Check if a tool's results are safe to content-clear during micro-compaction.
fn is_compactable_tool(tool_name: &str) -> bool {
    COMPACTABLE_TOOLS.contains(&tool_name)
}

/// Check if enough time has passed since the last assistant message to trigger
/// aggressive micro-compaction (CC's time-based trigger).
fn check_time_based_trigger(messages: &[ConversationMessage]) -> bool {
    // Find the last assistant message
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant);

    let Some(assistant_msg) = last_assistant else {
        return false;
    };

    // Check if the message has usage info with a timestamp
    // For now, use a simple heuristic: if the assistant's last text is very short
    // (likely an old message from a resumed session), treat as stale
    if let Some(usage) = &assistant_msg.usage {
        // If we have token counts, the message was from this session run
        if usage.input_tokens > 0 {
            return false; // Recent message, no trigger
        }
    }

    // Without proper timestamps, we can't do time-based triggering.
    // This will be enhanced when we add message timestamps.
    // Threshold when implemented: TIME_BASED_GAP_MINUTES minutes of inactivity.
    false
}

/// Run the full multi-tier compaction strategy:
/// 1. Micro-compact (content-clear old tool results)
/// 2. Full compaction (summarize older messages)
///
/// Returns `None` if no compaction was needed.
#[must_use]
pub fn auto_compact_session(
    session: &Session,
    config: &AutoCompactConfig,
) -> Option<CompactionResult> {
    let threshold = config.context_window.saturating_sub(config.buffer_tokens);

    // Check if we're in a recompaction chain
    let is_recompaction = session
        .messages
        .first()
        .and_then(extract_existing_compacted_summary)
        .is_some();
    let turns_since_previous = if is_recompaction {
        session.messages.len().saturating_sub(1) // exclude summary message
    } else {
        0
    };

    // Tier 1: Try micro-compaction first.
    if config.micro_compact_enabled {
        let micro_result =
            micro_compact_session(session, config.compaction.preserve_recent_messages);
        if micro_result.cleared_count > 0 {
            let new_tokens = estimate_session_tokens(&micro_result.session);
            if new_tokens < threshold {
                return Some(CompactionResult {
                    summary: format!(
                        "Micro-compacted {} tool results (~{} tokens freed{})",
                        micro_result.cleared_count,
                        micro_result.tokens_freed,
                        if micro_result.time_triggered {
                            ", time-based trigger"
                        } else {
                            ""
                        }
                    ),
                    formatted_summary: format!(
                        "Micro-compacted {} tool results",
                        micro_result.cleared_count
                    ),
                    compacted_session: micro_result.session,
                    removed_message_count: 0,
                    recompaction_info: Some(RecompactionInfo {
                        is_recompaction,
                        turns_since_previous,
                        auto_compact_threshold: threshold,
                    }),
                });
            }
            // Micro wasn't enough — continue to full compaction on the micro-compacted session
            let result = compact_session(&micro_result.session, config.compaction);
            if result.removed_message_count > 0 {
                return Some(CompactionResult {
                    recompaction_info: Some(RecompactionInfo {
                        is_recompaction,
                        turns_since_previous,
                        auto_compact_threshold: threshold,
                    }),
                    ..result
                });
            }
            return None;
        }
    }

    // Tier 2: Full compaction (no micro results or micro not enabled).
    let result = compact_session(session, config.compaction);
    if result.removed_message_count > 0 {
        Some(CompactionResult {
            recompaction_info: Some(RecompactionInfo {
                is_recompaction,
                turns_since_previous,
                auto_compact_threshold: threshold,
            }),
            ..result
        })
    } else {
        None
    }
}

// ── Post-compact file restoration ──────────────────────────────────────

/// After full compaction, restore recently-referenced files as context hints.
/// Appends file path references to the compacted summary.
#[must_use]
pub fn post_compact_restore_file_hints(result: &CompactionResult) -> CompactionResult {
    let files = collect_key_files_from_session(&result.compacted_session);
    if files.is_empty() {
        return result.clone();
    }

    let restore_files: Vec<_> = files
        .into_iter()
        .take(POST_COMPACT_MAX_FILES)
        .collect();
    let _ = POST_COMPACT_PER_FILE_TOKENS; // reserved for future per-file content restoration

    let hint = format!(
        "\n\nKey files from the compacted context (may need re-reading): {}",
        restore_files.join(", ")
    );

    let mut new_result = result.clone();
    if let Some(ContentBlock::Text { text }) = new_result
        .compacted_session
        .messages
        .first_mut()
        .and_then(|m| m.blocks.first_mut())
    {
        text.push_str(&hint);
    }
    new_result
}

fn collect_key_files_from_session(session: &Session) -> Vec<String> {
    let mut files = session
        .messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .map(|block| match block {
            ContentBlock::Text { text } => text.as_str(),
            ContentBlock::ToolUse { input, .. } => input.as_str(),
            ContentBlock::ToolResult { output, .. } => output.as_str(),
        })
        .flat_map(extract_file_candidates)
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files.into_iter().take(POST_COMPACT_MAX_FILES).collect()
}

fn compacted_summary_prefix_len(session: &Session) -> usize {
    usize::from(
        session
            .messages
            .first()
            .and_then(extract_existing_compacted_summary)
            .is_some(),
    )
}

fn summarize_messages(messages: &[ConversationMessage]) -> String {
    let user_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .count();
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .count();
    let tool_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .count();

    let mut tool_names = messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
            ContentBlock::ToolResult { tool_name, .. } => Some(tool_name.as_str()),
            ContentBlock::Text { .. } => None,
        })
        .collect::<Vec<_>>();
    tool_names.sort_unstable();
    tool_names.dedup();

    let mut lines = vec![
        "<summary>".to_string(),
        "Conversation summary:".to_string(),
        format!(
            "- Scope: {} earlier messages compacted (user={}, assistant={}, tool={}).",
            messages.len(),
            user_messages,
            assistant_messages,
            tool_messages
        ),
    ];

    if !tool_names.is_empty() {
        lines.push(format!("- Tools mentioned: {}.", tool_names.join(", ")));
    }

    let recent_user_requests = collect_recent_role_summaries(messages, MessageRole::User, 3);
    if !recent_user_requests.is_empty() {
        lines.push("- Recent user requests:".to_string());
        lines.extend(
            recent_user_requests
                .into_iter()
                .map(|request| format!("  - {request}")),
        );
    }

    let pending_work = infer_pending_work(messages);
    if !pending_work.is_empty() {
        lines.push("- Pending work:".to_string());
        lines.extend(pending_work.into_iter().map(|item| format!("  - {item}")));
    }

    let key_files = collect_key_files(messages);
    if !key_files.is_empty() {
        lines.push(format!("- Key files referenced: {}.", key_files.join(", ")));
    }

    if let Some(current_work) = infer_current_work(messages) {
        lines.push(format!("- Current work: {current_work}"));
    }

    lines.push("- Key timeline:".to_string());
    for message in messages {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let content = message
            .blocks
            .iter()
            .map(summarize_block)
            .collect::<Vec<_>>()
            .join(" | ");
        lines.push(format!("  - {role}: {content}"));
    }
    lines.push("</summary>".to_string());
    lines.join("\n")
}

fn merge_compact_summaries(existing_summary: Option<&str>, new_summary: &str) -> String {
    let Some(existing_summary) = existing_summary else {
        return new_summary.to_string();
    };

    let previous_highlights = extract_summary_highlights(existing_summary);
    let new_formatted_summary = format_compact_summary(new_summary);
    let new_highlights = extract_summary_highlights(&new_formatted_summary);
    let new_timeline = extract_summary_timeline(&new_formatted_summary);

    let mut lines = vec!["<summary>".to_string(), "Conversation summary:".to_string()];

    if !previous_highlights.is_empty() {
        lines.push("- Previously compacted context:".to_string());
        lines.extend(
            previous_highlights
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }

    if !new_highlights.is_empty() {
        lines.push("- Newly compacted context:".to_string());
        lines.extend(new_highlights.into_iter().map(|line| format!("  {line}")));
    }

    if !new_timeline.is_empty() {
        lines.push("- Key timeline:".to_string());
        lines.extend(new_timeline.into_iter().map(|line| format!("  {line}")));
    }

    lines.push("</summary>".to_string());
    lines.join("\n")
}

fn summarize_block(block: &ContentBlock) -> String {
    let raw = match block {
        ContentBlock::Text { text } => text.clone(),
        ContentBlock::ToolUse { name, input, .. } => format!("tool_use {name}({input})"),
        ContentBlock::ToolResult {
            tool_name,
            output,
            is_error,
            ..
        } => format!(
            "tool_result {tool_name}: {}{output}",
            if *is_error { "error " } else { "" }
        ),
    };
    truncate_summary(&raw, 160)
}

fn collect_recent_role_summaries(
    messages: &[ConversationMessage],
    role: MessageRole,
    limit: usize,
) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == role)
        .rev()
        .filter_map(|message| first_text_block(message))
        .take(limit)
        .map(|text| truncate_summary(text, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn infer_pending_work(messages: &[ConversationMessage]) -> Vec<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text_block)
        .filter(|text| {
            let lowered = text.to_ascii_lowercase();
            lowered.contains("todo")
                || lowered.contains("next")
                || lowered.contains("pending")
                || lowered.contains("follow up")
                || lowered.contains("remaining")
        })
        .take(3)
        .map(|text| truncate_summary(text, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn collect_key_files(messages: &[ConversationMessage]) -> Vec<String> {
    let mut files = messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .map(|block| match block {
            ContentBlock::Text { text } => text.as_str(),
            ContentBlock::ToolUse { input, .. } => input.as_str(),
            ContentBlock::ToolResult { output, .. } => output.as_str(),
        })
        .flat_map(extract_file_candidates)
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files.into_iter().take(8).collect()
}

fn infer_current_work(messages: &[ConversationMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text_block)
        .find(|text| !text.trim().is_empty())
        .map(|text| truncate_summary(text, 200))
}

fn first_text_block(message: &ConversationMessage) -> Option<&str> {
    message.blocks.iter().find_map(|block| match block {
        ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
        ContentBlock::ToolUse { .. }
        | ContentBlock::ToolResult { .. }
        | ContentBlock::Text { .. } => None,
    })
}

fn has_interesting_extension(candidate: &str) -> bool {
    std::path::Path::new(candidate)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["rs", "ts", "tsx", "js", "json", "md"]
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

fn extract_file_candidates(content: &str) -> Vec<String> {
    content
        .split_whitespace()
        .filter_map(|token| {
            let candidate = token.trim_matches(|char: char| {
                matches!(char, ',' | '.' | ':' | ';' | ')' | '(' | '"' | '\'' | '`')
            });
            if candidate.contains('/') && has_interesting_extension(candidate) {
                Some(candidate.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn truncate_summary(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let mut truncated = content.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

fn estimate_message_tokens(message: &ConversationMessage) -> usize {
    let raw: usize = message
        .blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => estimate_text_tokens(text),
            ContentBlock::ToolUse { name, input, .. } => {
                // Tool name + JSON-serialized input
                estimate_text_tokens(name) + estimate_text_tokens(input)
            }
            ContentBlock::ToolResult {
                tool_name, output, ..
            } => estimate_text_tokens(tool_name) + estimate_text_tokens(output),
        })
        .sum();
    // Apply 4/3 conservative padding (CC pattern)
    raw * TOKEN_ESTIMATION_PADDING_NUM / TOKEN_ESTIMATION_PADDING_DEN
}

/// Estimate tokens for a text string (~1 token per 4 chars, minimum 1).
fn estimate_text_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.len().div_ceil(4)
}

fn extract_tag_block(content: &str, tag: &str) -> Option<String> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let start_index = content.find(&start)? + start.len();
    let end_index = content[start_index..].find(&end)? + start_index;
    Some(content[start_index..end_index].to_string())
}

fn strip_tag_block(content: &str, tag: &str) -> String {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    if let (Some(start_index), Some(end_index_rel)) = (content.find(&start), content.find(&end)) {
        let end_index = end_index_rel + end.len();
        let mut stripped = String::new();
        stripped.push_str(&content[..start_index]);
        stripped.push_str(&content[end_index..]);
        stripped
    } else {
        content.to_string()
    }
}

fn collapse_blank_lines(content: &str) -> String {
    let mut result = String::new();
    let mut last_blank = false;
    for line in content.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && last_blank {
            continue;
        }
        result.push_str(line);
        result.push('\n');
        last_blank = is_blank;
    }
    result
}

fn extract_existing_compacted_summary(message: &ConversationMessage) -> Option<String> {
    if message.role != MessageRole::System {
        return None;
    }

    let text = first_text_block(message)?;
    let summary = text.strip_prefix(COMPACT_CONTINUATION_PREAMBLE)?;
    let summary = summary
        .split_once(&format!("\n\n{COMPACT_RECENT_MESSAGES_NOTE}"))
        .map_or(summary, |(value, _)| value);
    let summary = summary
        .split_once(&format!("\n{COMPACT_DIRECT_RESUME_INSTRUCTION}"))
        .map_or(summary, |(value, _)| value);
    Some(summary.trim().to_string())
}

fn extract_summary_highlights(summary: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_timeline = false;

    for line in format_compact_summary(summary).lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed == "Summary:" || trimmed == "Conversation summary:" {
            continue;
        }
        if trimmed == "- Key timeline:" {
            in_timeline = true;
            continue;
        }
        if in_timeline {
            continue;
        }
        lines.push(trimmed.to_string());
    }

    lines
}

fn extract_summary_timeline(summary: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_timeline = false;

    for line in format_compact_summary(summary).lines() {
        let trimmed = line.trim_end();
        if trimmed == "- Key timeline:" {
            in_timeline = true;
            continue;
        }
        if !in_timeline {
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        lines.push(trimmed.to_string());
    }

    lines
}

// ── Pre-compact checkpoint ──────────────────────────────────────────────

/// State snapshot saved before compaction to preserve critical context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCompactCheckpoint {
    /// Active TODO items at time of compaction.
    pub pending_todos: Vec<String>,
    /// Plan mode state.
    pub plan_mode: bool,
    /// Number of messages before compaction.
    pub message_count: usize,
    /// Estimated tokens before compaction.
    pub estimated_tokens: usize,
    /// Any detected pending work items from recent messages.
    pub pending_work: Vec<String>,
}

/// Create a checkpoint from the current session state before compaction.
#[must_use]
pub fn create_pre_compact_checkpoint(session: &Session) -> PreCompactCheckpoint {
    let pending_work = infer_pending_work(&session.messages);

    // Extract TODO items from recent tool results.
    let pending_todos = session
        .messages
        .iter()
        .rev()
        .take(10)
        .flat_map(|msg| msg.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } if text.contains("TODO") || text.contains("todo") => {
                text.lines()
                    .filter(|line| {
                        let t = line.trim().to_ascii_lowercase();
                        t.contains("todo") || t.starts_with("- [ ]")
                    })
                    .map(|line| line.trim().to_string())
                    .next()
            }
            _ => None,
        })
        .collect();

    PreCompactCheckpoint {
        pending_todos,
        plan_mode: session.plan_mode,
        message_count: session.messages.len(),
        estimated_tokens: estimate_session_tokens(session),
        pending_work,
    }
}

/// Render a checkpoint as a text block suitable for injection after compaction.
#[must_use]
pub fn render_checkpoint_context(checkpoint: &PreCompactCheckpoint) -> Option<String> {
    let mut lines = Vec::new();

    if !checkpoint.pending_todos.is_empty() {
        lines.push("Pre-compaction state — pending TODOs:".to_string());
        for todo in &checkpoint.pending_todos {
            lines.push(format!("  {todo}"));
        }
    }

    if !checkpoint.pending_work.is_empty() {
        lines.push("Pre-compaction state — detected pending work:".to_string());
        for item in &checkpoint.pending_work {
            lines.push(format!("  - {item}"));
        }
    }

    if checkpoint.plan_mode {
        lines.push("Pre-compaction state — plan mode was active".to_string());
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_key_files, compact_session, estimate_session_tokens, format_compact_summary,
        get_compact_continuation_message, infer_pending_work, should_compact, CompactionConfig,
    };
    use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};

    #[test]
    fn formats_compact_summary_like_upstream() {
        let summary = "<analysis>scratch</analysis>\n<summary>Kept work</summary>";
        assert_eq!(format_compact_summary(summary), "Summary:\nKept work");
    }

    #[test]
    fn leaves_small_sessions_unchanged() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![ConversationMessage::user_text("hello")],
        };

        let result = compact_session(&session, CompactionConfig::default());
        assert_eq!(result.removed_message_count, 0);
        assert_eq!(result.compacted_session, session);
        assert!(result.summary.is_empty());
        assert!(result.formatted_summary.is_empty());
    }

    #[test]
    fn compacts_older_messages_into_a_system_summary() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![
                ConversationMessage::user_text("one ".repeat(200)),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "two ".repeat(200),
                }]),
                ConversationMessage::tool_result("1", "bash", "ok ".repeat(200), false),
                ConversationMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![ContentBlock::Text {
                        text: "recent".to_string(),
                    }],
                    usage: None,
                },
            ],
        };

        let result = compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
        );

        assert_eq!(result.removed_message_count, 2);
        assert_eq!(
            result.compacted_session.messages[0].role,
            MessageRole::System
        );
        assert!(matches!(
            &result.compacted_session.messages[0].blocks[0],
            ContentBlock::Text { text } if text.contains("Summary:")
        ));
        assert!(result.formatted_summary.contains("Scope:"));
        assert!(result.formatted_summary.contains("Key timeline:"));
        assert!(should_compact(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            }
        ));
        assert!(
            estimate_session_tokens(&result.compacted_session) < estimate_session_tokens(&session)
        );
    }

    #[test]
    fn keeps_previous_compacted_context_when_compacting_again() {
        let initial_session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![
                ConversationMessage::user_text("Investigate rust/crates/runtime/src/compact.rs"),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "I will inspect the compact flow.".to_string(),
                }]),
                ConversationMessage::user_text(
                    "Also update rust/crates/runtime/src/conversation.rs",
                ),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "Next: preserve prior summary context during auto compact.".to_string(),
                }]),
            ],
        };
        let config = CompactionConfig {
            preserve_recent_messages: 2,
            max_estimated_tokens: 1,
        };

        let first = compact_session(&initial_session, config);
        let mut follow_up_messages = first.compacted_session.messages.clone();
        follow_up_messages.extend([
            ConversationMessage::user_text("Please add regression tests for compaction."),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Working on regression coverage now.".to_string(),
            }]),
        ]);

        let second = compact_session(
            &Session {
                version: 1,
                plan_mode: false,
            messages: follow_up_messages,
            },
            config,
        );

        assert!(second
            .formatted_summary
            .contains("Previously compacted context:"));
        assert!(second
            .formatted_summary
            .contains("Scope: 2 earlier messages compacted"));
        assert!(second
            .formatted_summary
            .contains("Newly compacted context:"));
        assert!(second
            .formatted_summary
            .contains("Also update rust/crates/runtime/src/conversation.rs"));
        assert!(matches!(
            &second.compacted_session.messages[0].blocks[0],
            ContentBlock::Text { text }
                if text.contains("Previously compacted context:")
                    && text.contains("Newly compacted context:")
        ));
        assert!(matches!(
            &second.compacted_session.messages[1].blocks[0],
            ContentBlock::Text { text } if text.contains("Please add regression tests for compaction.")
        ));
    }

    #[test]
    fn ignores_existing_compacted_summary_when_deciding_to_recompact() {
        let summary = "<summary>Conversation summary:\n- Scope: earlier work preserved.\n- Key timeline:\n  - user: large preserved context\n</summary>";
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![
                ConversationMessage {
                    role: MessageRole::System,
                    blocks: vec![ContentBlock::Text {
                        text: get_compact_continuation_message(summary, true, true),
                    }],
                    usage: None,
                },
                ConversationMessage::user_text("tiny"),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "recent".to_string(),
                }]),
            ],
        };

        assert!(!should_compact(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            }
        ));
    }

    #[test]
    fn truncates_long_blocks_in_summary() {
        let summary = super::summarize_block(&ContentBlock::Text {
            text: "x".repeat(400),
        });
        assert!(summary.ends_with('…'));
        assert!(summary.chars().count() <= 161);
    }

    #[test]
    fn extracts_key_files_from_message_content() {
        let files = collect_key_files(&[ConversationMessage::user_text(
            "Update rust/crates/runtime/src/compact.rs and rust/crates/tools/src/lib.rs next.",
        )]);
        assert!(files.contains(&"rust/crates/runtime/src/compact.rs".to_string()));
        assert!(files.contains(&"rust/crates/tools/src/lib.rs".to_string()));
    }

    #[test]
    fn infers_pending_work_from_recent_messages() {
        let pending = infer_pending_work(&[
            ConversationMessage::user_text("done"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Next: update tests and follow up on remaining CLI polish.".to_string(),
            }]),
        ]);
        assert_eq!(pending.len(), 1);
        assert!(pending[0].contains("Next: update tests"));
    }

    #[test]
    fn compact_session_includes_pre_compact_checkpoint_context() {
        let session = Session {
            version: 1,
            plan_mode: true,
            messages: vec![
                ConversationMessage::user_text(
                    "TODO: keep the current execution checklist visible after compaction.",
                ),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "Next: wire the checkpoint back into the continuation system message.".to_string(),
                }]),
                ConversationMessage::user_text("word ".repeat(250)),
            ],
        };

        let result = compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 1,
                max_estimated_tokens: 1,
            },
        );

        let ContentBlock::Text { text } = &result.compacted_session.messages[0].blocks[0] else {
            panic!("expected compacted system summary");
        };

        assert!(text.contains("Pre-compaction state — pending TODOs:"));
        assert!(text.contains("TODO: keep the current execution checklist visible after compaction."));
        assert!(text.contains("Pre-compaction state — detected pending work:"));
        assert!(text.contains("Next: wire the checkpoint back into the continuation system message."));
        assert!(text.contains("Pre-compaction state — plan mode was active"));
        assert!(text.contains(super::COMPACT_DIRECT_RESUME_INSTRUCTION));
    }

    // ── Auto-compaction tests ──────────────────────────────────────────

    use super::{
        auto_compact_session, calculate_token_warning, micro_compact_session,
        should_auto_compact, AutoCompactConfig, AutoCompactState, TokenWarningLevel,
    };

    #[test]
    fn token_warning_levels_are_correct() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![ConversationMessage::user_text("x".repeat(4000))],
        };
        // ~1001 tokens (4000/4+1). With 128K window, should be Normal.
        let state = calculate_token_warning(&session, 128_000);
        assert_eq!(state.level, TokenWarningLevel::Normal);

        // With a tiny window, should be Critical.
        let state = calculate_token_warning(&session, 2_000);
        assert_eq!(state.level, TokenWarningLevel::Critical);
    }

    #[test]
    fn auto_compact_triggers_when_threshold_reached() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![ConversationMessage::user_text("x".repeat(40_000))],
        };
        let config = AutoCompactConfig {
            context_window: 12_000,
            buffer_tokens: 2_000,
            micro_compact_enabled: true,
            compaction: CompactionConfig {
                preserve_recent_messages: 1,
                max_estimated_tokens: 1,
            },
        };
        let state = AutoCompactState::new();
        assert!(should_auto_compact(&session, &config, &state));
    }

    #[test]
    fn auto_compact_suppressed_after_circuit_breaker() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![ConversationMessage::user_text("x".repeat(40_000))],
        };
        let config = AutoCompactConfig {
            context_window: 12_000,
            buffer_tokens: 2_000,
            ..AutoCompactConfig::default()
        };
        let mut state = AutoCompactState::new();
        state.record_failure();
        state.record_failure();
        state.record_failure();
        assert!(state.is_suppressed());
        assert!(!should_auto_compact(&session, &config, &state));
    }

    #[test]
    fn micro_compact_clears_compactable_tool_results() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![
                ConversationMessage::tool_result("1", "bash", "x".repeat(20_000), false),
                ConversationMessage::user_text("recent"),
            ],
        };
        let result = micro_compact_session(&session, 1);
        assert_eq!(result.cleared_count, 1);
        let ContentBlock::ToolResult { output, .. } = &result.session.messages[0].blocks[0] else {
            panic!("expected tool result");
        };
        // bash is compactable → content-cleared
        assert!(output.contains("[Old tool result content cleared]"));
        assert!(result.tokens_freed > 0);
    }

    #[test]
    fn micro_compact_truncates_non_compactable_oversized_results() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![
                // "CustomTool" is not in COMPACTABLE_TOOLS
                ConversationMessage::tool_result("1", "CustomTool", "z".repeat(20_000), false),
                ConversationMessage::user_text("recent"),
            ],
        };
        let result = micro_compact_session(&session, 1);
        assert_eq!(result.cleared_count, 1);
        let ContentBlock::ToolResult { output, .. } = &result.session.messages[0].blocks[0] else {
            panic!("expected tool result");
        };
        // Non-compactable → truncated (not fully cleared)
        assert!(output.len() < 20_000);
        assert!(output.contains("truncated"));
    }

    #[test]
    fn micro_compact_preserves_recent_messages() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![
                ConversationMessage::tool_result("1", "bash", "x".repeat(20_000), false),
                ConversationMessage::tool_result("2", "bash", "y".repeat(20_000), false),
            ],
        };
        // preserve_recent=1 means only the last message is preserved
        let result = micro_compact_session(&session, 1);
        assert!(result.cleared_count >= 1);
        let ContentBlock::ToolResult { output, .. } = &result.session.messages[1].blocks[0] else {
            panic!("expected tool result");
        };
        // The last message should be untouched
        assert_eq!(output.len(), 20_000);
    }

    #[test]
    fn auto_compact_session_uses_micro_first() {
        let session = Session {
            version: 1,
            plan_mode: false,
            messages: vec![
                ConversationMessage::tool_result("1", "bash", "x".repeat(50_000), false),
                ConversationMessage::user_text("recent"),
            ],
        };
        let config = AutoCompactConfig {
            context_window: 200_000,
            buffer_tokens: 13_000,
            micro_compact_enabled: true,
            compaction: CompactionConfig {
                preserve_recent_messages: 1,
                max_estimated_tokens: 1,
            },
        };
        let result = auto_compact_session(&session, &config);
        if let Some(result) = result {
            // Should have micro-compacted (truncated tool result) rather than
            // full compacted, because the session fits after truncation.
            assert!(result.summary.contains("Micro-compacted"));
            assert_eq!(result.removed_message_count, 0);
        }
    }

    #[test]
    fn auto_compact_state_tracks_successes_and_failures() {
        let mut state = AutoCompactState::new();
        assert!(!state.is_suppressed());

        state.record_failure();
        state.record_failure();
        assert!(!state.is_suppressed());

        state.record_success();
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.total_compactions, 1);

        state.record_failure();
        state.record_failure();
        state.record_failure();
        assert!(state.is_suppressed());
    }
}
