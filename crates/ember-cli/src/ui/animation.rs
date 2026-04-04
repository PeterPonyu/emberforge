use std::env;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, queue};
use runtime::{RuntimeUiAnimationMode, RuntimeUiBannerMode, RuntimeUiConfig};

use super::banner::effective_banner_mode;
use super::capabilities::{ColorLevel, TerminalCapabilities};

const INTRO_FRAMES: &[&[&str]] = &[
    &[
        "           ",
        "           ",
        "           ",
        "           ",
        "           ",
        "     oo    ",
        "    oyy    ",
        "     oo    ",
    ],
    &[
        "           ",
        "           ",
        "           ",
        "     rr    ",
        "    roor   ",
        "   royyor  ",
        "    oyyo   ",
        "     oo    ",
    ],
    &[
        "           ",
        "     rr    ",
        "    roor   ",
        "   royyor  ",
        "  royyyyor ",
        "   oyyyyo  ",
        "    oooo   ",
        "     oo    ",
    ],
];
const INTRO_FRAME_DELAY_MS: u64 = 90;

/// Compact single-line fire-pixel spinner frames rendered on stderr.
/// Each frame is a short string of pixel tokens that gets colorized inline.
const FIRE_SPINNER_FRAMES: &[&str] = &[
    " oyryo ",
    " oyYyo ",
    " rYyYr ",
    " oYyro ",
    " ryYor ",
    " oyrYo ",
];
const FIRE_SPINNER_DELAY_MS: u64 = 200;

/// Global stop signal for the fire spinner.
static FIRE_SPINNER_STOP: AtomicBool = AtomicBool::new(false);
/// Indicates the fire spinner thread is still running.
static FIRE_SPINNER_RUNNING: AtomicBool = AtomicBool::new(false);

#[must_use]
pub fn should_play_intro_animation(
    ui_config: &RuntimeUiConfig,
    capabilities: &TerminalCapabilities,
) -> bool {
    if !capabilities.interactive || !capabilities.is_tty || capabilities.reduced_motion {
        return false;
    }
    if effective_banner_mode(ui_config, capabilities) != RuntimeUiBannerMode::Pixel {
        return false;
    }

    match env_animation_mode_override().unwrap_or(ui_config.animation().mode()) {
        RuntimeUiAnimationMode::Off => false,
        RuntimeUiAnimationMode::Intro => true,
        RuntimeUiAnimationMode::Auto => capabilities.color_level >= ColorLevel::Ansi16,
    }
}

pub fn play_intro_animation(capabilities: &TerminalCapabilities) -> io::Result<()> {
    let mut stdout = io::stdout();
    let frame_height = INTRO_FRAMES.first().map_or(0, |frame| frame.len());

    execute!(stdout, Hide)?;
    for (index, frame) in INTRO_FRAMES.iter().enumerate() {
        if index > 0 {
            clear_previous_frame(&mut stdout, frame_height)?;
        }
        render_frame(&mut stdout, frame, capabilities)?;
        stdout.flush()?;
        thread::sleep(Duration::from_millis(INTRO_FRAME_DELAY_MS));
    }
    clear_previous_frame(&mut stdout, frame_height)?;
    execute!(stdout, Show)?;
    stdout.flush()?;
    Ok(())
}

/// Start a fire-pixel spinner on **stderr** (safe alongside stdout streaming).
///
/// Shows a compact animated fire with an optional label and elapsed time.
/// Call [`stop_fire_spinner`] then join the handle to clean up.
pub fn start_fire_spinner(label: &'static str, color_enabled: bool) -> thread::JoinHandle<()> {
    FIRE_SPINNER_STOP.store(false, Ordering::Relaxed);
    FIRE_SPINNER_RUNNING.store(true, Ordering::Relaxed);

    thread::spawn(move || {
        use crossterm::{cursor, terminal};

        let mut stderr = io::stderr();
        let _ = execute!(stderr, cursor::Hide);
        let start = Instant::now();
        let mut index = 0usize;

        while !FIRE_SPINNER_STOP.load(Ordering::Relaxed) {
            let frame = FIRE_SPINNER_FRAMES[index % FIRE_SPINNER_FRAMES.len()];
            let elapsed = start.elapsed().as_secs();
            let time_suffix = if elapsed > 0 {
                format!(" \x1b[2m({elapsed}s)\x1b[0m")
            } else {
                String::new()
            };

            let rendered = render_inline_fire(frame, color_enabled);

            let _ = execute!(
                stderr,
                cursor::MoveToColumn(0),
                terminal::Clear(terminal::ClearType::CurrentLine),
            );
            let _ = write!(
                stderr,
                "{rendered} \x1b[2m{label}\x1b[0m{time_suffix}"
            );
            let _ = stderr.flush();

            index += 1;
            // Sleep in small increments for responsive stop.
            for _ in 0..(FIRE_SPINNER_DELAY_MS / 25) {
                if FIRE_SPINNER_STOP.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }

        // Clear spinner line and restore cursor.
        let _ = execute!(
            stderr,
            cursor::MoveToColumn(0),
            terminal::Clear(terminal::ClearType::CurrentLine),
            cursor::Show,
        );
        FIRE_SPINNER_RUNNING.store(false, Ordering::Relaxed);
    })
}

/// Signal the fire spinner to stop.
pub fn stop_fire_spinner() {
    FIRE_SPINNER_STOP.store(true, Ordering::Relaxed);
}

/// Returns true if the fire spinner thread is still running.
#[must_use]
pub fn is_fire_spinner_running() -> bool {
    FIRE_SPINNER_RUNNING.load(Ordering::Relaxed)
}

/// Render a compact inline fire frame as a colored string.
fn render_inline_fire(frame: &str, color_enabled: bool) -> String {
    let mut out = String::new();
    for token in frame.chars() {
        if !color_enabled {
            match token {
                'r' | 'o' | 'y' | 'Y' => out.push('█'),
                _ => out.push(token),
            }
            continue;
        }
        match token {
            'r' => {
                out.push_str("\x1b[38;5;196m█\x1b[0m");
            }
            'o' => {
                out.push_str("\x1b[38;5;208m█\x1b[0m");
            }
            'y' => {
                out.push_str("\x1b[38;5;220m█\x1b[0m");
            }
            'Y' => {
                // Bright yellow/white for the hot core
                out.push_str("\x1b[38;5;229m█\x1b[0m");
            }
            _ => out.push(token),
        }
    }
    out
}

fn render_frame(
    out: &mut impl Write,
    frame: &[&str],
    capabilities: &TerminalCapabilities,
) -> io::Result<()> {
    for line in frame {
        render_frame_line(out, line, capabilities)?;
        writeln!(out)?;
    }
    Ok(())
}

fn render_frame_line(
    out: &mut impl Write,
    line: &str,
    capabilities: &TerminalCapabilities,
) -> io::Result<()> {
    for token in line.chars() {
        match token {
            ' ' => write!(out, " ")?,
            'r' => write_colored_block(out, capabilities, Color::AnsiValue(196))?,
            'o' => write_colored_block(out, capabilities, Color::AnsiValue(208))?,
            'y' => write_colored_block(out, capabilities, Color::AnsiValue(220))?,
            other => write!(out, "{other}")?,
        }
    }
    Ok(())
}

fn write_colored_block(
    out: &mut impl Write,
    capabilities: &TerminalCapabilities,
    color: Color,
) -> io::Result<()> {
    if capabilities.color_level == ColorLevel::None {
        write!(out, "█")
    } else {
        queue!(out, SetForegroundColor(color), Print('█'), ResetColor)
    }
}

fn clear_previous_frame(out: &mut impl Write, frame_height: usize) -> io::Result<()> {
    if frame_height == 0 {
        return Ok(());
    }

    for index in 0..frame_height {
        execute!(out, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        if index + 1 < frame_height {
            execute!(out, MoveUp(1))?;
        }
    }
    execute!(out, MoveToColumn(0))?;
    Ok(())
}

fn env_animation_mode_override() -> Option<RuntimeUiAnimationMode> {
    parse_animation_mode(env::var("EMBER_UI_ANIMATION").ok()?.trim())
}

fn parse_animation_mode(value: &str) -> Option<RuntimeUiAnimationMode> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Some(RuntimeUiAnimationMode::Off),
        "intro" => Some(RuntimeUiAnimationMode::Intro),
        "auto" => Some(RuntimeUiAnimationMode::Auto),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use runtime::{RuntimeUiAnimationMode, RuntimeUiBannerMode, RuntimeUiBannerVariant, RuntimeUiConfig};

    use super::{render_inline_fire, should_play_intro_animation};
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

    #[test]
    fn intro_animation_runs_for_auto_pixel_startup() {
        let config = RuntimeUiConfig::default()
            .with_banner(RuntimeUiBannerMode::Auto, RuntimeUiBannerVariant::Auto)
            .with_animation_mode(RuntimeUiAnimationMode::Auto);
        assert!(should_play_intro_animation(&config, &capable_terminal()));
    }

    #[test]
    fn reduced_motion_disables_intro_animation() {
        let config = RuntimeUiConfig::default()
            .with_banner(RuntimeUiBannerMode::Auto, RuntimeUiBannerVariant::Auto)
            .with_animation_mode(RuntimeUiAnimationMode::Intro)
            .with_motion_reduced(true);
        let terminal = TerminalCapabilities {
            reduced_motion: true,
            ..capable_terminal()
        };
        assert!(!should_play_intro_animation(&config, &terminal));
    }

    #[test]
    fn inline_fire_renders_blocks_with_color() {
        let colored = render_inline_fire(" ory ", true);
        assert!(colored.contains("\x1b[38;5;208m█\x1b[0m")); // orange
        assert!(colored.contains("\x1b[38;5;196m█\x1b[0m")); // red
        assert!(colored.contains("\x1b[38;5;220m█\x1b[0m")); // yellow
    }

    #[test]
    fn inline_fire_renders_plain_blocks_without_color() {
        let plain = render_inline_fire(" ory ", false);
        assert_eq!(plain, " ███ ");
    }
}
