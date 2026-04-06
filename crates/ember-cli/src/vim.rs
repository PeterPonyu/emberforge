//! Vim mode for the REPL input editor.
//!
//! Implements a subset of vim keybindings for the interactive line editor,
//! mirroring the Claude Code TypeScript `vim/` module.

use std::fmt;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The current vim editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    Replace,
}

impl fmt::Display for VimMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normal => write!(f, "NORMAL"),
            Self::Insert => write!(f, "INSERT"),
            Self::Visual => write!(f, "VISUAL"),
            Self::Replace => write!(f, "REPLACE"),
        }
    }
}

/// A vim action resulting from processing a key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimAction {
    /// No change — key was absorbed but had no effect.
    Noop,
    /// Insert a character at the cursor.
    InsertChar(char),
    /// Delete character(s) at/around the cursor.
    Delete { count: usize, direction: Direction },
    /// Move the cursor.
    MoveCursor(Direction),
    /// Move to start/end of line.
    Home,
    End,
    /// Move by word.
    WordForward,
    WordBackward,
    /// Switch mode.
    SwitchMode(VimMode),
    /// Submit the current line (Enter in insert mode).
    Submit,
    /// Delete the current line.
    DeleteLine,
    /// Yank (copy) the current line.
    YankLine,
    /// Paste after cursor.
    Paste,
    /// Undo last change.
    Undo,
    /// Redo last undo.
    Redo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

// ---------------------------------------------------------------------------
// Vim state machine
// ---------------------------------------------------------------------------

/// Manages vim state and translates key events into actions.
#[derive(Debug)]
pub struct VimState {
    pub mode: VimMode,
    pub enabled: bool,
    pending_count: Option<u32>,
    yank_buffer: String,
}

impl VimState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: VimMode::Normal,
            enabled: false,
            pending_count: None,
            yank_buffer: String::new(),
        }
    }

    /// Toggle vim mode on/off.
    pub fn toggle(&mut self) -> bool {
        self.enabled = !self.enabled;
        if self.enabled {
            self.mode = VimMode::Normal;
        } else {
            self.mode = VimMode::Insert; // default: behave like normal editor
        }
        self.enabled
    }

    /// Process a key character and return the resulting action.
    pub fn process_key(&mut self, ch: char) -> VimAction {
        if !self.enabled {
            return VimAction::InsertChar(ch);
        }

        match self.mode {
            VimMode::Normal => self.process_normal(ch),
            VimMode::Insert => self.process_insert(ch),
            VimMode::Visual => self.process_visual(ch),
            VimMode::Replace => self.process_replace(ch),
        }
    }

    /// Process escape key.
    pub fn process_escape(&mut self) -> VimAction {
        if !self.enabled {
            return VimAction::Noop;
        }
        self.pending_count = None;
        self.mode = VimMode::Normal;
        VimAction::SwitchMode(VimMode::Normal)
    }

    fn repeat_count(&mut self) -> usize {
        let count = self.pending_count.unwrap_or(1) as usize;
        self.pending_count = None;
        count
    }

    fn process_normal(&mut self, ch: char) -> VimAction {
        // Digit prefix for repeat count
        if ch.is_ascii_digit() && (self.pending_count.is_some() || ch != '0') {
            let digit = ch.to_digit(10).unwrap_or(0);
            self.pending_count = Some(self.pending_count.unwrap_or(0) * 10 + digit);
            return VimAction::Noop;
        }

        match ch {
            // ── Mode switches ──
            'i' => {
                self.mode = VimMode::Insert;
                VimAction::SwitchMode(VimMode::Insert)
            }
            'a' => {
                self.mode = VimMode::Insert;
                VimAction::MoveCursor(Direction::Right)
            }
            'A' => {
                self.mode = VimMode::Insert;
                VimAction::End
            }
            'I' => {
                self.mode = VimMode::Insert;
                VimAction::Home
            }
            'v' => {
                self.mode = VimMode::Visual;
                VimAction::SwitchMode(VimMode::Visual)
            }
            'R' => {
                self.mode = VimMode::Replace;
                VimAction::SwitchMode(VimMode::Replace)
            }

            // ── Movement ──
            'h' => VimAction::MoveCursor(Direction::Left),
            'l' => VimAction::MoveCursor(Direction::Right),
            'j' => VimAction::MoveCursor(Direction::Down),
            'k' => VimAction::MoveCursor(Direction::Up),
            '0' | '^' => VimAction::Home,
            '$' => VimAction::End,
            'w' => VimAction::WordForward,
            'b' => VimAction::WordBackward,

            // ── Editing ──
            'x' => {
                let count = self.repeat_count();
                VimAction::Delete { count, direction: Direction::Right }
            }
            'X' => {
                let count = self.repeat_count();
                VimAction::Delete { count, direction: Direction::Left }
            }
            'd' => {
                // Simplified: `dd` deletes line, `d` alone waits but we simplify
                VimAction::DeleteLine
            }
            'y' => VimAction::YankLine,
            'p' => VimAction::Paste,
            'u' => VimAction::Undo,

            // ── Special ──
            'o' => {
                self.mode = VimMode::Insert;
                VimAction::End // simplified: append newline
            }

            _ => VimAction::Noop,
        }
    }

    fn process_insert(&mut self, ch: char) -> VimAction {
        match ch {
            '\n' | '\r' => VimAction::Submit,
            _ => VimAction::InsertChar(ch),
        }
    }

    fn process_visual(&mut self, ch: char) -> VimAction {
        match ch {
            'h' => VimAction::MoveCursor(Direction::Left),
            'l' => VimAction::MoveCursor(Direction::Right),
            'j' => VimAction::MoveCursor(Direction::Down),
            'k' => VimAction::MoveCursor(Direction::Up),
            'd' | 'x' => {
                self.mode = VimMode::Normal;
                VimAction::Delete { count: 1, direction: Direction::Right }
            }
            'y' => {
                self.mode = VimMode::Normal;
                VimAction::YankLine
            }
            _ => VimAction::Noop,
        }
    }

    fn process_replace(&mut self, ch: char) -> VimAction {
        self.mode = VimMode::Normal;
        VimAction::InsertChar(ch)
    }

    /// Get the yank buffer contents.
    #[must_use]
    pub fn yank_buffer(&self) -> &str {
        &self.yank_buffer
    }

    /// Set the yank buffer.
    pub fn set_yank_buffer(&mut self, content: String) {
        self.yank_buffer = content;
    }
}

impl Default for VimState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disabled() {
        let vim = VimState::new();
        assert!(!vim.enabled);
    }

    #[test]
    fn toggle_enables_normal_mode() {
        let mut vim = VimState::new();
        assert!(vim.toggle());
        assert!(vim.enabled);
        assert_eq!(vim.mode, VimMode::Normal);
    }

    #[test]
    fn insert_mode_passes_chars() {
        let mut vim = VimState::new();
        vim.toggle();
        vim.mode = VimMode::Insert;
        assert_eq!(vim.process_key('a'), VimAction::InsertChar('a'));
    }

    #[test]
    fn normal_h_l_movement() {
        let mut vim = VimState::new();
        vim.toggle();
        assert_eq!(vim.process_key('h'), VimAction::MoveCursor(Direction::Left));
        assert_eq!(vim.process_key('l'), VimAction::MoveCursor(Direction::Right));
    }

    #[test]
    fn i_enters_insert_mode() {
        let mut vim = VimState::new();
        vim.toggle();
        assert_eq!(vim.process_key('i'), VimAction::SwitchMode(VimMode::Insert));
        assert_eq!(vim.mode, VimMode::Insert);
    }

    #[test]
    fn escape_returns_to_normal() {
        let mut vim = VimState::new();
        vim.toggle();
        vim.mode = VimMode::Insert;
        vim.process_escape();
        assert_eq!(vim.mode, VimMode::Normal);
    }

    #[test]
    fn disabled_mode_passes_all_chars() {
        let mut vim = VimState::new();
        assert_eq!(vim.process_key('h'), VimAction::InsertChar('h'));
    }
}
