use std::env;

use runtime::{
    RuntimeUiBannerMode, RuntimeUiBannerVariant, RuntimeUiConfig,
};

use super::capabilities::TerminalCapabilities;

const CLASSIC_FIRE_ART: &[&str] = &[
    "          (  )",
    "         (    )",
    "        (  ()  )",
    "         (    )",
    "          |  |",
    "         /____\\",
];

const PIXEL_EMBER_ART: &[&str] = &[
    "     rr     ",
    "    roor    ",
    "   royyor   ",
    "  royyyyor  ",
    "  oyyyyyyo  ",
    "   oowwoo   ",
    "    oooo    ",
    "     oo     ",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupBannerContext {
    pub app_name: String,
    pub version: String,
    pub workspace_summary: String,
    pub model: String,
    pub provider_label: String,
    pub session_id: String,
    pub quick_start: String,
    pub show_setup_hint: bool,
}

pub fn render_startup_banner(
    context: &StartupBannerContext,
    capabilities: &TerminalCapabilities,
    ui_config: &RuntimeUiConfig,
) -> String {
    match effective_banner_mode(ui_config, capabilities) {
        RuntimeUiBannerMode::Off => render_metadata_only(context, capabilities),
        RuntimeUiBannerMode::Classic => render_classic_banner(context, capabilities),
        RuntimeUiBannerMode::Pixel => {
            render_pixel_banner(context, capabilities, effective_banner_variant(ui_config, capabilities))
        }
        RuntimeUiBannerMode::Auto => render_classic_banner(context, capabilities),
    }
}

#[must_use]
pub fn effective_banner_mode(
    ui_config: &RuntimeUiConfig,
    capabilities: &TerminalCapabilities,
) -> RuntimeUiBannerMode {
    let requested = env_banner_mode_override().unwrap_or(ui_config.banner().mode());
    match requested {
        RuntimeUiBannerMode::Auto => {
            if capabilities.prefers_pixel_banner() {
                RuntimeUiBannerMode::Pixel
            } else {
                RuntimeUiBannerMode::Classic
            }
        }
        RuntimeUiBannerMode::Pixel => {
            if capabilities.supports_pixel_banner() {
                RuntimeUiBannerMode::Pixel
            } else {
                RuntimeUiBannerMode::Classic
            }
        }
        other => other,
    }
}

fn effective_banner_variant(
    ui_config: &RuntimeUiConfig,
    capabilities: &TerminalCapabilities,
) -> RuntimeUiBannerVariant {
    match env_banner_variant_override().unwrap_or(ui_config.banner().variant()) {
        RuntimeUiBannerVariant::Auto => {
            if capabilities.width >= 72 {
                RuntimeUiBannerVariant::Wide
            } else {
                RuntimeUiBannerVariant::Compact
            }
        }
        other => other,
    }
}

fn render_classic_banner(
    context: &StartupBannerContext,
    capabilities: &TerminalCapabilities,
) -> String {
    let colors = BannerColors::new(capabilities.color_enabled());
    let bar = "-".repeat(usize::from(capabilities.width).min(80));
    let mut lines = CLASSIC_FIRE_ART
        .iter()
        .map(|line| colors.ember_bold(line))
        .collect::<Vec<_>>();
    lines.extend(build_metadata_lines(context, &colors, &bar));
    lines.join("\n")
}

fn render_pixel_banner(
    context: &StartupBannerContext,
    capabilities: &TerminalCapabilities,
    variant: RuntimeUiBannerVariant,
) -> String {
    let colors = BannerColors::new(capabilities.color_enabled());
    let art_lines = render_pixel_art(capabilities.color_enabled());
    let art_width = PIXEL_EMBER_ART
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);

    match variant {
        RuntimeUiBannerVariant::Wide => {
            let available = usize::from(capabilities.width)
                .saturating_sub(art_width + 3)
                .clamp(28, 52);
            let mut info_lines = build_metadata_lines(context, &colors, &"-".repeat(available));
            info_lines.insert(
                1,
                format!(
                    "{}{}{}",
                    colors.dim_prefix,
                    "  Built for local coding workflows",
                    colors.reset
                ),
            );
            combine_columns(&art_lines, &info_lines, art_width, 3).join("\n")
        }
        RuntimeUiBannerVariant::Auto | RuntimeUiBannerVariant::Compact => {
            let bar = "-".repeat(usize::from(capabilities.width).saturating_sub(4).clamp(24, 52));
            let mut lines = art_lines;
            lines.push(format!(
                "{}{}{} {}v{}{}{}",
                colors.orange_prefix,
                colors.bold_prefix,
                context.app_name,
                colors.dim_prefix,
                context.version,
                colors.reset,
                if capabilities.color_enabled() { "" } else { "" }
            ));
            lines.push(format!(
                "{}{}{}",
                colors.dim_prefix,
                "terminal coding tool",
                colors.reset
            ));
            lines.extend(build_metadata_rows_only(context, &colors, &bar));
            lines.join("\n")
        }
    }
}

fn render_metadata_only(
    context: &StartupBannerContext,
    capabilities: &TerminalCapabilities,
) -> String {
    let colors = BannerColors::new(capabilities.color_enabled());
    let bar = "-".repeat(usize::from(capabilities.width).saturating_sub(4).clamp(24, 52));
    build_metadata_lines(context, &colors, &bar).join("\n")
}

fn build_metadata_lines(
    context: &StartupBannerContext,
    colors: &BannerColors,
    bar: &str,
) -> Vec<String> {
    let mut lines = vec![
        format!(
            "{}{}{} {}v{}{} - {}{}{}",
            colors.orange_prefix,
            colors.bold_prefix,
            context.app_name,
            colors.dim_prefix,
            context.version,
            colors.reset,
            colors.dim_prefix,
            "terminal coding tool",
            colors.reset,
        ),
        format!("{}{}{}", colors.dim_prefix, bar, colors.reset),
    ];
    lines.extend(build_metadata_rows_only(context, colors, bar));
    lines
}

fn build_metadata_rows_only(
    context: &StartupBannerContext,
    colors: &BannerColors,
    bar: &str,
) -> Vec<String> {
    let mut lines = vec![
        format!(
            "  {}workspace{}  {}{}{}",
            colors.dim_prefix,
            colors.reset,
            colors.cyan_prefix,
            context.workspace_summary,
            colors.reset,
        ),
        format!(
            "  {}model{}      {}{}{} {}({}){}",
            colors.dim_prefix,
            colors.reset,
            colors.green_prefix,
            context.model,
            colors.reset,
            colors.dim_prefix,
            context.provider_label,
            colors.reset,
        ),
        format!(
            "  {}session{}    {}{}{}",
            colors.dim_prefix,
            colors.reset,
            colors.dim_prefix,
            context.session_id,
            colors.reset,
        ),
    ];
    if context.show_setup_hint {
        lines.push(format!(
            "  {}setup{}      /init to scaffold project config",
            colors.dim_prefix, colors.reset,
        ));
    }
    lines.push(format!(
        "  {}commands{}   {} | /verbose | /model",
        colors.dim_prefix,
        colors.reset,
        context.quick_start,
    ));
    lines.push(format!("{}{}{}", colors.dim_prefix, bar, colors.reset));
    lines.push(String::new());
    lines
}

fn render_pixel_art(color_enabled: bool) -> Vec<String> {
    PIXEL_EMBER_ART
        .iter()
        .map(|line| render_pixel_row(line, color_enabled))
        .collect()
}

fn render_pixel_row(row: &str, color_enabled: bool) -> String {
    let mut rendered = String::new();
    for token in row.chars() {
        match token {
            ' ' => rendered.push(' '),
            'r' => push_colored_block(&mut rendered, color_enabled, "38;5;196"),
            'o' => push_colored_block(&mut rendered, color_enabled, "38;5;208"),
            'y' => push_colored_block(&mut rendered, color_enabled, "38;5;220"),
            'w' => push_colored_block(&mut rendered, color_enabled, "97"),
            other => rendered.push(other),
        }
    }
    rendered
}

fn push_colored_block(output: &mut String, color_enabled: bool, ansi_code: &str) {
    if color_enabled {
        output.push_str("\x1b[");
        output.push_str(ansi_code);
        output.push('m');
        output.push('█');
        output.push_str("\x1b[0m");
    } else {
        output.push('█');
    }
}

fn combine_columns(
    left: &[String],
    right: &[String],
    left_width: usize,
    gap: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let max_lines = left.len().max(right.len());
    let blank_prefix = format!("{}{}", " ".repeat(left_width), " ".repeat(gap));
    let gap_padding = " ".repeat(gap);

    for index in 0..max_lines {
        match (left.get(index), right.get(index)) {
            (Some(left_line), Some(right_line)) => {
                lines.push(format!("{left_line}{gap_padding}{right_line}"));
            }
            (Some(left_line), None) => lines.push(left_line.clone()),
            (None, Some(right_line)) => lines.push(format!("{blank_prefix}{right_line}")),
            (None, None) => {}
        }
    }

    lines
}

fn env_banner_mode_override() -> Option<RuntimeUiBannerMode> {
    parse_banner_mode(env::var("EMBER_UI_BANNER").ok()?.trim())
}

fn env_banner_variant_override() -> Option<RuntimeUiBannerVariant> {
    parse_banner_variant(env::var("EMBER_UI_BANNER_VARIANT").ok()?.trim())
}

fn parse_banner_mode(value: &str) -> Option<RuntimeUiBannerMode> {
    match value.to_ascii_lowercase().as_str() {
        "classic" => Some(RuntimeUiBannerMode::Classic),
        "pixel" => Some(RuntimeUiBannerMode::Pixel),
        "auto" => Some(RuntimeUiBannerMode::Auto),
        "off" => Some(RuntimeUiBannerMode::Off),
        _ => None,
    }
}

fn parse_banner_variant(value: &str) -> Option<RuntimeUiBannerVariant> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Some(RuntimeUiBannerVariant::Auto),
        "compact" => Some(RuntimeUiBannerVariant::Compact),
        "wide" => Some(RuntimeUiBannerVariant::Wide),
        _ => None,
    }
}

struct BannerColors {
    reset: &'static str,
    bold_prefix: &'static str,
    dim_prefix: &'static str,
    orange_prefix: &'static str,
    cyan_prefix: &'static str,
    green_prefix: &'static str,
}

impl BannerColors {
    fn new(color_enabled: bool) -> Self {
        if color_enabled {
            Self {
                reset: "\x1b[0m",
                bold_prefix: "\x1b[1m",
                dim_prefix: "\x1b[2m",
                orange_prefix: "\x1b[38;5;208m",
                cyan_prefix: "\x1b[36m",
                green_prefix: "\x1b[32m",
            }
        } else {
            Self {
                reset: "",
                bold_prefix: "",
                dim_prefix: "",
                orange_prefix: "",
                cyan_prefix: "",
                green_prefix: "",
            }
        }
    }

    fn ember_bold(&self, line: &str) -> String {
        format!(
            "{}{}{}{}",
            self.orange_prefix, self.bold_prefix, line, self.reset
        )
    }
}

#[cfg(test)]
mod tests {
    use runtime::{RuntimeUiBannerMode, RuntimeUiBannerVariant, RuntimeUiConfig};

    use super::{
        effective_banner_mode, render_startup_banner, StartupBannerContext,
    };
    use crate::ui::capabilities::{ColorLevel, GlyphLevel, TerminalCapabilities};

    fn banner_context() -> StartupBannerContext {
        StartupBannerContext {
            app_name: String::from("Emberforge"),
            version: String::from("0.1.0"),
            workspace_summary: String::from("emberforge - main"),
            model: String::from("qwen3:4b"),
            provider_label: String::from("Ollama"),
            session_id: String::from("session-123"),
            quick_start: String::from("/init | /help"),
            show_setup_hint: true,
        }
    }

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

    fn narrow_terminal() -> TerminalCapabilities {
        TerminalCapabilities {
            width: 40,
            ..capable_terminal()
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
    fn classic_banner_preserves_legacy_fire() {
        let banner = render_startup_banner(
            &banner_context(),
            &capable_terminal(),
            &RuntimeUiConfig::default().with_banner(
                RuntimeUiBannerMode::Classic,
                RuntimeUiBannerVariant::Auto,
            ),
        );
        let plain_text = strip_ansi(&banner);

        assert!(plain_text.contains("/____\\"));
        assert!(plain_text.contains("Emberforge v0.1.0"));
    }

    #[test]
    fn default_banner_mode_prefers_pixel_on_capable_terminals() {
        let banner = render_startup_banner(
            &banner_context(),
            &capable_terminal(),
            &RuntimeUiConfig::default(),
        );
        let plain_text = strip_ansi(&banner);

        assert_eq!(
            effective_banner_mode(&RuntimeUiConfig::default(), &capable_terminal()),
            RuntimeUiBannerMode::Pixel
        );
        assert!(plain_text.contains('█'));
        assert!(!plain_text.contains("/____\\"));
    }

    #[test]
    fn auto_mode_prefers_pixel_banner_when_terminal_supports_it() {
        let config = RuntimeUiConfig::default().with_banner(
            RuntimeUiBannerMode::Auto,
            RuntimeUiBannerVariant::Auto,
        );
        let banner = render_startup_banner(&banner_context(), &capable_terminal(), &config);
        let plain_text = strip_ansi(&banner);

        assert_eq!(
            effective_banner_mode(&config, &capable_terminal()),
            RuntimeUiBannerMode::Pixel
        );
        assert!(plain_text.contains('█'));
        assert!(!plain_text.contains("/____\\"));
    }

    #[test]
    fn pixel_mode_falls_back_to_classic_for_narrow_terminals() {
        let config = RuntimeUiConfig::default().with_banner(
            RuntimeUiBannerMode::Pixel,
            RuntimeUiBannerVariant::Wide,
        );
        let banner = render_startup_banner(&banner_context(), &narrow_terminal(), &config);
        let plain_text = strip_ansi(&banner);

        assert_eq!(
            effective_banner_mode(&config, &narrow_terminal()),
            RuntimeUiBannerMode::Classic
        );
        assert!(plain_text.contains("/____\\"));
    }
}
