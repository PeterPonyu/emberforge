use std::fmt::Write as FmtWrite;
use std::io::{self, Write};

use crossterm::cursor::{MoveToColumn, RestorePosition, SavePosition};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor, Stylize};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, queue};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::{as_24_bit_terminal_escaped, LinesWithEndings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorTheme {
    heading: Color,
    emphasis: Color,
    strong: Color,
    inline_code: Color,
    link: Color,
    quote: Color,
    quote_bar: Color,
    table_border: Color,
    code_block_border: Color,
    spinner_active: Color,
    spinner_done: Color,
    spinner_failed: Color,
}

impl Default for ColorTheme {
    fn default() -> Self {
        Self {
            heading: Color::Cyan,
            emphasis: Color::Magenta,
            strong: Color::Yellow,
            inline_code: Color::Green,
            link: Color::Blue,
            quote: Color::DarkGrey,
            quote_bar: Color::DarkCyan,
            table_border: Color::DarkCyan,
            code_block_border: Color::DarkGrey,
            spinner_active: Color::Blue,
            spinner_done: Color::Green,
            spinner_failed: Color::Red,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Spinner {
    frame_index: usize,
}

#[allow(dead_code)]
impl Spinner {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(
        &mut self,
        label: &str,
        theme: &ColorTheme,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let frame = Self::FRAMES[self.frame_index % Self::FRAMES.len()];
        self.frame_index += 1;
        queue!(
            out,
            SavePosition,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_active),
            Print(format!("{frame} {label}")),
            ResetColor,
            RestorePosition
        )?;
        out.flush()
    }

    pub fn finish(
        &mut self,
        label: &str,
        theme: &ColorTheme,
        out: &mut impl Write,
    ) -> io::Result<()> {
        self.frame_index = 0;
        execute!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_done),
            Print(format!("✔ {label}\n")),
            ResetColor
        )?;
        out.flush()
    }

    pub fn fail(
        &mut self,
        label: &str,
        theme: &ColorTheme,
        out: &mut impl Write,
    ) -> io::Result<()> {
        self.frame_index = 0;
        execute!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_failed),
            Print(format!("✘ {label}\n")),
            ResetColor
        )?;
        out.flush()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListKind {
    Unordered,
    Ordered { next_index: u64 },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TableState {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_head: bool,
}

impl TableState {
    fn push_cell(&mut self) {
        let cell = self.current_cell.trim().to_string();
        self.current_row.push(cell);
        self.current_cell.clear();
    }

    fn finish_row(&mut self) {
        if self.current_row.is_empty() {
            return;
        }
        let row = std::mem::take(&mut self.current_row);
        if self.in_head {
            self.headers = row;
        } else {
            self.rows.push(row);
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RenderState {
    emphasis: usize,
    strong: usize,
    heading_level: Option<u8>,
    quote: usize,
    quote_line_start: bool,
    list_stack: Vec<ListKind>,
    link_stack: Vec<LinkState>,
    table: Option<TableState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkState {
    destination: String,
    text: String,
}

impl RenderState {
    fn style_text(&self, text: &str, theme: &ColorTheme) -> String {
        let mut style = text.stylize();

        if matches!(self.heading_level, Some(1 | 2)) || self.strong > 0 {
            style = style.bold();
        }
        if self.emphasis > 0 {
            style = style.italic();
        }

        if let Some(level) = self.heading_level {
            style = match level {
                1 => style.with(theme.heading),
                2 => style.white(),
                3 => style.with(Color::Blue),
                _ => style.with(Color::Grey),
            };
        } else if self.strong > 0 {
            style = style.with(theme.strong);
        } else if self.emphasis > 0 {
            style = style.with(theme.emphasis);
        }

        if self.quote > 0 {
            style = style.with(theme.quote);
        }

        format!("{style}")
    }

    fn append_raw(&mut self, output: &mut String, text: &str, theme: &ColorTheme) {
        if let Some(link) = self.link_stack.last_mut() {
            link.text.push_str(text);
        } else if let Some(table) = self.table.as_mut() {
            table.current_cell.push_str(text);
        } else if self.quote > 0 {
            self.append_quoted(output, text, theme);
        } else {
            output.push_str(text);
        }
    }

    fn append_styled(&mut self, output: &mut String, text: &str, theme: &ColorTheme) {
        let styled = self.style_text(text, theme);
        self.append_raw(output, &styled, theme);
    }

    fn append_quoted(&mut self, output: &mut String, text: &str, theme: &ColorTheme) {
        let bar = format!("{}", "│".with(theme.quote_bar));
        for segment in text.split_inclusive('\n') {
            let line = segment.trim_end_matches('\n');
            let visible = strip_ansi(line);
            let has_content = !visible.trim().is_empty();
            if self.quote_line_start && has_content {
                output.push_str(&bar);
                output.push(' ');
            }
            output.push_str(line);
            if segment.ends_with('\n') {
                output.push('\n');
                self.quote_line_start = true;
            } else if has_content {
                self.quote_line_start = false;
            }
        }
    }
}

#[derive(Debug)]
pub struct TerminalRenderer {
    syntax_set: SyntaxSet,
    syntax_theme: Theme,
    color_theme: ColorTheme,
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax_theme = ThemeSet::load_defaults()
            .themes
            .remove("base16-ocean.dark")
            .unwrap_or_default();
        Self {
            syntax_set,
            syntax_theme,
            color_theme: ColorTheme::default(),
        }
    }
}

impl TerminalRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn color_theme(&self) -> &ColorTheme {
        &self.color_theme
    }

    #[must_use]
    pub fn render_markdown(&self, markdown: &str) -> String {
        let mut output = String::new();
        let mut state = RenderState::default();
        let mut code_language = String::new();
        let mut code_buffer = String::new();
        let mut in_code_block = false;

        for event in Parser::new_ext(markdown, Options::all()) {
            self.render_event(
                event,
                &mut state,
                &mut output,
                &mut code_buffer,
                &mut code_language,
                &mut in_code_block,
            );
        }

        output.trim_end().to_string()
    }

    #[must_use]
    pub fn markdown_to_ansi(&self, markdown: &str) -> String {
        self.render_markdown(markdown)
    }

    #[allow(clippy::too_many_lines)]
    fn render_event(
        &self,
        event: Event<'_>,
        state: &mut RenderState,
        output: &mut String,
        code_buffer: &mut String,
        code_language: &mut String,
        in_code_block: &mut bool,
    ) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                self.start_heading(state, level as u8, output);
            }
            Event::End(TagEnd::Paragraph) => state.append_raw(output, "\n\n", &self.color_theme),
            Event::Start(Tag::BlockQuote(..)) => self.start_quote(state, output),
            Event::End(TagEnd::BlockQuote(..)) => {
                state.quote = state.quote.saturating_sub(1);
                state.quote_line_start = true;
                output.push('\n');
            }
            Event::End(TagEnd::Heading(..)) => {
                state.heading_level = None;
                output.push_str("\n\n");
            }
            Event::End(TagEnd::Item) | Event::SoftBreak | Event::HardBreak => {
                state.append_raw(output, "\n", &self.color_theme);
            }
            Event::Start(Tag::List(first_item)) => {
                let kind = match first_item {
                    Some(index) => ListKind::Ordered { next_index: index },
                    None => ListKind::Unordered,
                };
                state.list_stack.push(kind);
            }
            Event::End(TagEnd::List(..)) => {
                state.list_stack.pop();
                output.push('\n');
            }
            Event::Start(Tag::Item) => Self::start_item(state, output),
            Event::Start(Tag::CodeBlock(kind)) => {
                *in_code_block = true;
                *code_language = match kind {
                    CodeBlockKind::Indented => String::from("text"),
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                };
                code_buffer.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                self.render_code_block(code_buffer, code_language, output);
                *in_code_block = false;
                code_language.clear();
                code_buffer.clear();
            }
            Event::Start(Tag::Emphasis) => state.emphasis += 1,
            Event::End(TagEnd::Emphasis) => state.emphasis = state.emphasis.saturating_sub(1),
            Event::Start(Tag::Strong) => state.strong += 1,
            Event::End(TagEnd::Strong) => state.strong = state.strong.saturating_sub(1),
            Event::Code(code) => {
                let rendered =
                    format!("{}", format!("`{code}`").with(self.color_theme.inline_code));
                state.append_raw(output, &rendered, &self.color_theme);
            }
            Event::Rule => output.push_str("---\n"),
            Event::Text(text) => {
                self.push_text(text.as_ref(), state, output, code_buffer, *in_code_block);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                state.append_raw(output, &html, &self.color_theme);
            }
            Event::FootnoteReference(reference) => {
                state.append_raw(output, &format!("[{reference}]"), &self.color_theme);
            }
            Event::TaskListMarker(done) => {
                state.append_raw(output, if done { "[x] " } else { "[ ] " }, &self.color_theme);
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                state.append_raw(output, &math, &self.color_theme);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                state.link_stack.push(LinkState {
                    destination: dest_url.to_string(),
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = state.link_stack.pop() {
                    let label = if link.text.is_empty() {
                        link.destination.clone()
                    } else {
                        link.text
                    };
                    let rendered = format!(
                        "{}",
                        format!("[{label}]({})", link.destination)
                            .underlined()
                            .with(self.color_theme.link)
                    );
                    state.append_raw(output, &rendered, &self.color_theme);
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                let rendered = format!(
                    "{}",
                    format!("[image:{dest_url}]").with(self.color_theme.link)
                );
                state.append_raw(output, &rendered, &self.color_theme);
            }
            Event::Start(Tag::Table(..)) => state.table = Some(TableState::default()),
            Event::End(TagEnd::Table) => {
                if let Some(table) = state.table.take() {
                    output.push_str(&self.render_table(&table));
                    output.push_str("\n\n");
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(table) = state.table.as_mut() {
                    table.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(table) = state.table.as_mut() {
                    table.finish_row();
                    table.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {
                if let Some(table) = state.table.as_mut() {
                    table.current_row.clear();
                    table.current_cell.clear();
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = state.table.as_mut() {
                    table.finish_row();
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(table) = state.table.as_mut() {
                    table.current_cell.clear();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(table) = state.table.as_mut() {
                    table.push_cell();
                }
            }
            Event::Start(Tag::Paragraph | Tag::MetadataBlock(..) | _)
            | Event::End(TagEnd::Image | TagEnd::MetadataBlock(..) | _) => {}
        }
    }

    #[allow(clippy::unused_self)]
    fn start_heading(&self, state: &mut RenderState, level: u8, output: &mut String) {
        state.heading_level = Some(level);
        if !output.is_empty() {
            output.push('\n');
        }
    }

    fn start_quote(&self, state: &mut RenderState, output: &mut String) {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        state.quote += 1;
        state.quote_line_start = true;
    }

    fn start_item(state: &mut RenderState, output: &mut String) {
        let depth = state.list_stack.len().saturating_sub(1);
        output.push_str(&"  ".repeat(depth));

        let marker = match state.list_stack.last_mut() {
            Some(ListKind::Ordered { next_index }) => {
                let value = *next_index;
                *next_index += 1;
                format!("{value}. ")
            }
            _ => "• ".to_string(),
        };
        output.push_str(&marker);
    }

    fn render_code_block(&self, code_buffer: &str, code_language: &str, output: &mut String) {
        let label = if code_language.trim().is_empty() {
            "text"
        } else {
            code_language.trim()
        };
        let highlighted_lines = self.highlight_code_lines(code_buffer, code_language);
        let content_width = highlighted_lines
            .iter()
            .map(|line| visible_width(line))
            .max()
            .unwrap_or(0)
            .max(label.chars().count() + 2);

        let top_fill = (content_width + 2).saturating_sub(label.chars().count() + 3);
        let top = format!(
            "{}{}{}{}",
            format!("{}", "╭─".bold().with(self.color_theme.code_block_border)),
            format!(" {} ", label)
                .bold()
                .with(self.color_theme.inline_code),
            format!("{}", "─".repeat(top_fill).bold().with(self.color_theme.code_block_border)),
            format!("{}", "╮".bold().with(self.color_theme.code_block_border))
        );
        let side = format!("{}", "│".with(self.color_theme.code_block_border));
        let bottom = format!(
            "{}{}{}",
            format!("{}", "╰".bold().with(self.color_theme.code_block_border)),
            format!("{}", "─".repeat(content_width + 2).bold().with(self.color_theme.code_block_border)),
            format!("{}", "╯".bold().with(self.color_theme.code_block_border))
        );

        let _ = writeln!(output, "{top}");
        for line in highlighted_lines {
            let padded = pad_ansi_line_to_width(&line, content_width);
            let _ = writeln!(output, "{side} {padded} {side}");
        }
        let _ = writeln!(output, "{bottom}");
        output.push('\n');
    }

    fn push_text(
        &self,
        text: &str,
        state: &mut RenderState,
        output: &mut String,
        code_buffer: &mut String,
        in_code_block: bool,
    ) {
        if in_code_block {
            code_buffer.push_str(text);
        } else {
            state.append_styled(output, text, &self.color_theme);
        }
    }

    fn render_table(&self, table: &TableState) -> String {
        let mut rows = Vec::new();
        if !table.headers.is_empty() {
            rows.push(table.headers.clone());
        }
        rows.extend(table.rows.iter().cloned());

        if rows.is_empty() {
            return String::new();
        }

        let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths = (0..column_count)
            .map(|column| {
                rows.iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| visible_width(cell))
                    .max()
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();

        let top = self.render_table_border(&widths, '┌', '┬', '┐');
        let separator = self.render_table_border(&widths, '├', '┼', '┤');
        let bottom = self.render_table_border(&widths, '└', '┴', '┘');

        let mut output = String::new();
        output.push_str(&top);
        output.push('\n');
        if !table.headers.is_empty() {
            output.push_str(&self.render_table_row(&table.headers, &widths, true));
            output.push('\n');
            output.push_str(&separator);
            if !table.rows.is_empty() {
                output.push('\n');
            }
        }

        for (index, row) in table.rows.iter().enumerate() {
            output.push_str(&self.render_table_row(row, &widths, false));
            if index + 1 < table.rows.len() {
                output.push('\n');
            }
        }

        if !rows.is_empty() {
            output.push('\n');
            output.push_str(&bottom);
        }

        output
    }

    fn render_table_border(&self, widths: &[usize], left: char, middle: char, right: char) -> String {
        let left = format!("{}", left.to_string().with(self.color_theme.table_border));
        let right = format!("{}", right.to_string().with(self.color_theme.table_border));
        let middle = format!("{}", middle.to_string().with(self.color_theme.table_border));
        let segment = widths
            .iter()
            .map(|width| format!("{}", "─".repeat(*width + 2).with(self.color_theme.table_border)))
            .collect::<Vec<_>>()
            .join(&middle);
        format!("{left}{segment}{right}")
    }

    fn render_table_row(&self, row: &[String], widths: &[usize], is_header: bool) -> String {
        let border = format!("{}", "│".with(self.color_theme.table_border));
        let mut line = String::new();
        line.push_str(&border);

        for (index, width) in widths.iter().enumerate() {
            let cell = row.get(index).map_or("", String::as_str);
            line.push(' ');
            if is_header {
                let _ = write!(line, "{}", cell.bold().with(self.color_theme.heading));
            } else {
                line.push_str(cell);
            }
            let padding = width.saturating_sub(visible_width(cell));
            line.push_str(&" ".repeat(padding + 1));
            line.push_str(&border);
        }

        line
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn highlight_code(&self, code: &str, language: &str) -> String {
        self.highlight_code_lines(code, language).join("\n")
    }

    fn highlight_code_lines(&self, code: &str, language: &str) -> Vec<String> {
        let syntax = self
            .syntax_set
            .find_syntax_by_token(language)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let mut syntax_highlighter = HighlightLines::new(syntax, &self.syntax_theme);
        let mut colored_output = Vec::new();
        let mut saw_line = false;

        for line in LinesWithEndings::from(code) {
            saw_line = true;
            let plain = line.trim_end_matches('\n');
            let rendered = match syntax_highlighter.highlight_line(plain, &self.syntax_set) {
                Ok(ranges) => as_24_bit_terminal_escaped(&ranges[..], false),
                Err(_) => plain.to_string(),
            };
            colored_output.push(apply_code_block_background(&rendered));
        }

        if !saw_line {
            colored_output.push(apply_code_block_background(""));
        }

        colored_output
    }

    pub fn stream_markdown(&self, markdown: &str, out: &mut impl Write) -> io::Result<()> {
        let rendered_markdown = self.markdown_to_ansi(markdown);
        write!(out, "{rendered_markdown}")?;
        if !rendered_markdown.ends_with('\n') {
            writeln!(out)?;
        }
        out.flush()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MarkdownStreamState {
    pub(crate) pending: String,
}

impl MarkdownStreamState {
    #[must_use]
    #[allow(dead_code)]
    pub fn push(&mut self, renderer: &TerminalRenderer, delta: &str) -> Option<String> {
        self.pending.push_str(delta);
        let split = find_stream_safe_boundary(&self.pending)?;
        let ready = self.pending[..split].to_string();
        self.pending.drain(..split);
        Some(renderer.markdown_to_ansi(&ready))
    }

    #[must_use]
    pub fn flush(&mut self, renderer: &TerminalRenderer) -> Option<String> {
        if self.pending.trim().is_empty() {
            self.pending.clear();
            None
        } else {
            let pending = std::mem::take(&mut self.pending);
            Some(renderer.markdown_to_ansi(&pending))
        }
    }
}

fn apply_code_block_background(line: &str) -> String {
    let trimmed = line.trim_end_matches('\n');
    let trailing_newline = if trimmed.len() == line.len() {
        ""
    } else {
        "\n"
    };
    let with_background = trimmed.replace("\u{1b}[0m", "\u{1b}[0;48;5;236m");
    format!("\u{1b}[48;5;236m{with_background}\u{1b}[0m{trailing_newline}")
}

fn pad_ansi_line_to_width(line: &str, width: usize) -> String {
    let padding = width.saturating_sub(visible_width(line));
    if padding == 0 {
        return line.to_string();
    }

    let spaces = " ".repeat(padding);
    if let Some(stripped) = line.strip_suffix("\u{1b}[0m") {
        format!("{stripped}{spaces}\u{1b}[0m")
    } else {
        format!("{line}{spaces}")
    }
}

fn looks_like_markdown_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && !trimmed.starts_with("```")
        && !trimmed.starts_with("~~~")
        && trimmed.matches('|').count() >= 2
}

#[allow(dead_code)]
fn find_stream_safe_boundary(markdown: &str) -> Option<usize> {
    let mut in_fence = false;
    let mut in_table = false;
    let mut last_boundary = None;

    for (offset, line) in markdown.split_inclusive('\n').scan(0usize, |cursor, line| {
        let start = *cursor;
        *cursor += line.len();
        Some((start, line))
    }) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            if !in_fence {
                last_boundary = Some(offset + line.len());
            }
            continue;
        }

        if in_fence {
            continue;
        }

        let is_blank = trimmed.trim().is_empty();
        let is_table_line = looks_like_markdown_table_line(trimmed);

        if in_table {
            if is_blank {
                in_table = false;
                last_boundary = Some(offset + line.len());
                continue;
            }
            if is_table_line {
                continue;
            }
            last_boundary = Some(offset);
            break;
        }

        if is_table_line {
            in_table = true;
            continue;
        }

        if is_blank {
            last_boundary = Some(offset + line.len());
        }

        // Flush at any completed line to reduce first-token latency.
        // Without this, text buffers until a blank line or fence boundary,
        // making the stream feel stuck for the first few seconds.
        if line.ends_with('\n') {
            last_boundary = Some(offset + line.len());
        }
    }

    // If no newline boundary found but we have enough pending text,
    // flush what we have to keep the stream feeling responsive.
    if last_boundary.is_none() && !in_table && markdown.len() > 80 {
        last_boundary = Some(markdown.len());
    }

    last_boundary
}

fn visible_width(input: &str) -> usize {
    strip_ansi(input).chars().count()
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
        } else {
            output.push(ch);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::{strip_ansi, MarkdownStreamState, Spinner, TerminalRenderer};

    #[test]
    fn renders_markdown_with_styling_and_lists() {
        let terminal_renderer = TerminalRenderer::new();
        let markdown_output = terminal_renderer
            .render_markdown("# Heading\n\nThis is **bold** and *italic*.\n\n- item\n\n`code`");

        assert!(markdown_output.contains("Heading"));
        assert!(markdown_output.contains("• item"));
        assert!(markdown_output.contains("code"));
        assert!(markdown_output.contains('\u{1b}'));
    }

    #[test]
    fn renders_links_as_colored_markdown_labels() {
        let terminal_renderer = TerminalRenderer::new();
        let markdown_output =
            terminal_renderer.render_markdown("See [Claw](https://example.com/docs) now.");
        let plain_text = strip_ansi(&markdown_output);

        assert!(plain_text.contains("[Claw](https://example.com/docs)"));
        assert!(markdown_output.contains('\u{1b}'));
    }

    #[test]
    fn highlights_fenced_code_blocks() {
        let terminal_renderer = TerminalRenderer::new();
        let markdown_output =
            terminal_renderer.markdown_to_ansi("```rust\nfn hi() { println!(\"hi\"); }\n```");
        let plain_text = strip_ansi(&markdown_output);

        assert!(plain_text.contains("╭─ rust"));
        assert!(plain_text.contains("│ fn hi() { println!(\"hi\"); } │"));
        assert!(plain_text.contains("fn hi"));
        assert!(markdown_output.contains('\u{1b}'));
        assert!(markdown_output.contains("[48;5;236m"));
        assert!(plain_text.contains('╯'));
    }

    #[test]
    fn renders_ordered_and_nested_lists() {
        let terminal_renderer = TerminalRenderer::new();
        let markdown_output =
            terminal_renderer.render_markdown("1. first\n2. second\n   - nested\n   - child");
        let plain_text = strip_ansi(&markdown_output);

        assert!(plain_text.contains("1. first"));
        assert!(plain_text.contains("2. second"));
        assert!(plain_text.contains("  • nested"));
        assert!(plain_text.contains("  • child"));
    }

    #[test]
    fn renders_tables_with_alignment() {
        let terminal_renderer = TerminalRenderer::new();
        let markdown_output = terminal_renderer
            .render_markdown("| Name | Value |\n| ---- | ----- |\n| alpha | 1 |\n| beta | 22 |");
        let plain_text = strip_ansi(&markdown_output);
        let lines = plain_text.lines().collect::<Vec<_>>();

        assert_eq!(lines[0], "┌───────┬───────┐");
        assert_eq!(lines[1], "│ Name  │ Value │");
        assert_eq!(lines[2], "├───────┼───────┤");
        assert_eq!(lines[3], "│ alpha │ 1     │");
        assert_eq!(lines[4], "│ beta  │ 22    │");
        assert_eq!(lines[5], "└───────┴───────┘");
        assert!(markdown_output.contains('\u{1b}'));
    }

    #[test]
    fn blockquotes_prefix_each_line() {
        let terminal_renderer = TerminalRenderer::new();
        let markdown_output = terminal_renderer.render_markdown(
            "> first line\n> second line\n>\n> tail"
        );
        let plain_text = strip_ansi(&markdown_output);

        assert!(plain_text.contains("│ first line"));
        assert!(plain_text.contains("│ second line"));
        assert!(plain_text.contains("│ tail"));
    }

    #[test]
    fn streaming_state_waits_for_complete_blocks() {
        let renderer = TerminalRenderer::new();
        let mut state = MarkdownStreamState::default();

        assert_eq!(state.push(&renderer, "# Heading"), None);
        let flushed = state
            .push(&renderer, "\n\nParagraph\n\n")
            .expect("completed block");
        let plain_text = strip_ansi(&flushed);
        assert!(plain_text.contains("Heading"));
        assert!(plain_text.contains("Paragraph"));

        assert_eq!(state.push(&renderer, "```rust\nfn main() {}\n"), None);
        let code = state
            .push(&renderer, "```\n")
            .expect("closed code fence flushes");
        assert!(strip_ansi(&code).contains("fn main()"));
    }

    #[test]
    fn streaming_state_waits_for_complete_tables() {
        let renderer = TerminalRenderer::new();
        let mut state = MarkdownStreamState::default();

        assert_eq!(state.push(&renderer, "| Key | Value |\n"), None);
        assert_eq!(state.push(&renderer, "| --- | --- |\n"), None);
        assert_eq!(state.push(&renderer, "| tool | TOOL_OK |\n"), None);

        let table = state.push(&renderer, "\n").expect("blank line flushes table");
        let plain_text = strip_ansi(&table);

        assert!(plain_text.contains("│ Key"));
        assert!(plain_text.contains("│ tool"));
        assert!(!plain_text.contains("| Key | Value |"));
    }

    #[test]
    fn streaming_state_flush_renders_trailing_table_block() {
        let renderer = TerminalRenderer::new();
        let mut state = MarkdownStreamState::default();

        assert_eq!(state.push(&renderer, "| Name | Value |\n"), None);
        assert_eq!(state.push(&renderer, "| ---- | ----- |\n"), None);
        assert_eq!(state.push(&renderer, "| alpha | 1 |\n"), None);

        let table = state.flush(&renderer).expect("flush renders trailing table");
        let plain_text = strip_ansi(&table);

        assert!(plain_text.contains("│ Name"));
        assert!(plain_text.contains("│ alpha"));
        assert!(!plain_text.contains("| Name | Value |"));
    }

    #[test]
    fn spinner_advances_frames() {
        let terminal_renderer = TerminalRenderer::new();
        let mut spinner = Spinner::new();
        let mut out = Vec::new();
        spinner
            .tick("Working", terminal_renderer.color_theme(), &mut out)
            .expect("tick succeeds");
        spinner
            .tick("Working", terminal_renderer.color_theme(), &mut out)
            .expect("tick succeeds");

        let output = String::from_utf8_lossy(&out);
        assert!(output.contains("Working"));
    }
}
