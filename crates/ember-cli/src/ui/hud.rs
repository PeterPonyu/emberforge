use std::env;

use runtime::{RuntimeUiConfig, RuntimeUiHudPreset};

use super::capabilities::TerminalCapabilities;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnHudContext {
    pub git_branch: Option<String>,
    pub model: String,
    pub provider_label: String,
    pub permission_mode: String,
    pub turns: u32,
    pub estimated_tokens: usize,
    pub background_task_count: usize,
    pub session_task_count: usize,
    pub session_id: String,
    pub effort: String,
    pub cumulative_input_tokens: u32,
    pub cumulative_output_tokens: u32,
    pub thinking_visible: bool,
}

pub fn render_turn_hud(
    context: &TurnHudContext,
    capabilities: &TerminalCapabilities,
    ui_config: &RuntimeUiConfig,
) -> Option<String> {
    if !capabilities.is_tty || !capabilities.interactive {
        return None;
    }

    let preset = effective_hud_preset(ui_config);
    if preset == RuntimeUiHudPreset::Off {
        return None;
    }

    let colors = HudColors::new(capabilities.color_enabled());
    let mut parts = Vec::new();

    if let Some(branch) = context.git_branch.as_deref().and_then(sanitize) {
        parts.push(format!("{}branch:{}{}", colors.accent_prefix, branch, colors.reset));
    }
    parts.push(format!("{}model:{}{}", colors.green_prefix, sanitize(&context.model)?, colors.reset));

    match preset {
        RuntimeUiHudPreset::Off => return None,
        RuntimeUiHudPreset::Minimal => {
            if context.effort != "balanced" {
                parts.push(format!("{}effort:{}{}", colors.yellow_prefix, context.effort, colors.reset));
            }
        }
        RuntimeUiHudPreset::Focused => {
            parts.push(format!("{}perm:{}{}", colors.dim_prefix, sanitize(&context.permission_mode)?, colors.reset));
            parts.push(format!("{}turns:{}{}", colors.dim_prefix, context.turns, colors.reset));
            if context.effort != "balanced" {
                parts.push(format!("{}effort:{}{}", colors.yellow_prefix, context.effort, colors.reset));
            }
            if context.thinking_visible {
                parts.push(format!("{}thinking:on{}", colors.accent_prefix, colors.reset));
            }
            parts.push(format!(
                "{}tasks:{}{}",
                colors.dim_prefix,
                format_task_count(context.session_task_count, context.background_task_count),
                colors.reset
            ));
        }
        RuntimeUiHudPreset::Full => {
            parts.push(format!("{}provider:{}{}", colors.dim_prefix, sanitize(&context.provider_label)?, colors.reset));
            parts.push(format!("{}perm:{}{}", colors.dim_prefix, sanitize(&context.permission_mode)?, colors.reset));
            parts.push(format!("{}effort:{}{}", colors.yellow_prefix, context.effort, colors.reset));
            parts.push(format!("{}turns:{}{}", colors.dim_prefix, context.turns, colors.reset));
            parts.push(format!("{}ctx:{}{}", colors.dim_prefix, format_token_count(context.estimated_tokens), colors.reset));
            parts.push(format!("{}cost:{}in/{}out{}", colors.dim_prefix,
                format_token_count(context.cumulative_input_tokens as usize),
                format_token_count(context.cumulative_output_tokens as usize),
                colors.reset));
            if context.thinking_visible {
                parts.push(format!("{}thinking:on{}", colors.accent_prefix, colors.reset));
            }
            parts.push(format!(
                "{}tasks:{}{}",
                colors.dim_prefix,
                format_task_count(context.session_task_count, context.background_task_count),
                colors.reset
            ));
            parts.push(format!("{}session:{}{}", colors.dim_prefix, shorten_session_id(&context.session_id), colors.reset));
        }
    }

    let separator = format!("{} | {}", colors.dim_prefix, colors.reset);
    Some(format!(
        "{}[hud]{} {}",
        colors.orange_prefix,
        colors.reset,
        parts.join(&separator)
    ))
}

#[must_use]
pub fn effective_hud_preset(ui_config: &RuntimeUiConfig) -> RuntimeUiHudPreset {
    env_hud_preset_override().unwrap_or(ui_config.hud().preset())
}

fn env_hud_preset_override() -> Option<RuntimeUiHudPreset> {
    parse_hud_preset(env::var("EMBER_UI_HUD").ok()?.trim())
}

fn parse_hud_preset(value: &str) -> Option<RuntimeUiHudPreset> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Some(RuntimeUiHudPreset::Off),
        "minimal" => Some(RuntimeUiHudPreset::Minimal),
        "focused" => Some(RuntimeUiHudPreset::Focused),
        "full" => Some(RuntimeUiHudPreset::Full),
        _ => None,
    }
}

fn sanitize(value: &str) -> Option<String> {
    let sanitized = value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn shorten_session_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= 16 {
        trimmed.to_string()
    } else {
        trimmed[trimmed.len() - 16..].to_string()
    }
}

fn format_task_count(session_tasks: usize, total_tasks: usize) -> String {
    match (session_tasks, total_tasks) {
        (_, 0) => String::from("0 total"),
        (0, total) => format!("{total} total"),
        (session, total) if session == total => format!("{session} session"),
        (session, total) => format!("{session} session / {total} total"),
    }
}

fn format_token_count(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

struct HudColors {
    reset: &'static str,
    orange_prefix: &'static str,
    accent_prefix: &'static str,
    green_prefix: &'static str,
    yellow_prefix: &'static str,
    dim_prefix: &'static str,
}

impl HudColors {
    fn new(color_enabled: bool) -> Self {
        if color_enabled {
            Self {
                reset: "\x1b[0m",
                orange_prefix: "\x1b[38;5;208m",
                accent_prefix: "\x1b[36m",
                green_prefix: "\x1b[32m",
                yellow_prefix: "\x1b[33m",
                dim_prefix: "\x1b[2m",
            }
        } else {
            Self {
                reset: "",
                orange_prefix: "",
                accent_prefix: "",
                green_prefix: "",
                yellow_prefix: "",
                dim_prefix: "",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use runtime::{RuntimeUiConfig, RuntimeUiHudPreset};

    use super::{format_task_count, render_turn_hud, TurnHudContext};
    use crate::ui::capabilities::{ColorLevel, GlyphLevel, TerminalCapabilities};

    fn capable_terminal() -> TerminalCapabilities {
        TerminalCapabilities {
            is_tty: true,
            interactive: true,
            width: 96,
            height: 28,
            color_level: ColorLevel::Ansi256,
            glyph_level: GlyphLevel::UnicodeBlocks,
            reduced_motion: false,
        }
    }

    fn context() -> TurnHudContext {
        TurnHudContext {
            git_branch: Some(String::from("main")),
            model: String::from("qwen3:4b"),
            provider_label: String::from("Ollama"),
            permission_mode: String::from("danger-full-access"),
            turns: 3,
            estimated_tokens: 1450,
            background_task_count: 2,
            session_task_count: 1,
            session_id: String::from("session-1234567890abcdef"),
            effort: String::from("balanced"),
            cumulative_input_tokens: 5000,
            cumulative_output_tokens: 1200,
            thinking_visible: false,
        }
    }

    fn strip_ansi(input: &str) -> String {
        let mut output = String::new();
        let mut chars = input.chars().peekable();

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
            output.push(ch);
        }

        output
    }

    #[test]
    fn minimal_hud_renders_label_branch_and_model() {
        let config = RuntimeUiConfig::default().with_hud_preset(RuntimeUiHudPreset::Minimal);
        let hud = render_turn_hud(&context(), &capable_terminal(), &config).expect("hud should render");
        let plain = strip_ansi(&hud);

        assert!(plain.contains("[hud]"));
        assert!(plain.contains("branch:main"));
        assert!(plain.contains("model:qwen3:4b"));
        assert!(!plain.contains("tokens:"));
    }

    #[test]
    fn full_hud_includes_extended_metrics() {
        let config = RuntimeUiConfig::default().with_hud_preset(RuntimeUiHudPreset::Full);
        let hud = render_turn_hud(&context(), &capable_terminal(), &config).expect("hud should render");
        let plain = strip_ansi(&hud);

        assert!(plain.contains("provider:Ollama"));
        assert!(plain.contains("ctx:1.4k"));
        assert!(plain.contains("effort:balanced"));
        assert!(plain.contains("cost:5.0kin/1.2kout"));
        assert!(plain.contains("tasks:1 session / 2 total"));
        assert!(plain.contains("session:1234567890abcdef"));
    }

    #[test]
    fn task_count_strings_are_explicit_about_session_scope() {
        assert_eq!(format_task_count(0, 0), "0 total");
        assert_eq!(format_task_count(0, 3), "3 total");
        assert_eq!(format_task_count(2, 2), "2 session");
        assert_eq!(format_task_count(1, 3), "1 session / 3 total");
    }

    #[test]
    fn off_hud_returns_none() {
        let config = RuntimeUiConfig::default().with_hud_preset(RuntimeUiHudPreset::Off);
        assert!(render_turn_hud(&context(), &capable_terminal(), &config).is_none());
    }
}
