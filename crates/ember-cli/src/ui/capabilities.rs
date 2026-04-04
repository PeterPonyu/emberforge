use std::env;
use std::io::{self, IsTerminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorLevel {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphLevel {
    Ascii,
    UnicodeBasic,
    UnicodeBlocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub is_tty: bool,
    pub interactive: bool,
    pub width: u16,
    pub height: u16,
    pub color_level: ColorLevel,
    pub glyph_level: GlyphLevel,
    pub reduced_motion: bool,
}

impl TerminalCapabilities {
    #[must_use]
    pub fn detect() -> Self {
        let is_tty = io::stdout().is_terminal();
        let interactive = is_tty;
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        let term = env::var("TERM")
            .unwrap_or_else(|_| String::from("unknown"))
            .to_ascii_lowercase();
        let colorterm = env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let no_color = env::var_os("NO_COLOR").is_some();
        let reduced_motion = env_flag("EMBER_UI_REDUCED_MOTION")
            || env_flag("EMBER_REDUCED_MOTION")
            || env_flag("EMBER_UI_MOTION_REDUCED");
        let term_is_dumb = term == "dumb";

        let color_level = if !is_tty || no_color || term_is_dumb {
            ColorLevel::None
        } else if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            ColorLevel::TrueColor
        } else if term.contains("256color") || term.contains("256") {
            ColorLevel::Ansi256
        } else {
            ColorLevel::Ansi16
        };

        let glyph_level = if !is_tty || term_is_dumb || env_flag("EMBER_UI_ASCII_ONLY") {
            GlyphLevel::Ascii
        } else if cfg!(target_os = "windows") {
            GlyphLevel::UnicodeBasic
        } else {
            GlyphLevel::UnicodeBlocks
        };

        Self {
            is_tty,
            interactive,
            width,
            height,
            color_level,
            glyph_level,
            reduced_motion,
        }
    }

    #[must_use]
    pub fn supports_pixel_banner(self) -> bool {
        self.is_tty
            && self.width >= 48
            && self.color_level >= ColorLevel::Ansi16
            && matches!(self.glyph_level, GlyphLevel::UnicodeBlocks | GlyphLevel::UnicodeBasic)
    }

    #[must_use]
    pub fn prefers_pixel_banner(self) -> bool {
        self.supports_pixel_banner() && self.width >= 68 && self.color_level >= ColorLevel::Ansi256
    }

    #[must_use]
    pub fn color_enabled(self) -> bool {
        self.color_level != ColorLevel::None
    }
}

#[must_use]
pub fn detect_terminal_capabilities() -> TerminalCapabilities {
    TerminalCapabilities::detect()
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

#[cfg(test)]
mod tests {
    use super::{ColorLevel, GlyphLevel, TerminalCapabilities};

    #[test]
    fn prefers_pixel_banner_only_when_terminal_is_capable() {
        let caps = TerminalCapabilities {
            is_tty: true,
            interactive: true,
            width: 80,
            height: 24,
            color_level: ColorLevel::Ansi256,
            glyph_level: GlyphLevel::UnicodeBlocks,
            reduced_motion: false,
        };
        assert!(caps.prefers_pixel_banner());
        assert!(caps.supports_pixel_banner());
    }

    #[test]
    fn falls_back_when_terminal_is_too_narrow() {
        let caps = TerminalCapabilities {
            is_tty: true,
            interactive: true,
            width: 40,
            height: 24,
            color_level: ColorLevel::Ansi256,
            glyph_level: GlyphLevel::UnicodeBlocks,
            reduced_motion: false,
        };
        assert!(!caps.prefers_pixel_banner());
        assert!(!caps.supports_pixel_banner());
    }
}
