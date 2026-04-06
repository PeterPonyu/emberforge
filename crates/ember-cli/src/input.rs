use std::borrow::Cow;
use std::io::{self, IsTerminal, Write};

use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::queue;
use crossterm::terminal::{self, Clear, ClearType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Submit(String),
    Cancel,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorMode {
    Plain,
    Insert,
    Normal,
    Visual,
    Command,
}

impl EditorMode {
    fn indicator(self, vim_enabled: bool) -> Option<&'static str> {
        if !vim_enabled {
            return None;
        }

        Some(match self {
            Self::Plain => "PLAIN",
            Self::Insert => "INSERT",
            Self::Normal => "NORMAL",
            Self::Visual => "VISUAL",
            Self::Command => "COMMAND",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct YankBuffer {
    text: String,
    linewise: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditSession {
    text: String,
    cursor: usize,
    mode: EditorMode,
    pending_operator: Option<char>,
    /// Pending count prefix (e.g., `3` in `3dw`).
    pending_count: Option<u32>,
    /// Pending find motion: ('f'|'F'|'t'|'T', awaiting_char).
    pending_find: Option<char>,
    /// Last find for ;/, repeat.
    last_find: Option<(char, char)>,
    /// Pending text object scope ('i' or 'a', awaiting type char).
    pending_scope: Option<char>,
    visual_anchor: Option<usize>,
    command_buffer: String,
    command_cursor: usize,
    history_index: Option<usize>,
    history_backup: Option<String>,
    rendered_cursor_row: usize,
    rendered_lines: usize,
}

impl EditSession {
    fn new(vim_enabled: bool) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            mode: if vim_enabled {
                EditorMode::Insert
            } else {
                EditorMode::Plain
            },
            pending_operator: None,
            pending_count: None,
            pending_find: None,
            last_find: None,
            pending_scope: None,
            visual_anchor: None,
            command_buffer: String::new(),
            command_cursor: 0,
            history_index: None,
            history_backup: None,
            rendered_cursor_row: 0,
            rendered_lines: 1,
        }
    }

    fn active_text(&self) -> &str {
        if self.mode == EditorMode::Command {
            &self.command_buffer
        } else {
            &self.text
        }
    }

    fn current_len(&self) -> usize {
        self.active_text().len()
    }

    fn has_input(&self) -> bool {
        !self.active_text().is_empty()
    }

    fn current_line(&self) -> String {
        self.active_text().to_string()
    }

    fn set_text_from_history(&mut self, entry: String) {
        self.text = entry;
        self.cursor = self.text.len();
        self.pending_operator = None;
        self.visual_anchor = None;
        if self.mode != EditorMode::Plain && self.mode != EditorMode::Insert {
            self.mode = EditorMode::Normal;
        }
    }

    fn enter_insert_mode(&mut self) {
        self.mode = EditorMode::Insert;
        self.pending_operator = None;
        self.visual_anchor = None;
    }

    fn enter_normal_mode(&mut self) {
        self.mode = EditorMode::Normal;
        self.pending_operator = None;
        self.visual_anchor = None;
    }

    fn enter_visual_mode(&mut self) {
        self.mode = EditorMode::Visual;
        self.pending_operator = None;
        self.visual_anchor = Some(self.cursor);
    }

    fn enter_command_mode(&mut self) {
        self.mode = EditorMode::Command;
        self.pending_operator = None;
        self.visual_anchor = None;
        self.command_buffer.clear();
        self.command_buffer.push(':');
        self.command_cursor = self.command_buffer.len();
    }

    fn exit_command_mode(&mut self) {
        self.command_buffer.clear();
        self.command_cursor = 0;
        self.enter_normal_mode();
    }

    fn visible_buffer(&self) -> Cow<'_, str> {
        if self.mode != EditorMode::Visual {
            return Cow::Borrowed(self.active_text());
        }

        let Some(anchor) = self.visual_anchor else {
            return Cow::Borrowed(self.active_text());
        };
        let Some((start, end)) = selection_bounds(&self.text, anchor, self.cursor) else {
            return Cow::Borrowed(self.active_text());
        };

        Cow::Owned(render_selected_text(&self.text, start, end))
    }

    fn prompt<'a>(&self, base_prompt: &'a str, vim_enabled: bool) -> Cow<'a, str> {
        match self.mode.indicator(vim_enabled) {
            Some(mode) => Cow::Owned(format!("[{mode}] {base_prompt}")),
            None => Cow::Borrowed(base_prompt),
        }
    }

    fn clear_render(&self, out: &mut impl Write) -> io::Result<()> {
        if self.rendered_cursor_row > 0 {
            queue!(out, MoveUp(to_u16(self.rendered_cursor_row)?))?;
        }
        queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        out.flush()
    }

    fn render(
        &mut self,
        out: &mut impl Write,
        base_prompt: &str,
        vim_enabled: bool,
    ) -> io::Result<()> {
        self.clear_render(out)?;

        let prompt = self.prompt(base_prompt, vim_enabled);
        let buffer = self.visible_buffer();
        write!(out, "{prompt}{buffer}")?;

        let (cursor_row, cursor_col, total_lines) = self.cursor_layout(prompt.as_ref());
        let rows_to_move_up = total_lines.saturating_sub(cursor_row + 1);
        if rows_to_move_up > 0 {
            queue!(out, MoveUp(to_u16(rows_to_move_up)?))?;
        }
        queue!(out, MoveToColumn(to_u16(cursor_col)?))?;
        out.flush()?;

        self.rendered_cursor_row = cursor_row;
        self.rendered_lines = total_lines;
        Ok(())
    }

    fn finalize_render(
        &self,
        out: &mut impl Write,
        base_prompt: &str,
        vim_enabled: bool,
    ) -> io::Result<()> {
        self.clear_render(out)?;
        let prompt = self.prompt(base_prompt, vim_enabled);
        let buffer = self.visible_buffer();
        write!(out, "{prompt}{buffer}")?;
        writeln!(out)
    }

    fn cursor_layout(&self, prompt: &str) -> (usize, usize, usize) {
        let active_text = self.active_text();
        let cursor = if self.mode == EditorMode::Command {
            self.command_cursor
        } else {
            self.cursor
        };

        let cursor_prefix = &active_text[..cursor];
        let cursor_row = cursor_prefix.bytes().filter(|byte| *byte == b'\n').count();
        let cursor_col = match cursor_prefix.rsplit_once('\n') {
            Some((_, suffix)) => suffix.chars().count(),
            None => prompt.chars().count() + cursor_prefix.chars().count(),
        };
        let total_lines = active_text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        (cursor_row, cursor_col, total_lines)
    }
}

enum KeyAction {
    Continue,
    Submit(String),
    Cancel,
    Exit,
    ToggleVim,
}

pub struct LineEditor {
    prompt: String,
    completions: Vec<String>,
    history: Vec<String>,
    yank_buffer: YankBuffer,
    vim_enabled: bool,
    completion_state: Option<CompletionState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletionState {
    prefix: String,
    matches: Vec<String>,
    next_index: usize,
}

impl LineEditor {
    #[must_use]
    pub fn new(prompt: impl Into<String>, completions: Vec<String>) -> Self {
        Self {
            prompt: prompt.into(),
            completions,
            history: Vec::new(),
            yank_buffer: YankBuffer::default(),
            vim_enabled: false,
            completion_state: None,
        }
    }

    pub fn push_history(&mut self, entry: impl Into<String>) {
        let entry = entry.into();
        if entry.trim().is_empty() {
            return;
        }

        self.history.push(entry);
    }

    pub fn read_line(&mut self) -> io::Result<ReadOutcome> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return self.read_line_fallback();
        }

        let _raw_mode = RawModeGuard::new()?;
        let mut stdout = io::stdout();
        let mut session = EditSession::new(self.vim_enabled);
        session.render(&mut stdout, &self.prompt, self.vim_enabled)?;

        loop {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            match self.handle_key_event(&mut session, key) {
                KeyAction::Continue => {
                    session.render(&mut stdout, &self.prompt, self.vim_enabled)?;
                }
                KeyAction::Submit(line) => {
                    session.finalize_render(&mut stdout, &self.prompt, self.vim_enabled)?;
                    return Ok(ReadOutcome::Submit(line));
                }
                KeyAction::Cancel => {
                    session.clear_render(&mut stdout)?;
                    writeln!(stdout)?;
                    return Ok(ReadOutcome::Cancel);
                }
                KeyAction::Exit => {
                    session.clear_render(&mut stdout)?;
                    writeln!(stdout)?;
                    return Ok(ReadOutcome::Exit);
                }
                KeyAction::ToggleVim => {
                    session.clear_render(&mut stdout)?;
                    self.vim_enabled = !self.vim_enabled;
                    writeln!(
                        stdout,
                        "Vim mode {}.",
                        if self.vim_enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    )?;
                    session = EditSession::new(self.vim_enabled);
                    session.render(&mut stdout, &self.prompt, self.vim_enabled)?;
                }
            }
        }
    }

    fn read_line_fallback(&mut self) -> io::Result<ReadOutcome> {
        loop {
            let mut stdout = io::stdout();
            write!(stdout, "{}", self.prompt)?;
            stdout.flush()?;

            let mut buffer = String::new();
            let bytes_read = io::stdin().read_line(&mut buffer)?;
            if bytes_read == 0 {
                return Ok(ReadOutcome::Exit);
            }

            while matches!(buffer.chars().last(), Some('\n' | '\r')) {
                buffer.pop();
            }

            if self.handle_submission(&buffer) == Submission::ToggleVim {
                self.vim_enabled = !self.vim_enabled;
                writeln!(
                    stdout,
                    "Vim mode {}.",
                    if self.vim_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )?;
                continue;
            }

            return Ok(ReadOutcome::Submit(buffer));
        }
    }

    fn handle_key_event(&mut self, session: &mut EditSession, key: KeyEvent) -> KeyAction {
        if key.code != KeyCode::Tab {
            self.completion_state = None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    return if session.has_input() {
                        KeyAction::Cancel
                    } else {
                        KeyAction::Exit
                    };
                }
                KeyCode::Char('j') | KeyCode::Char('J') => {
                    if session.mode != EditorMode::Normal && session.mode != EditorMode::Visual {
                        self.insert_active_text(session, "\n");
                    }
                    return KeyAction::Continue;
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if session.current_len() == 0 {
                        return KeyAction::Exit;
                    }
                    self.delete_char_under_cursor(session);
                    return KeyAction::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if session.mode != EditorMode::Normal && session.mode != EditorMode::Visual {
                    self.insert_active_text(session, "\n");
                }
                KeyAction::Continue
            }
            KeyCode::Enter => self.submit_or_toggle(session),
            KeyCode::Esc => self.handle_escape(session),
            KeyCode::Backspace => {
                self.handle_backspace(session);
                KeyAction::Continue
            }
            KeyCode::Delete => {
                self.delete_char_under_cursor(session);
                KeyAction::Continue
            }
            KeyCode::Left => {
                self.move_left(session);
                KeyAction::Continue
            }
            KeyCode::Right => {
                self.move_right(session);
                KeyAction::Continue
            }
            KeyCode::Up => {
                self.history_up(session);
                KeyAction::Continue
            }
            KeyCode::Down => {
                self.history_down(session);
                KeyAction::Continue
            }
            KeyCode::Home => {
                self.move_line_start(session);
                KeyAction::Continue
            }
            KeyCode::End => {
                self.move_line_end(session);
                KeyAction::Continue
            }
            KeyCode::Tab => {
                self.complete_slash_command(session);
                KeyAction::Continue
            }
            KeyCode::Char(ch) => {
                self.handle_char(session, ch);
                KeyAction::Continue
            }
            _ => KeyAction::Continue,
        }
    }

    fn handle_char(&mut self, session: &mut EditSession, ch: char) {
        match session.mode {
            EditorMode::Plain => self.insert_active_char(session, ch),
            EditorMode::Insert => self.insert_active_char(session, ch),
            EditorMode::Normal => self.handle_normal_char(session, ch),
            EditorMode::Visual => self.handle_visual_char(session, ch),
            EditorMode::Command => self.insert_active_char(session, ch),
        }
    }

    fn handle_normal_char(&mut self, session: &mut EditSession, ch: char) {
        // ── Pending find motion: waiting for target char ──
        if let Some(find_type) = session.pending_find.take() {
            session.last_find = Some((find_type, ch));
            self.execute_find(session, find_type, ch);
            return;
        }

        // ── Pending text object scope: waiting for type char ──
        if let Some(scope) = session.pending_scope.take() {
            if let Some(operator) = session.pending_operator.take() {
                self.execute_operator_text_object(session, operator, scope, ch);
            }
            return;
        }

        // ── Pending operator: waiting for motion/text-object/line-op ──
        if let Some(operator) = session.pending_operator {
            match ch {
                // Line operations: dd, cc, yy
                c if c == operator => {
                    session.pending_operator = None;
                    match operator {
                        'd' => self.delete_current_line(session),
                        'y' => self.yank_current_line(session),
                        'c' => {
                            self.delete_current_line(session);
                            session.enter_insert_mode();
                        }
                        _ => {}
                    }
                    return;
                }
                // Text object scope: di", ya(, ci[, etc.
                'i' | 'a' => {
                    session.pending_scope = Some(ch);
                    return;
                }
                // Find motions with operator: df}, ct;, etc.
                'f' | 'F' | 't' | 'T' => {
                    session.pending_find = Some(ch);
                    return;
                }
                // Motion after operator: dw, cw, ye, d$, etc.
                _ => {
                    session.pending_operator = None;
                    let start = session.cursor;
                    self.execute_motion(session, ch);
                    let end = session.cursor;
                    if start != end {
                        let (from, to) = if start < end { (start, end) } else { (end, start) };
                        match operator {
                            'd' => {
                                self.yank_buffer.text = session.text[from..to].to_string();
                                self.yank_buffer.linewise = false;
                                session.text.drain(from..to);
                                session.cursor = from.min(session.text.len());
                            }
                            'c' => {
                                self.yank_buffer.text = session.text[from..to].to_string();
                                self.yank_buffer.linewise = false;
                                session.text.drain(from..to);
                                session.cursor = from.min(session.text.len());
                                session.enter_insert_mode();
                            }
                            'y' => {
                                self.yank_buffer.text = session.text[from..to].to_string();
                                self.yank_buffer.linewise = false;
                                session.cursor = from; // yank moves to start
                            }
                            _ => {}
                        }
                    }
                    return;
                }
            }
        }

        // ── Count prefix (digits 1-9, or 0 only if already accumulating) ──
        if ch.is_ascii_digit() && (session.pending_count.is_some() || ch != '0') {
            let digit = ch.to_digit(10).unwrap_or(0);
            let current = session.pending_count.unwrap_or(0);
            session.pending_count = Some((current * 10 + digit).min(10_000));
            return;
        }

        // ── Normal mode commands ──
        let count = session.pending_count.take().unwrap_or(1) as usize;

        match ch {
            // Mode switches
            'i' => session.enter_insert_mode(),
            'a' => {
                session.cursor = next_boundary(&session.text, session.cursor);
                session.enter_insert_mode();
            }
            'A' => {
                session.cursor = line_end(&session.text, session.cursor);
                session.enter_insert_mode();
            }
            'I' => {
                session.cursor = first_non_blank(&session.text, session.cursor);
                session.enter_insert_mode();
            }
            'o' => {
                let end = line_end(&session.text, session.cursor);
                if end < session.text.len() {
                    session.cursor = end + 1;
                    session.text.insert(end, '\n');
                } else {
                    session.text.push('\n');
                    session.cursor = session.text.len();
                }
                session.enter_insert_mode();
            }
            'O' => {
                let start = line_start(&session.text, session.cursor);
                session.text.insert(start, '\n');
                session.cursor = start;
                session.enter_insert_mode();
            }
            'v' => session.enter_visual_mode(),
            ':' => session.enter_command_mode(),

            // Operators (wait for motion)
            'd' | 'c' | 'y' => session.pending_operator = Some(ch),

            // Find motions (wait for char)
            'f' | 'F' | 't' | 'T' => session.pending_find = Some(ch),

            // Repeat find
            ';' => {
                if let Some((find_type, target)) = session.last_find {
                    self.execute_find(session, find_type, target);
                }
            }
            ',' => {
                // Reverse find
                if let Some((find_type, target)) = session.last_find {
                    let reverse = match find_type {
                        'f' => 'F',
                        'F' => 'f',
                        't' => 'T',
                        'T' => 't',
                        _ => return,
                    };
                    self.execute_find(session, reverse, target);
                }
            }

            // x: delete char under cursor
            'x' => {
                for _ in 0..count {
                    if session.cursor < session.text.len() {
                        let end = next_boundary(&session.text, session.cursor);
                        self.yank_buffer.text = session.text[session.cursor..end].to_string();
                        self.yank_buffer.linewise = false;
                        session.text.drain(session.cursor..end);
                    }
                }
                if session.cursor >= session.text.len() && session.cursor > 0 {
                    session.cursor = previous_boundary(&session.text, session.cursor);
                }
            }
            // X: delete char before cursor
            'X' => {
                for _ in 0..count {
                    if session.cursor > 0 {
                        let start = previous_boundary(&session.text, session.cursor);
                        self.yank_buffer.text = session.text[start..session.cursor].to_string();
                        self.yank_buffer.linewise = false;
                        session.text.drain(start..session.cursor);
                        session.cursor = start;
                    }
                }
            }
            // ~: toggle case
            '~' => {
                for _ in 0..count {
                    if session.cursor < session.text.len() {
                        let end = next_boundary(&session.text, session.cursor);
                        let ch = session.text[session.cursor..end].chars().next().unwrap();
                        let toggled: String = if ch.is_uppercase() {
                            ch.to_lowercase().collect()
                        } else {
                            ch.to_uppercase().collect()
                        };
                        session.text.replace_range(session.cursor..end, &toggled);
                        session.cursor = next_boundary(&session.text, session.cursor);
                    }
                }
            }
            // r: replace single char
            'r' => {
                // Next char typed will replace — handled via pending_find trick
                session.pending_find = Some('r');
            }
            // J: join lines
            'J' => {
                let end = line_end(&session.text, session.cursor);
                if end < session.text.len() && session.text.as_bytes()[end] == b'\n' {
                    session.text.replace_range(end..end + 1, " ");
                }
            }

            // Paste
            'p' => {
                for _ in 0..count {
                    self.paste_after(session);
                }
            }
            'P' => {
                for _ in 0..count {
                    self.paste_before(session);
                }
            }

            // Undo (placeholder — real undo needs undo stack)
            'u' => {}

            // g-prefix: gg goes to start
            'g' => {
                // Simplified: next char 'g' → go to start
                session.pending_find = Some('g');
            }
            // G: go to end (or line N if count given)
            'G' => {
                session.cursor = session.text.len().saturating_sub(1).max(0);
            }

            // All motions (with count)
            _ => {
                for _ in 0..count {
                    self.execute_motion(session, ch);
                }
            }
        }
    }

    fn execute_motion(&self, session: &mut EditSession, ch: char) {
        match ch {
            'h' => self.move_left(session),
            'j' => self.move_down(session),
            'k' => self.move_up(session),
            'l' => self.move_right(session),
            'w' => session.cursor = word_forward(&session.text, session.cursor),
            'b' => session.cursor = word_backward(&session.text, session.cursor),
            'e' => session.cursor = word_end(&session.text, session.cursor),
            'W' => session.cursor = big_word_forward(&session.text, session.cursor),
            'B' => session.cursor = big_word_backward(&session.text, session.cursor),
            'E' => session.cursor = big_word_end(&session.text, session.cursor),
            '0' => self.move_line_start(session),
            '^' => session.cursor = first_non_blank(&session.text, session.cursor),
            '$' => self.move_line_end(session),
            _ => {}
        }
    }

    fn execute_find(&self, session: &mut EditSession, find_type: char, target: char) {
        match find_type {
            'f' => {
                if let Some(pos) = find_char_forward(&session.text, session.cursor, target) {
                    session.cursor = pos;
                }
            }
            'F' => {
                if let Some(pos) = find_char_backward(&session.text, session.cursor, target) {
                    session.cursor = pos;
                }
            }
            't' => {
                if let Some(pos) = till_char_forward(&session.text, session.cursor, target) {
                    session.cursor = pos;
                }
            }
            'T' => {
                if let Some(pos) = till_char_backward(&session.text, session.cursor, target) {
                    session.cursor = pos;
                }
            }
            // r: replace character
            'r' => {
                if session.cursor < session.text.len() {
                    let end = next_boundary(&session.text, session.cursor);
                    let mut buf = [0; 4];
                    session.text.replace_range(session.cursor..end, target.encode_utf8(&mut buf));
                }
            }
            // g: gg → go to start
            'g' => {
                session.cursor = 0;
            }
            _ => {}
        }
    }

    fn execute_operator_text_object(
        &mut self,
        session: &mut EditSession,
        operator: char,
        scope: char,
        obj_type: char,
    ) {
        let Some((start, end)) = resolve_text_object(&session.text, session.cursor, scope, obj_type) else {
            return;
        };

        match operator {
            'd' => {
                self.yank_buffer.text = session.text[start..end].to_string();
                self.yank_buffer.linewise = false;
                session.text.drain(start..end);
                session.cursor = start.min(session.text.len());
            }
            'c' => {
                self.yank_buffer.text = session.text[start..end].to_string();
                self.yank_buffer.linewise = false;
                session.text.drain(start..end);
                session.cursor = start.min(session.text.len());
                session.enter_insert_mode();
            }
            'y' => {
                self.yank_buffer.text = session.text[start..end].to_string();
                self.yank_buffer.linewise = false;
                session.cursor = start;
            }
            _ => {}
        }
    }

    fn paste_before(&mut self, session: &mut EditSession) {
        if self.yank_buffer.text.is_empty() {
            return;
        }

        if self.yank_buffer.linewise {
            let start = line_start(&session.text, session.cursor);
            let mut insertion = self.yank_buffer.text.clone();
            if !insertion.ends_with('\n') {
                insertion.push('\n');
            }
            session.text.insert_str(start, &insertion);
            session.cursor = start;
        } else {
            session.text.insert_str(session.cursor, &self.yank_buffer.text);
        }
    }

    fn handle_visual_char(&mut self, session: &mut EditSession, ch: char) {
        match ch {
            // Movement
            'h' => self.move_left(session),
            'j' => self.move_down(session),
            'k' => self.move_up(session),
            'l' => self.move_right(session),
            'w' => session.cursor = word_forward(&session.text, session.cursor),
            'b' => session.cursor = word_backward(&session.text, session.cursor),
            'e' => session.cursor = word_end(&session.text, session.cursor),
            'W' => session.cursor = big_word_forward(&session.text, session.cursor),
            'B' => session.cursor = big_word_backward(&session.text, session.cursor),
            '0' => self.move_line_start(session),
            '$' => self.move_line_end(session),

            // Operations on selection
            'd' | 'x' => {
                if let Some(anchor) = session.visual_anchor {
                    let (start, end) = if anchor <= session.cursor {
                        (anchor, next_boundary(&session.text, session.cursor))
                    } else {
                        (session.cursor, next_boundary(&session.text, anchor))
                    };
                    self.yank_buffer.text = session.text[start..end].to_string();
                    self.yank_buffer.linewise = false;
                    session.text.drain(start..end);
                    session.cursor = start.min(session.text.len());
                }
                session.enter_normal_mode();
            }
            'c' => {
                if let Some(anchor) = session.visual_anchor {
                    let (start, end) = if anchor <= session.cursor {
                        (anchor, next_boundary(&session.text, session.cursor))
                    } else {
                        (session.cursor, next_boundary(&session.text, anchor))
                    };
                    self.yank_buffer.text = session.text[start..end].to_string();
                    self.yank_buffer.linewise = false;
                    session.text.drain(start..end);
                    session.cursor = start.min(session.text.len());
                }
                session.enter_insert_mode();
            }
            'y' => {
                if let Some(anchor) = session.visual_anchor {
                    let (start, end) = if anchor <= session.cursor {
                        (anchor, next_boundary(&session.text, session.cursor))
                    } else {
                        (session.cursor, next_boundary(&session.text, anchor))
                    };
                    self.yank_buffer.text = session.text[start..end].to_string();
                    self.yank_buffer.linewise = false;
                    session.cursor = start;
                }
                session.enter_normal_mode();
            }
            '~' => {
                if let Some(anchor) = session.visual_anchor {
                    let (start, end) = if anchor <= session.cursor {
                        (anchor, next_boundary(&session.text, session.cursor))
                    } else {
                        (session.cursor, next_boundary(&session.text, anchor))
                    };
                    let toggled: String = session.text[start..end]
                        .chars()
                        .map(|c| {
                            if c.is_uppercase() {
                                c.to_lowercase().next().unwrap_or(c)
                            } else {
                                c.to_uppercase().next().unwrap_or(c)
                            }
                        })
                        .collect();
                    session.text.replace_range(start..end, &toggled);
                }
                session.enter_normal_mode();
            }

            // Exit visual
            'v' => session.enter_normal_mode(),
            _ => {}
        }
    }

    fn handle_escape(&mut self, session: &mut EditSession) -> KeyAction {
        match session.mode {
            EditorMode::Plain => KeyAction::Continue,
            EditorMode::Insert => {
                if session.cursor > 0 {
                    session.cursor = previous_boundary(&session.text, session.cursor);
                }
                session.enter_normal_mode();
                KeyAction::Continue
            }
            EditorMode::Normal => KeyAction::Continue,
            EditorMode::Visual => {
                session.enter_normal_mode();
                KeyAction::Continue
            }
            EditorMode::Command => {
                session.exit_command_mode();
                KeyAction::Continue
            }
        }
    }

    fn handle_backspace(&mut self, session: &mut EditSession) {
        match session.mode {
            EditorMode::Normal | EditorMode::Visual => self.move_left(session),
            EditorMode::Command => {
                if session.command_cursor <= 1 {
                    session.exit_command_mode();
                } else {
                    remove_previous_char(&mut session.command_buffer, &mut session.command_cursor);
                }
            }
            EditorMode::Plain | EditorMode::Insert => {
                remove_previous_char(&mut session.text, &mut session.cursor);
            }
        }
    }

    fn submit_or_toggle(&mut self, session: &EditSession) -> KeyAction {
        let line = session.current_line();
        match self.handle_submission(&line) {
            Submission::Submit => KeyAction::Submit(line),
            Submission::ToggleVim => KeyAction::ToggleVim,
        }
    }

    fn handle_submission(&mut self, line: &str) -> Submission {
        if line.trim() == "/vim" {
            Submission::ToggleVim
        } else {
            Submission::Submit
        }
    }

    fn insert_active_char(&mut self, session: &mut EditSession, ch: char) {
        let mut buffer = [0; 4];
        self.insert_active_text(session, ch.encode_utf8(&mut buffer));
    }

    fn insert_active_text(&mut self, session: &mut EditSession, text: &str) {
        if session.mode == EditorMode::Command {
            session
                .command_buffer
                .insert_str(session.command_cursor, text);
            session.command_cursor += text.len();
        } else {
            session.text.insert_str(session.cursor, text);
            session.cursor += text.len();
        }
    }

    fn move_left(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor =
                previous_command_boundary(&session.command_buffer, session.command_cursor);
        } else {
            session.cursor = previous_boundary(&session.text, session.cursor);
        }
    }

    fn move_right(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor = next_boundary(&session.command_buffer, session.command_cursor);
        } else {
            session.cursor = next_boundary(&session.text, session.cursor);
        }
    }

    fn move_line_start(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor = 1;
        } else {
            session.cursor = line_start(&session.text, session.cursor);
        }
    }

    fn move_line_end(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor = session.command_buffer.len();
        } else {
            session.cursor = line_end(&session.text, session.cursor);
        }
    }

    fn move_up(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            return;
        }
        session.cursor = move_vertical(&session.text, session.cursor, -1);
    }

    fn move_down(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            return;
        }
        session.cursor = move_vertical(&session.text, session.cursor, 1);
    }

    fn delete_char_under_cursor(&self, session: &mut EditSession) {
        match session.mode {
            EditorMode::Command => {
                if session.command_cursor < session.command_buffer.len() {
                    let end = next_boundary(&session.command_buffer, session.command_cursor);
                    session.command_buffer.drain(session.command_cursor..end);
                }
            }
            _ => {
                if session.cursor < session.text.len() {
                    let end = next_boundary(&session.text, session.cursor);
                    session.text.drain(session.cursor..end);
                }
            }
        }
    }

    fn delete_current_line(&mut self, session: &mut EditSession) {
        let (line_start_idx, line_end_idx, delete_start_idx) =
            current_line_delete_range(&session.text, session.cursor);
        self.yank_buffer.text = session.text[line_start_idx..line_end_idx].to_string();
        self.yank_buffer.linewise = true;
        session.text.drain(delete_start_idx..line_end_idx);
        session.cursor = delete_start_idx.min(session.text.len());
    }

    fn yank_current_line(&mut self, session: &mut EditSession) {
        let (line_start_idx, line_end_idx, _) =
            current_line_delete_range(&session.text, session.cursor);
        self.yank_buffer.text = session.text[line_start_idx..line_end_idx].to_string();
        self.yank_buffer.linewise = true;
    }

    fn paste_after(&mut self, session: &mut EditSession) {
        if self.yank_buffer.text.is_empty() {
            return;
        }

        if self.yank_buffer.linewise {
            let line_end_idx = line_end(&session.text, session.cursor);
            let insert_at = if line_end_idx < session.text.len() {
                line_end_idx + 1
            } else {
                session.text.len()
            };
            let mut insertion = self.yank_buffer.text.clone();
            if insert_at == session.text.len()
                && !session.text.is_empty()
                && !session.text.ends_with('\n')
            {
                insertion.insert(0, '\n');
            }
            if insert_at < session.text.len() && !insertion.ends_with('\n') {
                insertion.push('\n');
            }
            session.text.insert_str(insert_at, &insertion);
            session.cursor = if insertion.starts_with('\n') {
                insert_at + 1
            } else {
                insert_at
            };
            return;
        }

        let insert_at = next_boundary(&session.text, session.cursor);
        session.text.insert_str(insert_at, &self.yank_buffer.text);
        session.cursor = insert_at + self.yank_buffer.text.len();
    }

    fn complete_slash_command(&mut self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            self.completion_state = None;
            return;
        }
        if let Some(state) = self
            .completion_state
            .as_mut()
            .filter(|_| session.cursor == session.text.len())
            .filter(|state| {
                state
                    .matches
                    .iter()
                    .any(|candidate| candidate == &session.text)
            })
        {
            let candidate = state.matches[state.next_index % state.matches.len()].clone();
            state.next_index += 1;
            session.text.replace_range(..session.cursor, &candidate);
            session.cursor = candidate.len();
            return;
        }
        let Some(prefix) = slash_command_prefix(&session.text, session.cursor) else {
            self.completion_state = None;
            return;
        };
        let matches = self
            .completions
            .iter()
            .filter(|candidate| candidate.starts_with(prefix) && candidate.as_str() != prefix)
            .cloned()
            .collect::<Vec<_>>();
        if matches.is_empty() {
            self.completion_state = None;
            return;
        }

        let candidate = if let Some(state) = self
            .completion_state
            .as_mut()
            .filter(|state| state.prefix == prefix && state.matches == matches)
        {
            let index = state.next_index % state.matches.len();
            state.next_index += 1;
            state.matches[index].clone()
        } else {
            let candidate = matches[0].clone();
            self.completion_state = Some(CompletionState {
                prefix: prefix.to_string(),
                matches,
                next_index: 1,
            });
            candidate
        };

        session.text.replace_range(..session.cursor, &candidate);
        session.cursor = candidate.len();
    }

    fn history_up(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command || self.history.is_empty() {
            return;
        }

        let next_index = match session.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                session.history_backup = Some(session.text.clone());
                self.history.len() - 1
            }
        };

        session.history_index = Some(next_index);
        session.set_text_from_history(self.history[next_index].clone());
    }

    fn history_down(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            return;
        }

        let Some(index) = session.history_index else {
            return;
        };

        if index + 1 < self.history.len() {
            let next_index = index + 1;
            session.history_index = Some(next_index);
            session.set_text_from_history(self.history[next_index].clone());
            return;
        }

        session.history_index = None;
        let restored = session.history_backup.take().unwrap_or_default();
        session.set_text_from_history(restored);
        if self.vim_enabled {
            session.enter_insert_mode();
        } else {
            session.mode = EditorMode::Plain;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Submission {
    Submit,
    ToggleVim,
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode().map_err(io::Error::other)?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }

    text[..cursor]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn previous_command_boundary(text: &str, cursor: usize) -> usize {
    previous_boundary(text, cursor).max(1)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }

    text[cursor..]
        .chars()
        .next()
        .map_or(text.len(), |ch| cursor + ch.len_utf8())
}

fn remove_previous_char(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }

    let start = previous_boundary(text, *cursor);
    text.drain(start..*cursor);
    *cursor = start;
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map_or(text.len(), |index| cursor + index)
}

fn move_vertical(text: &str, cursor: usize, delta: isize) -> usize {
    let starts = line_starts(text);
    let current_row = text[..cursor].bytes().filter(|byte| *byte == b'\n').count();
    let current_start = starts[current_row];
    let current_col = text[current_start..cursor].chars().count();

    let max_row = starts.len().saturating_sub(1) as isize;
    let target_row = (current_row as isize + delta).clamp(0, max_row) as usize;
    if target_row == current_row {
        return cursor;
    }

    let target_start = starts[target_row];
    let target_end = if target_row + 1 < starts.len() {
        starts[target_row + 1] - 1
    } else {
        text.len()
    };
    byte_index_for_char_column(&text[target_start..target_end], current_col) + target_start
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn byte_index_for_char_column(text: &str, column: usize) -> usize {
    let mut current = 0;
    for (index, _) in text.char_indices() {
        if current == column {
            return index;
        }
        current += 1;
    }
    text.len()
}

fn current_line_delete_range(text: &str, cursor: usize) -> (usize, usize, usize) {
    let line_start_idx = line_start(text, cursor);
    let line_end_core = line_end(text, cursor);
    let line_end_idx = if line_end_core < text.len() {
        line_end_core + 1
    } else {
        line_end_core
    };
    let delete_start_idx = if line_end_idx == text.len() && line_start_idx > 0 {
        line_start_idx - 1
    } else {
        line_start_idx
    };
    (line_start_idx, line_end_idx, delete_start_idx)
}

fn selection_bounds(text: &str, anchor: usize, cursor: usize) -> Option<(usize, usize)> {
    if text.is_empty() {
        return None;
    }

    if cursor >= anchor {
        let end = next_boundary(text, cursor);
        Some((anchor.min(text.len()), end.min(text.len())))
    } else {
        let end = next_boundary(text, anchor);
        Some((cursor.min(text.len()), end.min(text.len())))
    }
}

fn render_selected_text(text: &str, start: usize, end: usize) -> String {
    let mut rendered = String::new();
    let mut in_selection = false;

    for (index, ch) in text.char_indices() {
        if !in_selection && index == start {
            rendered.push_str("\x1b[7m");
            in_selection = true;
        }
        if in_selection && index == end {
            rendered.push_str("\x1b[0m");
            in_selection = false;
        }
        rendered.push(ch);
    }

    if in_selection {
        rendered.push_str("\x1b[0m");
    }

    rendered
}

fn slash_command_prefix(line: &str, pos: usize) -> Option<&str> {
    if pos != line.len() {
        return None;
    }

    let prefix = &line[..pos];
    if prefix.contains(char::is_whitespace) || !prefix.starts_with('/') {
        return None;
    }

    Some(prefix)
}

fn to_u16(value: usize) -> io::Result<u16> {
    u16::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal position overflowed u16",
        )
    })
}

/// Move to the first non-blank character on the current line (^ motion).
fn first_non_blank(text: &str, cursor: usize) -> usize {
    let start = line_start(text, cursor);
    let end = line_end(text, cursor);
    for (i, ch) in text[start..end].char_indices() {
        if !ch.is_whitespace() {
            return start + i;
        }
    }
    start
}

// ── Vim word motions ─────────────────────────────────────────────────

/// Character classification for vim word boundaries.
#[derive(PartialEq)]
enum CharClass {
    Whitespace,
    Word,       // alphanumeric + underscore
    Punctuation,
}

fn char_class(ch: char) -> CharClass {
    if ch.is_whitespace() {
        CharClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Move forward to the start of the next vim word (w).
fn word_forward(text: &str, cursor: usize) -> usize {
    let bytes = text.as_bytes();
    let len = text.len();
    if cursor >= len {
        return len;
    }
    let mut pos = cursor;

    // Get class of char at cursor
    let start_ch = text[pos..].chars().next().unwrap();
    let start_class = char_class(start_ch);

    // Skip current class
    while pos < len {
        let ch = text[pos..].chars().next().unwrap();
        if char_class(ch) != start_class {
            break;
        }
        pos += ch.len_utf8();
    }

    // Skip whitespace
    while pos < len {
        let ch = text[pos..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }

    pos
}

/// Move backward to the start of the previous vim word (b).
fn word_backward(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut pos = cursor;

    // Skip whitespace backward
    while pos > 0 {
        let prev = prev_char(text, pos);
        if !prev.is_whitespace() {
            break;
        }
        pos -= prev.len_utf8();
    }

    if pos == 0 {
        return 0;
    }

    // Get class of char before pos
    let target_class = char_class(prev_char(text, pos));

    // Skip same class backward
    while pos > 0 {
        let prev = prev_char(text, pos);
        if char_class(prev) != target_class {
            break;
        }
        pos -= prev.len_utf8();
    }

    pos
}

/// Move to the end of the current/next vim word (e).
fn word_end(text: &str, cursor: usize) -> usize {
    let len = text.len();
    if cursor >= len {
        return len;
    }
    let mut pos = next_boundary(text, cursor); // Move past current char

    // Skip whitespace
    while pos < len {
        let ch = text[pos..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }

    if pos >= len {
        return len.saturating_sub(1).max(cursor);
    }

    let target_class = char_class(text[pos..].chars().next().unwrap());

    // Move to end of word
    while pos < len {
        let next = next_boundary(text, pos);
        if next >= len {
            return pos;
        }
        let ch = text[next..].chars().next().unwrap_or(' ');
        if char_class(ch) != target_class {
            return pos;
        }
        pos = next;
    }

    pos
}

/// Move forward to the start of the next WORD (W) — whitespace-delimited.
fn big_word_forward(text: &str, cursor: usize) -> usize {
    let len = text.len();
    let mut pos = cursor;

    // Skip non-whitespace
    while pos < len {
        let ch = text[pos..].chars().next().unwrap();
        if ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }

    // Skip whitespace
    while pos < len {
        let ch = text[pos..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }

    pos
}

/// Move backward to the start of the previous WORD (B).
fn big_word_backward(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut pos = cursor;

    // Skip whitespace backward
    while pos > 0 && prev_char(text, pos).is_whitespace() {
        pos -= prev_char(text, pos).len_utf8();
    }

    // Skip non-whitespace backward
    while pos > 0 && !prev_char(text, pos).is_whitespace() {
        pos -= prev_char(text, pos).len_utf8();
    }

    pos
}

/// Move to the end of the current/next WORD (E).
fn big_word_end(text: &str, cursor: usize) -> usize {
    let len = text.len();
    let mut pos = next_boundary(text, cursor);

    // Skip whitespace
    while pos < len {
        let ch = text[pos..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }

    // Skip non-whitespace to find end
    while pos < len {
        let next = next_boundary(text, pos);
        if next >= len {
            return pos;
        }
        let ch = text[next..].chars().next().unwrap_or(' ');
        if ch.is_whitespace() {
            return pos;
        }
        pos = next;
    }

    pos.min(len.saturating_sub(1))
}

/// Get the character immediately before `pos`.
fn prev_char(text: &str, pos: usize) -> char {
    text[..pos].chars().next_back().unwrap_or(' ')
}

// ── Find motions (f/F/t/T) ──────────────────────────────────────────

/// Find character forward (f). Returns byte offset of the character.
fn find_char_forward(text: &str, cursor: usize, target: char) -> Option<usize> {
    let after = next_boundary(text, cursor);
    for (i, ch) in text[after..].char_indices() {
        if ch == target {
            return Some(after + i);
        }
    }
    None
}

/// Find character backward (F). Returns byte offset of the character.
fn find_char_backward(text: &str, cursor: usize, target: char) -> Option<usize> {
    for (i, ch) in text[..cursor].char_indices().rev() {
        if ch == target {
            return Some(i);
        }
    }
    None
}

/// Till character forward (t). Returns byte offset just before the character.
fn till_char_forward(text: &str, cursor: usize, target: char) -> Option<usize> {
    find_char_forward(text, cursor, target).map(|pos| previous_boundary(text, pos))
}

/// Till character backward (T). Returns byte offset just after the character.
fn till_char_backward(text: &str, cursor: usize, target: char) -> Option<usize> {
    find_char_backward(text, cursor, target).map(|pos| next_boundary(text, pos))
}

// ── Text objects ─────────────────────────────────────────────────────

/// Find the range for "inner word" (iw) text object.
fn text_object_inner_word(text: &str, cursor: usize) -> Option<(usize, usize)> {
    if cursor >= text.len() {
        return None;
    }
    let ch = text[cursor..].chars().next()?;
    let target_class = char_class(ch);

    // Expand backward
    let mut start = cursor;
    while start > 0 {
        let prev = prev_char(text, start);
        if char_class(prev) != target_class {
            break;
        }
        start -= prev.len_utf8();
    }

    // Expand forward
    let mut end = cursor;
    while end < text.len() {
        let c = text[end..].chars().next()?;
        if char_class(c) != target_class {
            break;
        }
        end += c.len_utf8();
    }

    Some((start, end))
}

/// Find the range for "around word" (aw) — includes trailing whitespace.
fn text_object_around_word(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let (start, mut end) = text_object_inner_word(text, cursor)?;

    // Include trailing whitespace
    while end < text.len() {
        let ch = text[end..].chars().next()?;
        if !ch.is_whitespace() {
            break;
        }
        end += ch.len_utf8();
    }

    // If no trailing whitespace was consumed, try leading whitespace
    if end == text_object_inner_word(text, cursor)?.1 {
        let mut new_start = start;
        while new_start > 0 && prev_char(text, new_start).is_whitespace() {
            new_start -= prev_char(text, new_start).len_utf8();
        }
        return Some((new_start, end));
    }

    Some((start, end))
}

/// Find matching bracket pair around cursor.
fn text_object_bracket(text: &str, cursor: usize, open: char, close: char, inner: bool) -> Option<(usize, usize)> {
    // Search backward for opening bracket
    let mut depth = 0i32;
    let mut open_pos = None;
    for (i, ch) in text[..=cursor.min(text.len().saturating_sub(1))].char_indices().rev() {
        if ch == close {
            depth += 1;
        } else if ch == open {
            if depth == 0 {
                open_pos = Some(i);
                break;
            }
            depth -= 1;
        }
    }

    let open_pos = open_pos?;

    // Search forward for closing bracket
    depth = 0;
    let mut close_pos = None;
    for (i, ch) in text[open_pos..].char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                close_pos = Some(open_pos + i);
                break;
            }
        }
    }

    let close_pos = close_pos?;

    if inner {
        Some((open_pos + open.len_utf8(), close_pos))
    } else {
        Some((open_pos, close_pos + close.len_utf8()))
    }
}

/// Find matching quote pair around cursor.
fn text_object_quote(text: &str, cursor: usize, quote: char, inner: bool) -> Option<(usize, usize)> {
    // Find the nearest quote pair containing cursor
    let mut positions = Vec::new();
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            positions.push(i);
        }
    }

    // Find pair that contains cursor
    for pair in positions.chunks_exact(2) {
        let (start, end) = (pair[0], pair[1]);
        if cursor >= start && cursor <= end {
            return if inner {
                Some((start + quote.len_utf8(), end))
            } else {
                Some((start, end + quote.len_utf8()))
            };
        }
    }

    None
}

/// Resolve a text object from the scope (i/a) and type character.
fn resolve_text_object(text: &str, cursor: usize, scope: char, obj_type: char) -> Option<(usize, usize)> {
    let inner = scope == 'i';
    match obj_type {
        'w' => {
            if inner {
                text_object_inner_word(text, cursor)
            } else {
                text_object_around_word(text, cursor)
            }
        }
        '(' | ')' | 'b' => text_object_bracket(text, cursor, '(', ')', inner),
        '[' | ']' => text_object_bracket(text, cursor, '[', ']', inner),
        '{' | '}' | 'B' => text_object_bracket(text, cursor, '{', '}', inner),
        '<' | '>' => text_object_bracket(text, cursor, '<', '>', inner),
        '"' => text_object_quote(text, cursor, '"', inner),
        '\'' => text_object_quote(text, cursor, '\'', inner),
        '`' => text_object_quote(text, cursor, '`', inner),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        selection_bounds, slash_command_prefix, EditSession, EditorMode, KeyAction, LineEditor,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn extracts_only_terminal_slash_command_prefixes() {
        // given
        let complete_prefix = slash_command_prefix("/he", 3);
        let whitespace_prefix = slash_command_prefix("/help me", 5);
        let plain_text_prefix = slash_command_prefix("hello", 5);
        let mid_buffer_prefix = slash_command_prefix("/help", 2);

        // when
        let result = (
            complete_prefix,
            whitespace_prefix,
            plain_text_prefix,
            mid_buffer_prefix,
        );

        // then
        assert_eq!(result, (Some("/he"), None, None, None));
    }

    #[test]
    fn toggle_submission_flips_vim_mode() {
        // given
        let mut editor = LineEditor::new("> ", vec!["/help".to_string(), "/vim".to_string()]);

        // when
        let first = editor.handle_submission("/vim");
        editor.vim_enabled = true;
        let second = editor.handle_submission("/vim");

        // then
        assert!(matches!(first, super::Submission::ToggleVim));
        assert!(matches!(second, super::Submission::ToggleVim));
    }

    #[test]
    fn normal_mode_supports_motion_and_insert_transition() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "hello".to_string();
        session.cursor = session.text.len();
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'h');
        editor.handle_char(&mut session, 'i');
        editor.handle_char(&mut session, '!');

        // then
        assert_eq!(session.mode, EditorMode::Insert);
        assert_eq!(session.text, "hel!lo");
    }

    #[test]
    fn yy_and_p_paste_yanked_line_after_current_line() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "alpha\nbeta\ngamma".to_string();
        session.cursor = 0;
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'y');
        editor.handle_char(&mut session, 'y');
        editor.handle_char(&mut session, 'p');

        // then
        assert_eq!(session.text, "alpha\nalpha\nbeta\ngamma");
    }

    #[test]
    fn dd_and_p_paste_deleted_line_after_current_line() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "alpha\nbeta\ngamma".to_string();
        session.cursor = 0;
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'j');
        editor.handle_char(&mut session, 'd');
        editor.handle_char(&mut session, 'd');
        editor.handle_char(&mut session, 'p');

        // then
        assert_eq!(session.text, "alpha\ngamma\nbeta\n");
    }

    #[test]
    fn visual_mode_tracks_selection_with_motions() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "alpha\nbeta".to_string();
        session.cursor = 0;
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'v');
        editor.handle_char(&mut session, 'j');
        editor.handle_char(&mut session, 'l');

        // then
        assert_eq!(session.mode, EditorMode::Visual);
        assert_eq!(
            selection_bounds(
                &session.text,
                session.visual_anchor.unwrap_or(0),
                session.cursor
            ),
            Some((0, 8))
        );
    }

    #[test]
    fn command_mode_submits_colon_prefixed_input() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "draft".to_string();
        session.cursor = session.text.len();
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, ':');
        editor.handle_char(&mut session, 'q');
        editor.handle_char(&mut session, '!');
        let action = editor.submit_or_toggle(&session);

        // then
        assert_eq!(session.mode, EditorMode::Command);
        assert_eq!(session.command_buffer, ":q!");
        assert!(matches!(action, KeyAction::Submit(line) if line == ":q!"));
    }

    #[test]
    fn push_history_ignores_blank_entries() {
        // given
        let mut editor = LineEditor::new("> ", vec!["/help".to_string()]);

        // when
        editor.push_history("   ");
        editor.push_history("/help");

        // then
        assert_eq!(editor.history, vec!["/help".to_string()]);
    }

    #[test]
    fn tab_completes_matching_slash_commands() {
        // given
        let mut editor = LineEditor::new("> ", vec!["/help".to_string(), "/hello".to_string()]);
        let mut session = EditSession::new(false);
        session.text = "/he".to_string();
        session.cursor = session.text.len();

        // when
        editor.complete_slash_command(&mut session);

        // then
        assert_eq!(session.text, "/help");
        assert_eq!(session.cursor, 5);
    }

    #[test]
    fn tab_cycles_between_matching_slash_commands() {
        // given
        let mut editor = LineEditor::new(
            "> ",
            vec!["/permissions".to_string(), "/plugin".to_string()],
        );
        let mut session = EditSession::new(false);
        session.text = "/p".to_string();
        session.cursor = session.text.len();

        // when
        editor.complete_slash_command(&mut session);
        let first = session.text.clone();
        session.cursor = session.text.len();
        editor.complete_slash_command(&mut session);
        let second = session.text.clone();

        // then
        assert_eq!(first, "/permissions");
        assert_eq!(second, "/plugin");
    }

    #[test]
    fn ctrl_c_cancels_when_input_exists() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        let mut session = EditSession::new(false);
        session.text = "draft".to_string();
        session.cursor = session.text.len();

        // when
        let action = editor.handle_key_event(
            &mut session,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );

        // then
        assert!(matches!(action, KeyAction::Cancel));
    }

    // ── Word motion tests ─────────────────────────────────────────────

    #[test]
    fn word_forward_skips_word_boundary() {
        assert_eq!(super::word_forward("hello world", 0), 6);
        assert_eq!(super::word_forward("hello world", 6), 11);
        assert_eq!(super::word_forward("foo.bar baz", 0), 3); // stops at punctuation boundary
    }

    #[test]
    fn word_backward_skips_word_boundary() {
        assert_eq!(super::word_backward("hello world", 11), 6);
        assert_eq!(super::word_backward("hello world", 6), 0);
        assert_eq!(super::word_backward("hello world", 0), 0);
    }

    #[test]
    fn word_end_stops_at_end_of_word() {
        assert_eq!(super::word_end("hello world", 0), 4);
        assert_eq!(super::word_end("hello world", 4), 10);
    }

    #[test]
    fn big_word_forward_whitespace_delimited() {
        assert_eq!(super::big_word_forward("foo.bar baz", 0), 8); // skips past "foo.bar"
        assert_eq!(super::big_word_forward("hello world", 0), 6);
    }

    #[test]
    fn big_word_backward_whitespace_delimited() {
        assert_eq!(super::big_word_backward("foo.bar baz", 11), 8);
        assert_eq!(super::big_word_backward("foo.bar baz", 8), 0);
    }

    // ── Find motion tests ─────────────────────────────────────────────

    #[test]
    fn find_char_forward_locates_char() {
        assert_eq!(super::find_char_forward("hello world", 0, 'o'), Some(4));
        assert_eq!(super::find_char_forward("hello world", 5, 'o'), Some(7));
        assert_eq!(super::find_char_forward("hello world", 0, 'z'), None);
    }

    #[test]
    fn find_char_backward_locates_char() {
        assert_eq!(super::find_char_backward("hello world", 11, 'o'), Some(7));
        assert_eq!(super::find_char_backward("hello world", 7, 'o'), Some(4));
        assert_eq!(super::find_char_backward("hello world", 0, 'o'), None);
    }

    #[test]
    fn till_char_forward_stops_before() {
        let pos = super::till_char_forward("hello world", 0, 'o');
        assert!(pos.is_some());
        assert!(pos.unwrap() < 4); // before the 'o' at pos 4
    }

    // ── Text object tests ─────────────────────────────────────────────

    #[test]
    fn text_object_inner_word_selects_word() {
        let (start, end) = super::text_object_inner_word("hello world", 2).unwrap();
        assert_eq!(&"hello world"[start..end], "hello");
    }

    #[test]
    fn text_object_around_word_includes_whitespace() {
        let (start, end) = super::text_object_around_word("hello world", 2).unwrap();
        assert_eq!(&"hello world"[start..end], "hello ");
    }

    #[test]
    fn text_object_bracket_inner() {
        let (start, end) = super::text_object_bracket("fn(a, b)", 4, '(', ')', true).unwrap();
        assert_eq!(&"fn(a, b)"[start..end], "a, b");
    }

    #[test]
    fn text_object_bracket_around() {
        let (start, end) = super::text_object_bracket("fn(a, b)", 4, '(', ')', false).unwrap();
        assert_eq!(&"fn(a, b)"[start..end], "(a, b)");
    }

    #[test]
    fn text_object_quote_inner() {
        let text = r#"say "hello world" now"#;
        let (start, end) = super::text_object_quote(text, 6, '"', true).unwrap();
        assert_eq!(&text[start..end], "hello world");
    }

    #[test]
    fn text_object_quote_around() {
        let text = r#"say "hello world" now"#;
        let (start, end) = super::text_object_quote(text, 6, '"', false).unwrap();
        assert_eq!(&text[start..end], "\"hello world\"");
    }

    // ── Operator + motion tests ───────────────────────────────────────

    #[test]
    fn dw_deletes_word() {
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "hello world".to_string();
        session.cursor = 0;
        session.enter_normal_mode();

        // Type: dw
        editor.handle_char(&mut session, 'd');
        editor.handle_char(&mut session, 'w');

        assert_eq!(session.text, "world");
        assert_eq!(editor.yank_buffer.text, "hello ");
    }

    #[test]
    fn ciw_changes_inner_word() {
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "hello world".to_string();
        session.cursor = 2; // in "hello"
        session.enter_normal_mode();

        // Type: ciw
        editor.handle_char(&mut session, 'c');
        editor.handle_char(&mut session, 'i');
        editor.handle_char(&mut session, 'w');

        assert_eq!(session.text, " world");
        assert_eq!(session.mode, EditorMode::Insert);
        assert_eq!(editor.yank_buffer.text, "hello");
    }

    #[test]
    fn dd_deletes_line_and_yanks() {
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "line1\nline2\nline3".to_string();
        session.cursor = 6; // start of line2
        session.enter_normal_mode();

        editor.handle_char(&mut session, 'd');
        editor.handle_char(&mut session, 'd');

        assert!(!session.text.contains("line2"));
        assert!(editor.yank_buffer.linewise);
    }

    #[test]
    fn count_prefix_repeats_motion() {
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "a b c d e".to_string();
        session.cursor = 0;
        session.enter_normal_mode();

        // Type: 3w (move forward 3 words)
        editor.handle_char(&mut session, '3');
        editor.handle_char(&mut session, 'w');

        assert_eq!(session.cursor, 6); // at 'd'
    }

    #[test]
    fn x_deletes_char_under_cursor() {
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "hello".to_string();
        session.cursor = 0;
        session.enter_normal_mode();

        editor.handle_char(&mut session, 'x');

        assert_eq!(session.text, "ello");
    }

    #[test]
    fn tilde_toggles_case() {
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "Hello".to_string();
        session.cursor = 0;
        session.enter_normal_mode();

        editor.handle_char(&mut session, '~');

        assert_eq!(&session.text[..1], "h"); // 'H' → 'h'
    }

    #[test]
    fn first_non_blank_skips_whitespace() {
        assert_eq!(super::first_non_blank("  hello", 0), 2);
        assert_eq!(super::first_non_blank("hello", 0), 0);
        assert_eq!(super::first_non_blank("\thello", 0), 1);
    }

    #[test]
    fn resolve_text_object_dispatches_correctly() {
        // iw
        let r = super::resolve_text_object("hello world", 2, 'i', 'w');
        assert_eq!(r, Some((0, 5)));

        // i(
        let r = super::resolve_text_object("fn(x, y)", 4, 'i', '(');
        assert_eq!(r, Some((3, 7)));

        // a"
        let r = super::resolve_text_object(r#"say "hi" bye"#, 5, 'a', '"');
        assert!(r.is_some());
        let (s, e) = r.unwrap();
        assert!(e > s);
    }
}
