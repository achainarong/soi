//! Terminal state: the character grid, scrollback, cursor and modes, driven
//! by escape sequences from the `vte` parser (Alacritty's parser crate, not
//! GNOME's VTE).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use super::graphics::{Graphics, PlaceContext};
use super::links::{MAX_URL_LEN, find_url};
use super::sixel;

/// Distinct OSC 8 links kept per terminal; later ones are ignored.
const MAX_LINKS: usize = 4096;
/// Prompt positions remembered for jumping between commands.
const MAX_PROMPTS: usize = 10_000;
/// Longest command line kept for a finished command's notice.
const MAX_COMMAND_CHARS: usize = 200;
/// Longest window title kept.
const MAX_TITLE_CHARS: usize = 256;
/// The most a program can put on the clipboard at once (OSC 52), decoded.
const MAX_CLIPBOARD_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub mod attr {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const INVERSE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    pub const STRIKE: u16 = 1 << 7;
    /// Underline styles other than a single line (SGR 4:2 to 4:5); each
    /// comes with `UNDERLINE` set.
    pub const DOUBLE_UNDERLINE: u16 = 1 << 8;
    pub const CURLY_UNDERLINE: u16 = 1 << 9;
    pub const DOTTED_UNDERLINE: u16 = 1 << 10;
    pub const DASHED_UNDERLINE: u16 = 1 << 11;
    pub const ANY_UNDERLINE: u16 =
        UNDERLINE | DOUBLE_UNDERLINE | CURLY_UNDERLINE | DOTTED_UNDERLINE | DASHED_UNDERLINE;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: u16,
    /// Underline color (SGR 58); `Default` is the text color.
    pub underline: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Width {
    #[default]
    Narrow,
    /// First half of a double-width character.
    Wide,
    /// Second half of a double-width character; draws nothing.
    Spacer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
    pub width: Width,
    /// Combining characters following `ch` (accents, emoji modifiers).
    pub extra: Option<Box<str>>,
    /// OSC 8 link id (see `Term::links`), 0 for none.
    pub link: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::default(),
            width: Width::Narrow,
            extra: None,
            link: 0,
        }
    }
}

impl Cell {
    fn blank(bg: Color) -> Self {
        Self {
            style: Style {
                bg,
                ..Style::default()
            },
            ..Self::default()
        }
    }

    pub fn push_text(&self, out: &mut String) {
        out.push(self.ch);
        if let Some(extra) = &self.extra {
            out.push_str(extra);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Continued on the next row because of autowrap.
    pub wrapped: bool,
}

impl Row {
    fn new(cols: usize, bg: Color) -> Self {
        Self {
            cells: vec![Cell::blank(bg); cols],
            wrapped: false,
        }
    }

    fn resize(&mut self, cols: usize) {
        self.cells.resize(cols, Cell::default());
        if let Some(last) = self.cells.last_mut()
            && last.width == Width::Wide
        {
            *last = Cell::default();
        }
    }

    /// The row's text with trailing blanks removed.
    pub fn text(&self, from: usize, to: usize) -> String {
        let mut out = String::new();
        for cell in self.cells.iter().take(to).skip(from) {
            if cell.width != Width::Spacer {
                cell.push_text(&mut out);
            }
        }
        out.truncate(out.trim_end().len());
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    row: usize,
    col: usize,
    style: Style,
    /// The last column was written; the next character wraps first.
    pending_wrap: bool,
    origin: bool,
    /// Whether G0/G1 use the DEC line-drawing set.
    line_drawing: [bool; 2],
    charset: usize,
    /// Current OSC 8 link id, 0 for none.
    link: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub autowrap: bool,
    pub insert: bool,
    pub cursor_visible: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_reporting: bool,
    /// Which pointer motion is reported too.
    pub mouse_motion: MouseMotion,
    pub sgr_mouse: bool,
    pub cursor_shape: CursorShape,
    /// Whether the program wants the cursor to blink (`None`: up to the
    /// user's setting).
    pub cursor_blink: Option<bool>,
    /// Report focus changes (mode 1004).
    pub focus_events: bool,
    /// The program is drawing a frame; hold off showing it (mode 2026).
    pub synchronized: bool,
}

/// Which pointer motion a program wants reported, besides clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMotion {
    None,
    /// While a button is held (mode 1002), so it can select by dragging.
    Drags,
    /// All of it (mode 1003).
    All,
}

/// How keys are encoded for the program, as it asked for.
#[derive(Clone, Debug, Default)]
pub struct Keyboard {
    /// Kitty keyboard protocol flags, a stack per screen (main, alternate).
    stacks: [Vec<u8>; 2],
    /// xterm's modifyOtherKeys level (0, 1 or 2).
    pub modify_other_keys: u8,
}

/// Kitty keyboard flags soi implements: disambiguate escape codes (1) and
/// report all keys as escape codes (8).
pub const KITTY_SUPPORTED: u8 = 1 | 8;
/// Deep enough for any program; the oldest entries are dropped beyond it.
const MAX_KEYBOARD_STACK: usize = 32;

impl Keyboard {
    pub fn kitty_flags(&self, alt: bool) -> u8 {
        self.stacks[alt as usize].last().copied().unwrap_or(0)
    }

    fn push(&mut self, alt: bool, flags: u8) {
        let stack = &mut self.stacks[alt as usize];
        if stack.len() >= MAX_KEYBOARD_STACK {
            stack.remove(0);
        }
        stack.push(flags & KITTY_SUPPORTED);
    }

    /// A program leaving the alternate screen takes its settings along.
    fn clear_alt(&mut self) {
        self.stacks[1].clear();
    }

    fn pop(&mut self, alt: bool, count: usize) {
        let stack = &mut self.stacks[alt as usize];
        let keep = stack.len().saturating_sub(count);
        stack.truncate(keep);
    }

    /// `CSI = flags ; mode u`: 1 sets, 2 adds, 3 removes flags.
    fn set(&mut self, alt: bool, flags: u8, mode: u16) {
        let current = self.kitty_flags(alt);
        let flags = match mode {
            2 => current | flags,
            3 => current & !flags,
            _ => flags,
        } & KITTY_SUPPORTED;
        let stack = &mut self.stacks[alt as usize];
        match stack.last_mut() {
            Some(top) => *top = flags,
            None => stack.push(flags),
        }
    }
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            autowrap: true,
            insert: false,
            cursor_visible: true,
            app_cursor: false,
            bracketed_paste: false,
            mouse_reporting: false,
            mouse_motion: MouseMotion::None,
            sgr_mouse: false,
            cursor_shape: CursorShape::Block,
            cursor_blink: None,
            focus_events: false,
            synchronized: false,
        }
    }
}

pub struct Term {
    cols: usize,
    rows: usize,
    /// The active screen (primary or alternate).
    screen: Vec<Row>,
    /// The primary screen while the alternate one is active.
    primary: Option<Vec<Row>>,
    scrollback: VecDeque<Row>,
    scrollback_limit: usize,
    /// Lines that have left the top of the primary screen so far, so screen
    /// row `r` is absolute line `scrolled_off + r`. Images are anchored to
    /// absolute lines so they scroll with the text.
    scrolled_off: i64,
    cursor: Cursor,
    saved_cursor: Cursor,
    /// Scroll region, inclusive.
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    pub modes: Modes,
    pub keyboard: Keyboard,
    title: Option<String>,
    /// A bell (empty) or notification (OSC 9 / 777) since last asked.
    attention: Option<String>,
    /// Bytes to send back to the program (answers to queries).
    responses: Vec<u8>,
    /// Text a program copied (OSC 52) since last asked.
    copied: Option<Copy>,
    pub graphics: Graphics,
    /// Cell size in pixels, for image placement and size queries.
    cell_px: (f64, f64),
    /// Default colors, reported for OSC 4/10/11 queries.
    pub report_fg: (u8, u8, u8),
    pub report_bg: (u8, u8, u8),
    pub report_palette: [(u8, u8, u8); 256],
    /// Colors the program changed (OSC 4/10/11/12), and whether they
    /// changed since last asked.
    colors: Colors,
    colors_changed: bool,
    last_char: Option<char>,
    /// OSC 8 link targets; cell link id `n` is `links[n - 1]`.
    links: Vec<String>,
    link_ids: HashMap<String, u32>,
    /// The shell's commands, from where their prompts start (OSC 133;A),
    /// oldest first.
    prompts: VecDeque<Command>,
    /// A command that finished since last asked (see `take_finished`).
    finished: Option<Finished>,
    /// What the program reports of its progress (OSC 9;4), and whether
    /// that changed since last asked.
    progress: Option<Progress>,
    progress_changed: bool,
    /// The shell's folder as it reported it (OSC 7): host and path.
    reported_cwd: Option<(String, String)>,
    /// A Sixel image arriving (DCS q).
    sixel: Option<Box<sixel::Decoder>>,
}

/// One shell command, as the shell marks it (OSC 133): its prompt (A),
/// what was typed (B), its output (C) and its end with the exit status (D).
/// Positions are (absolute line, column).
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub prompt: i64,
    input: Option<(i64, usize)>,
    pub output: Option<(i64, usize)>,
    pub end: Option<(i64, usize)>,
    pub exit: Option<i32>,
    command: Option<String>,
    started: Option<Instant>,
}

impl Command {
    fn new(prompt: i64) -> Self {
        Self {
            prompt,
            input: None,
            output: None,
            end: None,
            exit: None,
            command: None,
            started: None,
        }
    }

    pub fn failed(&self) -> bool {
        self.exit.is_some_and(|exit| exit != 0)
    }
}

/// Colors a program set instead of the theme's (`None`: the theme's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub palette: [Option<(u8, u8, u8)>; 256],
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    pub cursor: Option<(u8, u8, u8)>,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            palette: [None; 256],
            fg: None,
            bg: None,
            cursor: None,
        }
    }
}

/// An X11 color spec: `rgb:r/g/b` (1 to 4 hex digits each) or `#rgb`,
/// `#rrggbb`, `#rrrgggbbb`, `#rrrrggggbbbb`.
fn parse_color(spec: &[u8]) -> Option<(u8, u8, u8)> {
    let spec = std::str::from_utf8(spec).ok()?;
    if let Some(color) = named_color(spec) {
        return Some(color);
    }
    // Scales a hex component of any length to 8 bits.
    let scale = |hex: &str| -> Option<u8> {
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * hex.len())) - 1;
        Some((value * 255 / max) as u8)
    };
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut parts = rest.split('/');
        let color = (
            scale(parts.next()?)?,
            scale(parts.next()?)?,
            scale(parts.next()?)?,
        );
        return parts.next().is_none().then_some(color);
    }
    let hex = spec.strip_prefix('#')?;
    if hex.len() % 3 != 0 || !hex.is_ascii() {
        return None;
    }
    let n = hex.len() / 3;
    // In this form, shorter components are the high bits (`#fff` = f0f0f0).
    let part = |i: usize| -> Option<u8> {
        let value = scale(&hex[i * n..(i + 1) * n])?;
        Some(if n == 1 { value & 0xf0 } else { value })
    };
    Some((part(0)?, part(1)?, part(2)?))
}

/// The common X11 color names (as in its `rgb.txt`), ignoring case and
/// spaces.
fn named_color(name: &str) -> Option<(u8, u8, u8)> {
    let name: String = name
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    Some(match name.as_str() {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" | "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "gray" | "grey" => (190, 190, 190),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "dimgray" | "dimgrey" => (105, 105, 105),
        "silver" => (192, 192, 192),
        "orange" => (255, 165, 0),
        "purple" => (160, 32, 240),
        "brown" => (165, 42, 42),
        "pink" => (255, 192, 203),
        "navy" | "navyblue" => (0, 0, 128),
        "maroon" => (176, 48, 96),
        "olive" => (128, 128, 0),
        "teal" => (0, 128, 128),
        "darkred" => (139, 0, 0),
        "darkgreen" => (0, 100, 0),
        "darkblue" => (0, 0, 139),
        _ => return None,
    })
}

/// How far a program is, as it reports it (OSC 9;4, from ConEmu; also
/// Windows Terminal and Ghostty).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Percent done.
    Normal(u8),
    Error(u8),
    Paused(u8),
    /// Busy, without knowing how long it takes.
    Indeterminate,
}

/// `%20` → ` `; invalid escapes stay as they are.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A command that ended (see `Term::take_finished`).
#[derive(Clone, Debug, PartialEq)]
pub struct Finished {
    /// What was typed, when the shell marked it.
    pub command: Option<String>,
    pub exit: Option<i32>,
    pub took: Duration,
}

impl Term {
    pub fn new(cols: usize, rows: usize, scrollback_limit: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            cols,
            rows,
            screen: (0..rows).map(|_| Row::new(cols, Color::Default)).collect(),
            primary: None,
            scrollback: VecDeque::new(),
            scrollback_limit,
            scrolled_off: 0,
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            top: 0,
            bottom: rows - 1,
            tabs: default_tabs(cols),
            modes: Modes::default(),
            keyboard: Keyboard::default(),
            title: None,
            attention: None,
            responses: Vec::new(),
            copied: None,
            graphics: Graphics::default(),
            cell_px: (8.0, 16.0),
            report_fg: (0xd0, 0xd0, 0xd0),
            report_bg: (0x1e, 0x1e, 0x1e),
            report_palette: [(0, 0, 0); 256],
            colors: Colors::default(),
            colors_changed: false,
            last_char: None,
            links: Vec::new(),
            link_ids: HashMap::new(),
            prompts: VecDeque::new(),
            finished: None,
            reported_cwd: None,
            sixel: None,
            progress: None,
            progress_changed: false,
        }
    }

    // -- accessors used by the widget ---------------------------------------

    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    pub fn scrolled_off(&self) -> i64 {
        self.scrolled_off
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn alt_active(&self) -> bool {
        self.primary.is_some()
    }

    /// Cursor position on the screen, if it should be drawn.
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.modes
            .cursor_visible
            .then_some((self.cursor.row, self.cursor.col))
    }

    /// A line by absolute number: scrollback lines are below `scrolled_off`.
    pub fn line(&self, abs: i64) -> Option<&Row> {
        if abs >= self.scrolled_off {
            self.screen
                .get(usize::try_from(abs - self.scrolled_off).ok()?)
        } else {
            let back = usize::try_from(self.scrolled_off - abs).ok()?;
            self.scrollback
                .get(self.scrollback.len().checked_sub(back)?)
        }
    }

    /// The normal screen's last `max_lines` rows (also while a full-screen
    /// program runs) with their colors, as text and escape sequences, for
    /// saving: fed back in, they show the same output. Soft-wrapped rows
    /// stay one line, so they re-wrap to the width they come back at.
    pub fn export(&self, max_lines: usize) -> String {
        let screen = self.primary.as_ref().unwrap_or(&self.screen);
        let mut rows: Vec<&Row> = self.scrollback.iter().chain(screen.iter()).collect();
        while rows.last().is_some_and(|row| is_blank(row)) {
            rows.pop();
        }
        let mut start = rows.len().saturating_sub(max_lines);
        // Don't start in the middle of a wrapped line.
        while start > 0 && rows[start - 1].wrapped {
            start -= 1;
        }
        let mut out = String::new();
        let mut style = Style::default();
        for row in &rows[start..] {
            let end = if row.wrapped {
                row.cells.len()
            } else {
                row.cells
                    .iter()
                    .rposition(|c| !is_blank_cell(c))
                    .map_or(0, |i| i + 1)
            };
            for cell in &row.cells[..end] {
                if cell.width == Width::Spacer {
                    continue;
                }
                if cell.style != style {
                    style = cell.style;
                    out.push_str(&sgr_sequence(&style));
                }
                cell.push_text(&mut out);
            }
            if !row.wrapped {
                if style != Style::default() {
                    style = Style::default();
                    out.push_str("\x1b[0m");
                }
                out.push_str("\r\n");
            }
        }
        if style != Style::default() {
            out.push_str("\x1b[0m");
        }
        out
    }

    pub fn take_title(&mut self) -> Option<String> {
        self.title.take()
    }

    /// A bell rang (`Some("")`) or a program sent a notification with this
    /// text since the last call.
    pub fn take_attention(&mut self) -> Option<String> {
        self.attention.take()
    }

    /// Text a program copied (e.g. Neovim or tmux, also over SSH) since the
    /// last call.
    pub fn take_copied(&mut self) -> Option<Copy> {
        self.copied.take()
    }

    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    pub fn set_cell_px(&mut self, width: f64, height: f64) {
        self.cell_px = (width, height);
    }

    pub fn set_scrollback_limit(&mut self, lines: usize) {
        self.scrollback_limit = lines;
        while self.scrollback.len() > lines {
            self.scrollback.pop_front();
        }
    }

    /// Text between two (absolute line, column) positions, inclusive.
    pub fn text_between(&self, start: (i64, usize), end: (i64, usize)) -> String {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let mut out = String::new();
        for abs in start.0..=end.0 {
            let Some(row) = self.line(abs) else { continue };
            let from = if abs == start.0 { start.1 } else { 0 };
            let to = if abs == end.0 {
                end.1 + 1
            } else {
                row.cells.len()
            };
            out.push_str(&row.text(from, to));
            if abs != end.0 && !row.wrapped {
                out.push('\n');
            }
        }
        out
    }

    // -- resizing ---------------------------------------------------------

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        // Full-screen programs redraw themselves; normal output is re-wrapped.
        if cols != self.cols && self.primary.is_none() {
            self.reflow(cols);
        }
        for row in self.screen.iter_mut().chain(self.scrollback.iter_mut()) {
            row.resize(cols);
        }
        if let Some(primary) = &mut self.primary {
            for row in primary.iter_mut() {
                row.resize(cols);
            }
            resize_rows(primary, rows, cols, &mut 0, None);
        }
        let alt = self.primary.is_some();
        let mut cursor_row = self.cursor.row;
        let moved = resize_rows(
            &mut self.screen,
            rows,
            cols,
            &mut cursor_row,
            (!alt).then_some(&mut self.scrollback),
        );
        if !alt {
            self.scrolled_off += moved;
        }
        self.cursor.row = cursor_row.min(rows - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.cursor.pending_wrap = false;
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.tabs = default_tabs(cols);
        self.trim_scrollback();
    }

    /// Re-wraps the scrollback and screen for a new width: rows that were
    /// joined by autowrap become one line again, split at the new width.
    /// The cursor stays on the same character, and images and prompt marks
    /// move with their lines. The row count is left to `resize`.
    fn reflow(&mut self, cols: usize) {
        let old_scrollback = self.scrollback.len();
        let first_abs = self.scrolled_off - old_scrollback as i64;
        let mut rows: Vec<Row> = self
            .scrollback
            .drain(..)
            .chain(self.screen.drain(..))
            .collect();
        let cursor_row = old_scrollback + self.cursor.row;
        // With a pending wrap, the cursor is logically past the last column.
        let cursor_col = self.cursor.col + self.cursor.pending_wrap as usize;
        // Empty rows below the cursor are just unused screen.
        while rows.len() > cursor_row + 1 && rows.last().is_some_and(is_blank) {
            rows.pop();
        }

        // Join wrapped rows into lines, remembering where each row and the
        // cursor start within its line.
        struct Line {
            cells: Vec<Cell>,
        }
        let mut lines: Vec<Line> = Vec::new();
        let mut row_starts = Vec::with_capacity(rows.len()); // (line, offset)
        let mut cursor = (0, 0);
        let mut continuing = false;
        for (i, row) in rows.into_iter().enumerate() {
            if !continuing {
                lines.push(Line { cells: Vec::new() });
            }
            let line = lines.len() - 1;
            let offset = lines[line].cells.len();
            row_starts.push((line, offset));
            if i == cursor_row {
                cursor = (line, offset + cursor_col);
            }
            let mut cells = row.cells;
            if !row.wrapped {
                let content = cells
                    .iter()
                    .rposition(|c| !is_blank_cell(c))
                    .map_or(0, |i| i + 1);
                cells.truncate(content);
            }
            lines[line].cells.extend(cells);
            continuing = row.wrapped;
        }

        // Split every line at the new width.
        let mut new_rows: Vec<Row> = Vec::new();
        // Where an offset of a line lands: (row, col), for the mapped offsets.
        let place = |line: &Line, wanted: &[usize]| -> (Vec<Row>, Vec<(usize, usize)>) {
            let mut out = vec![Row::new(cols, Color::Default)];
            let mut col = 0;
            let mut positions = vec![(0, 0); wanted.len()];
            let record =
                |offset: usize, row: usize, col: usize, positions: &mut Vec<(usize, usize)>| {
                    for (i, w) in wanted.iter().enumerate() {
                        if *w == offset {
                            positions[i] = (row, col);
                        }
                    }
                };
            for (offset, cell) in line.cells.iter().enumerate() {
                // A wide character doesn't fit in the last column: next row.
                let needs = if cell.width == Width::Wide { 2 } else { 1 };
                if col + needs > cols {
                    out.last_mut().expect("a row").wrapped = true;
                    out.push(Row::new(cols, Color::Default));
                    col = 0;
                }
                let row = out.len() - 1;
                record(offset, row, col, &mut positions);
                if col < cols {
                    out[row].cells[col] = cell.clone();
                }
                col += 1;
            }
            // Offsets at or past the end (e.g. the cursor after the text).
            for (i, w) in wanted.iter().enumerate() {
                if *w >= line.cells.len() {
                    let extra = *w - line.cells.len() + col;
                    positions[i] = (out.len() - 1 + extra / cols, extra % cols);
                }
            }
            (out, positions)
        };
        let mut row_map = vec![0; row_starts.len()];
        let mut new_cursor = (0, 0);
        for (index, line) in lines.iter().enumerate() {
            let mut wanted: Vec<usize> = row_starts
                .iter()
                .filter(|(l, _)| *l == index)
                .map(|(_, offset)| *offset)
                .collect();
            let cursor_here = cursor.0 == index;
            if cursor_here {
                wanted.push(cursor.1);
            }
            let (mut out, positions) = place(line, &wanted);
            let base = new_rows.len();
            let mut position = positions.into_iter();
            for (old, (l, _)) in row_starts.iter().enumerate() {
                if *l == index {
                    row_map[old] = base + position.next().map_or(0, |(r, _)| r);
                }
            }
            if cursor_here && let Some((r, c)) = position.next() {
                new_cursor = (base + r, c);
            }
            new_rows.append(&mut out);
        }
        while new_rows.len() <= new_cursor.0 {
            new_rows.push(Row::new(cols, Color::Default));
        }

        // The last screenful (with the cursor on it) is the screen, the rest
        // goes to the scrollback.
        let start = new_rows.len().saturating_sub(self.rows).min(new_cursor.0);
        let screen: Vec<Row> = new_rows.drain(start..).take(self.rows).collect();
        self.scrollback = new_rows.into();
        self.screen = screen;
        while self.screen.len() < self.rows {
            self.screen.push(Row::new(cols, Color::Default));
        }
        self.cursor.row = new_cursor.0 - start;
        self.cursor.col = new_cursor.1.min(cols - 1);
        self.cursor.pending_wrap = false;
        self.scrolled_off = first_abs + self.scrollback.len() as i64;

        // Images and prompt marks follow their lines.
        let remap = |abs: i64| -> i64 {
            usize::try_from(abs - first_abs)
                .ok()
                .and_then(|old| row_map.get(old))
                .map_or(abs, |new| first_abs + *new as i64)
        };
        for placement in self.graphics.placements.iter_mut().filter(|p| !p.alt) {
            placement.abs_row = remap(placement.abs_row);
        }
        for command in self.prompts.iter_mut() {
            command.prompt = remap(command.prompt);
            for pos in [&mut command.input, &mut command.output, &mut command.end]
                .into_iter()
                .flatten()
            {
                pos.0 = remap(pos.0);
            }
        }
    }

    // -- basic operations ---------------------------------------------------

    fn blank_row(&self) -> Row {
        Row::new(self.cols, self.cursor.style.bg)
    }

    fn push_scrollback(&mut self, row: Row) {
        self.scrolled_off += 1;
        if self.scrollback_limit > 0 {
            self.scrollback.push_back(row);
            self.trim_scrollback();
        }
    }

    fn trim_scrollback(&mut self) {
        while self.scrollback.len() > self.scrollback_limit {
            self.scrollback.pop_front();
        }
        let oldest = self.scrolled_off - self.scrollback.len() as i64;
        self.graphics.forget_above(oldest);
        while self.prompts.front().is_some_and(|c| c.prompt < oldest) {
            self.prompts.pop_front();
        }
    }

    /// Scrolls the lines `top..=bottom` up by `n`, saving lines that leave
    /// the top of the primary screen.
    fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        for _ in 0..n {
            let row = self.screen.remove(top);
            if top == 0 && self.primary.is_none() {
                self.push_scrollback(row);
            }
            self.screen.insert(bottom, self.blank_row());
        }
    }

    fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        for _ in 0..n {
            self.screen.remove(bottom);
            self.screen.insert(top, self.blank_row());
        }
    }

    fn linefeed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.bottom {
            self.scroll_up(self.top, self.bottom, 1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.top {
            self.scroll_down(self.top, self.bottom, 1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let blank = Cell::blank(self.cursor.style.bg);
        if let Some(row) = self.screen.get_mut(row) {
            let to = to.min(row.cells.len());
            for cell in &mut row.cells[from.min(to)..to] {
                *cell = blank.clone();
            }
        }
    }

    fn erase_rows(&mut self, from: usize, to: usize) {
        for row in from..to.min(self.rows) {
            self.screen[row] = self.blank_row();
        }
    }

    fn set_cursor(&mut self, row: usize, col: usize) {
        let (min_row, max_row) = if self.cursor.origin {
            (self.top, self.bottom)
        } else {
            (0, self.rows - 1)
        };
        let row = if self.cursor.origin {
            row + self.top
        } else {
            row
        };
        self.cursor.row = row.clamp(min_row, max_row);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn move_rows(&mut self, delta: isize) {
        // Movement stops at the scroll region when starting inside it.
        let (min, max) = if (self.top..=self.bottom).contains(&self.cursor.row) {
            (self.top, self.bottom)
        } else {
            (0, self.rows - 1)
        };
        let row = (self.cursor.row as isize + delta).clamp(min as isize, max as isize);
        self.cursor.row = row as usize;
        self.cursor.pending_wrap = false;
    }

    fn move_cols(&mut self, delta: isize) {
        let col = (self.cursor.col as isize + delta).clamp(0, self.cols as isize - 1);
        self.cursor.col = col as usize;
        self.cursor.pending_wrap = false;
    }

    fn put_char(&mut self, c: char) {
        let c = if self.cursor.line_drawing[self.cursor.charset] {
            line_drawing(c)
        } else {
            c
        };
        if self.join_cluster(c) {
            return;
        }
        let width = c.width().unwrap_or(0);
        if width == 0 {
            self.add_combining(c);
            return;
        }
        if self.cursor.pending_wrap && self.modes.autowrap {
            self.screen[self.cursor.row].wrapped = true;
            self.cursor.col = 0;
            self.linefeed();
        }
        if width == 2 && self.cursor.col + 1 >= self.cols {
            if !self.modes.autowrap || self.cols < 2 {
                return;
            }
            self.screen[self.cursor.row].wrapped = true;
            self.cursor.col = 0;
            self.linefeed();
        }
        let (row, col) = (self.cursor.row, self.cursor.col);
        if self.modes.insert {
            let cells = &mut self.screen[row].cells;
            for _ in 0..width {
                cells.insert(col, Cell::default());
                cells.pop();
            }
        }
        self.clear_wide_partner(row, col);
        if width == 2 {
            self.clear_wide_partner(row, col + 1);
        }
        let (style, link) = (self.cursor.style, self.cursor.link);
        let cells = &mut self.screen[row].cells;
        cells[col] = Cell {
            ch: c,
            style,
            width: if width == 2 {
                Width::Wide
            } else {
                Width::Narrow
            },
            extra: None,
            link,
        };
        if width == 2 {
            cells[col + 1] = Cell {
                ch: ' ',
                style,
                width: Width::Spacer,
                extra: None,
                link,
            };
        }
        self.last_char = Some(c);
        if col + width >= self.cols {
            self.cursor.col = self.cols - 1;
            self.cursor.pending_wrap = self.modes.autowrap;
        } else {
            self.cursor.col = col + width;
        }
    }

    /// The saved cursor may come from before a resize, so it's clamped.
    fn restore_cursor(&mut self) {
        self.cursor = self.saved_cursor;
        self.cursor.row = self.cursor.row.min(self.rows - 1);
        self.cursor.col = self.cursor.col.min(self.cols - 1);
    }

    /// Before overwriting a cell, blanks the other half of a wide character.
    fn clear_wide_partner(&mut self, row: usize, col: usize) {
        let cells = &mut self.screen[row].cells;
        match cells.get(col).map(|c| c.width) {
            Some(Width::Spacer) if col > 0 => cells[col - 1] = Cell::default(),
            Some(Width::Wide) if col + 1 < cells.len() => cells[col + 1] = Cell::default(),
            _ => {}
        }
    }

    /// The cell the last character went into: where combining characters
    /// attach.
    fn last_cell(&self) -> usize {
        let col = if self.cursor.pending_wrap {
            self.cursor.col
        } else {
            self.cursor.col.saturating_sub(1)
        };
        match self.screen[self.cursor.row].cells.get(col).map(|c| c.width) {
            Some(Width::Spacer) => col.saturating_sub(1),
            _ => col,
        }
    }

    /// Emoji made of several characters take one wide cell, as programs
    /// that measure graphemes (Ink, i.e. Claude Code and OpenCode, fish)
    /// expect: 👨‍👩‍👧 (joined by ZWJ), 👍🏽 (skin tone) and 🇩🇪 (two regional
    /// indicators). Returns whether `c` joined the previous cell.
    fn join_cluster(&mut self, c: char) -> bool {
        if self.cursor.col == 0 && !self.cursor.pending_wrap {
            return false;
        }
        let col = self.last_cell();
        let Some(cell) = self.screen[self.cursor.row].cells.get(col) else {
            return false;
        };
        let after_zwj = cell
            .extra
            .as_deref()
            .is_some_and(|e| e.ends_with('\u{200D}'));
        let skin_tone = ('\u{1F3FB}'..='\u{1F3FF}').contains(&c) && cell.width == Width::Wide;
        let flag = is_regional_indicator(c)
            && is_regional_indicator(cell.ch)
            && cell.extra.is_none()
            && cell.width == Width::Narrow;
        if !(after_zwj || skin_tone || flag) {
            return false;
        }
        self.add_combining(c);
        if flag {
            self.widen(col);
        }
        true
    }

    /// Makes the narrow cell `col` of the cursor row wide, e.g. ❤ followed
    /// by VS16 (emoji presentation). At the end of the line it stays narrow.
    fn widen(&mut self, col: usize) {
        let row = self.cursor.row;
        if self.screen[row].cells[col].width != Width::Narrow || col + 1 >= self.cols {
            return;
        }
        self.clear_wide_partner(row, col + 1);
        let cells = &mut self.screen[row].cells;
        cells[col].width = Width::Wide;
        cells[col + 1] = Cell {
            ch: ' ',
            width: Width::Spacer,
            ..cells[col].clone()
        };
        cells[col + 1].extra = None;
        if col + 2 >= self.cols {
            self.cursor.col = self.cols - 1;
            self.cursor.pending_wrap = self.modes.autowrap;
        } else {
            self.cursor.col = col + 2;
        }
    }

    fn add_combining(&mut self, c: char) {
        // One grapheme must not evade the screen/scrollback memory bounds.
        const MAX_COMBINING_BYTES: usize = 256;
        let col = self.last_cell();
        let Some(cell) = self.screen[self.cursor.row].cells.get_mut(col) else {
            return;
        };
        if cell.extra.as_ref().map_or(0, |s| s.len()) + c.len_utf8() > MAX_COMBINING_BYTES {
            return;
        }
        let mut extra = cell.extra.take().map(String::from).unwrap_or_default();
        extra.push(c);
        cell.extra = Some(extra.into_boxed_str());
        // VS16: shown as an emoji, which is wide.
        if c == '\u{FE0F}' && (self.cursor.col > 0 || self.cursor.pending_wrap) {
            self.widen(col);
        }
    }

    fn enter_alt_screen(&mut self, save_cursor: bool) {
        if self.primary.is_some() {
            return;
        }
        if save_cursor {
            self.saved_cursor = self.cursor;
        }
        let alt = (0..self.rows)
            .map(|_| Row::new(self.cols, Color::Default))
            .collect();
        self.primary = Some(std::mem::replace(&mut self.screen, alt));
    }

    fn leave_alt_screen(&mut self, restore_cursor: bool) {
        if let Some(primary) = self.primary.take() {
            self.screen = primary;
            self.keyboard.clear_alt();
            self.graphics.clear_screen(true, None);
            if restore_cursor {
                self.restore_cursor();
            }
        }
    }

    fn reset(&mut self) {
        let limit = self.scrollback_limit;
        let (fg, bg, cell_px) = (self.report_fg, self.report_bg, self.cell_px);
        let palette = self.report_palette;
        let scrolled_off = self.scrolled_off;
        let had_colors = self.colors != Colors::default();
        *self = Self::new(self.cols, self.rows, limit);
        self.report_fg = fg;
        self.report_bg = bg;
        self.report_palette = palette;
        self.colors_changed = had_colors;
        self.cell_px = cell_px;
        self.scrolled_off = scrolled_off;
    }

    fn respond(&mut self, text: &str) {
        self.responses.extend_from_slice(text.as_bytes());
    }

    // -- APC (Kitty graphics) -----------------------------------------------

    pub fn apc(&mut self, data: &[u8]) {
        let Some(command) = data.strip_prefix(b"G") else {
            return;
        };
        let context = PlaceContext {
            abs_row: self.scrolled_off + self.cursor.row as i64,
            col: self.cursor.col,
            cell_px: self.cell_px,
            alt: self.alt_active(),
        };
        let outcome = self.graphics.command(command, context);
        if let Some(reply) = outcome.reply {
            self.respond(&reply);
        }
        if let Some((cols, rows)) = outcome.move_cursor {
            // Like kitty: to the column after the image, on its last row.
            for _ in 1..rows {
                self.linefeed();
            }
            let col = self.cursor.col + cols;
            if col >= self.cols {
                self.cursor.col = self.cols - 1;
                self.cursor.pending_wrap = self.modes.autowrap;
            } else {
                self.cursor.col = col;
            }
        }
    }

    /// Shows a Sixel image at the cursor; the cursor goes to the image's
    /// last row, in the column it starts in (as in xterm).
    fn show_sixel(&mut self, width: u32, height: u32, rgba: &[u8]) {
        let context = PlaceContext {
            abs_row: self.scrolled_off + self.cursor.row as i64,
            col: self.cursor.col,
            cell_px: self.cell_px,
            alt: self.alt_active(),
        };
        let (_, rows) = self.graphics.show_pixels(width, height, rgba, context);
        let col = self.cursor.col;
        for _ in 1..rows {
            self.linefeed();
        }
        self.cursor.col = col;
        self.cursor.pending_wrap = false;
    }

    // -- control sequences --------------------------------------------------

    fn sgr(&mut self, params: &[Vec<u16>]) {
        if params.is_empty() {
            self.cursor.style = Style::default();
            return;
        }
        let style = &mut self.cursor.style;
        let mut i = 0;
        while i < params.len() {
            let param = &params[i];
            let code = param.first().copied().unwrap_or(0);
            match code {
                0 => *style = Style::default(),
                1 => style.attrs |= attr::BOLD,
                2 => style.attrs |= attr::DIM,
                3 => style.attrs |= attr::ITALIC,
                // 4 or 4:1 single, 4:2 double, 4:3 curly, 4:4 dotted,
                // 4:5 dashed, 4:0 none.
                4 => {
                    style.attrs &= !attr::ANY_UNDERLINE;
                    style.attrs |= match param.get(1).copied().unwrap_or(1) {
                        0 => 0,
                        2 => attr::UNDERLINE | attr::DOUBLE_UNDERLINE,
                        3 => attr::UNDERLINE | attr::CURLY_UNDERLINE,
                        4 => attr::UNDERLINE | attr::DOTTED_UNDERLINE,
                        5 => attr::UNDERLINE | attr::DASHED_UNDERLINE,
                        _ => attr::UNDERLINE,
                    };
                }
                5 | 6 => style.attrs |= attr::BLINK,
                7 => style.attrs |= attr::INVERSE,
                8 => style.attrs |= attr::HIDDEN,
                9 => style.attrs |= attr::STRIKE,
                21 => {
                    style.attrs &= !attr::ANY_UNDERLINE;
                    style.attrs |= attr::UNDERLINE | attr::DOUBLE_UNDERLINE;
                }
                22 => style.attrs &= !(attr::BOLD | attr::DIM),
                23 => style.attrs &= !attr::ITALIC,
                24 => style.attrs &= !attr::ANY_UNDERLINE,
                25 => style.attrs &= !attr::BLINK,
                27 => style.attrs &= !attr::INVERSE,
                28 => style.attrs &= !attr::HIDDEN,
                29 => style.attrs &= !attr::STRIKE,
                30..=37 => style.fg = Color::Indexed((code - 30) as u8),
                39 => style.fg = Color::Default,
                40..=47 => style.bg = Color::Indexed((code - 40) as u8),
                49 => style.bg = Color::Default,
                90..=97 => style.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => style.bg = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 | 58 => {
                    let (color, used) = extended_color(params, i);
                    i += used;
                    match (code, color) {
                        (38, Some(color)) => style.fg = color,
                        (48, Some(color)) => style.bg = color,
                        (58, Some(color)) => style.underline = color,
                        _ => {}
                    }
                }
                59 => style.underline = Color::Default,
                _ => {}
            }
            i += 1;
        }
    }

    fn set_mode(&mut self, params: &[Vec<u16>], private: bool, on: bool) {
        for param in params {
            let mode = param.first().copied().unwrap_or(0);
            match (private, mode) {
                (false, 4) => self.modes.insert = on,
                (true, 1) => self.modes.app_cursor = on,
                (true, 6) => {
                    self.cursor.origin = on;
                    self.set_cursor(0, 0);
                }
                (true, 7) => self.modes.autowrap = on,
                (true, 25) => self.modes.cursor_visible = on,
                (true, 47) | (true, 1047) => {
                    if on {
                        self.enter_alt_screen(false);
                    } else {
                        self.leave_alt_screen(false);
                    }
                }
                (true, 1049) => {
                    if on {
                        self.enter_alt_screen(true);
                    } else {
                        self.leave_alt_screen(true);
                    }
                }
                // One of these at a time, as in xterm.
                (true, 1000 | 1002 | 1003) => {
                    self.modes.mouse_reporting = on;
                    self.modes.mouse_motion = match (on, mode) {
                        (true, 1002) => MouseMotion::Drags,
                        (true, 1003) => MouseMotion::All,
                        _ => MouseMotion::None,
                    };
                }
                (true, 1004) => self.modes.focus_events = on,
                (true, 1006) => self.modes.sgr_mouse = on,
                (true, 2004) => self.modes.bracketed_paste = on,
                (true, 2026) => self.modes.synchronized = on,
                _ => {}
            }
        }
    }

    /// For DECRQM: 1 = set, 2 = reset, 0 = not recognized.
    fn mode_status(&self, private: bool, mode: u16) -> u8 {
        let value = match (private, mode) {
            (false, 4) => self.modes.insert,
            (true, 1) => self.modes.app_cursor,
            (true, 6) => self.cursor.origin,
            (true, 7) => self.modes.autowrap,
            (true, 25) => self.modes.cursor_visible,
            (true, 47 | 1047 | 1049) => self.alt_active(),
            (true, 1000) => {
                self.modes.mouse_reporting && self.modes.mouse_motion == MouseMotion::None
            }
            (true, 1002) => self.modes.mouse_motion == MouseMotion::Drags,
            (true, 1003) => self.modes.mouse_motion == MouseMotion::All,
            (true, 1004) => self.modes.focus_events,
            (true, 1006) => self.modes.sgr_mouse,
            (true, 2004) => self.modes.bracketed_paste,
            (true, 2026) => self.modes.synchronized,
            // Grapheme clusters: always on (3 = permanently set).
            (true, 2027) => return 3,
            _ => return 0,
        };
        if value { 1 } else { 2 }
    }

    fn erase_display(&mut self, mode: u16) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        match mode {
            0 => {
                self.erase_cells(row, col, self.cols);
                self.erase_rows(row + 1, self.rows);
            }
            1 => {
                self.erase_rows(0, row);
                self.erase_cells(row, 0, col + 1);
            }
            2 => {
                self.erase_rows(0, self.rows);
                let alt = self.alt_active();
                self.graphics.clear_screen(alt, Some(self.scrolled_off));
            }
            3 => {
                self.scrollback.clear();
                let oldest = self.scrolled_off;
                self.graphics.forget_above(oldest);
            }
            _ => {}
        }
    }
}

fn is_blank_cell(cell: &Cell) -> bool {
    cell.ch == ' ' && cell.extra.is_none() && cell.style == Style::default() && cell.link == 0
}

fn is_blank(row: &Row) -> bool {
    !row.wrapped && row.cells.iter().all(is_blank_cell)
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c % 8 == 0 && c > 0).collect()
}

/// Grows or shrinks `screen` to `rows` lines, keeping the cursor line visible.
/// Lines removed from the top go to `scrollback` (when given) and lines are
/// taken back from it when growing. Returns how many lines moved into the
/// scrollback (negative when taken back).
fn resize_rows(
    screen: &mut Vec<Row>,
    rows: usize,
    cols: usize,
    cursor_row: &mut usize,
    mut scrollback: Option<&mut VecDeque<Row>>,
) -> i64 {
    let mut moved = 0;
    while screen.len() > rows {
        if *cursor_row > 0 {
            let row = screen.remove(0);
            if let Some(scrollback) = scrollback.as_deref_mut() {
                scrollback.push_back(row);
            }
            moved += 1;
            *cursor_row -= 1;
        } else {
            screen.pop();
        }
    }
    while screen.len() < rows {
        match scrollback.as_deref_mut().and_then(|s| s.pop_back()) {
            Some(row) => {
                screen.insert(0, row);
                moved -= 1;
                *cursor_row += 1;
            }
            None => screen.push(Row::new(cols, Color::Default)),
        }
    }
    moved
}

/// Parses `38;5;n`, `38;2;r;g;b` and their colon forms starting at
/// `params[i]`. Returns the color and how many extra params were used.
fn extended_color(params: &[Vec<u16>], i: usize) -> (Option<Color>, usize) {
    let param = &params[i];
    if param.len() > 1 {
        // Colon form: 38:5:n or 38:2:[colorspace:]r:g:b
        let color = match param[1] {
            5 => param.get(2).map(|n| Color::Indexed(*n as u8)),
            2 => {
                let rgb = if param.len() >= 6 {
                    &param[3..6]
                } else {
                    &param[2..]
                };
                (rgb.len() >= 3).then(|| Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
            }
            _ => None,
        };
        return (color, 0);
    }
    let next = |k: usize| params.get(i + k).and_then(|p| p.first().copied());
    match next(1) {
        Some(5) => (next(2).map(|n| Color::Indexed(n as u8)), 2),
        Some(2) => match (next(2), next(3), next(4)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(r as u8, g as u8, b as u8)), 4),
            _ => (None, 4),
        },
        _ => (None, 0),
    }
}

/// The DEC special graphics character set, used by TUIs for box drawing.
fn line_drawing(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        c => c,
    }
}

impl Perform for Term {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    // DCS sequences: only Sixel images (`ESC P … q`).
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // P2 1 leaves what isn't painted transparent; otherwise it's the
        // background color.
        let transparent = params.iter().nth(1).and_then(|p| p.first()) == Some(&1);
        let background = self.colors.bg.unwrap_or(self.report_bg);
        self.sixel = (action == 'q' && intermediates.is_empty() && !ignore)
            .then(|| Box::new(sixel::Decoder::new((!transparent).then_some(background))));
    }

    fn put(&mut self, byte: u8) {
        if let Some(decoder) = &mut self.sixel {
            decoder.put(byte);
        }
    }

    fn unhook(&mut self) {
        if let Some(decoder) = self.sixel.take()
            && let Some((width, height, rgba)) = decoder.finish()
        {
            self.show_sixel(width, height, &rgba);
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.move_cols(-1),
            0x09 => {
                let next = (self.cursor.col + 1..self.cols).find(|&c| self.tabs[c]);
                self.cursor.col = next.unwrap_or(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            0x0e => self.cursor.charset = 1,
            0x0f => self.cursor.charset = 0,
            0x07 if self.attention.is_none() => self.attention = Some(String::new()),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let params: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();
        let raw = |i: usize| params.get(i).and_then(|p| p.first().copied()).unwrap_or(0);
        // Most parameters treat 0 as "default", which is 1.
        let n = |i: usize| raw(i).max(1) as usize;
        let private = intermediates.first() == Some(&b'?');
        match (intermediates, action) {
            ([], 'A') => self.move_rows(-(n(0) as isize)),
            ([], 'B') | ([], 'e') => self.move_rows(n(0) as isize),
            ([], 'C') | ([], 'a') => self.move_cols(n(0) as isize),
            ([], 'D') => self.move_cols(-(n(0) as isize)),
            ([], 'E') => {
                self.move_rows(n(0) as isize);
                self.cursor.col = 0;
            }
            ([], 'F') => {
                self.move_rows(-(n(0) as isize));
                self.cursor.col = 0;
            }
            ([], 'G') | ([], '`') => {
                self.cursor.col = (n(0) - 1).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            ([], 'H') | ([], 'f') => self.set_cursor(n(0) - 1, n(1) - 1),
            ([], 'd') => {
                let col = self.cursor.col;
                self.set_cursor(n(0) - 1, col);
            }
            ([], 'I') => {
                for _ in 0..n(0) {
                    self.execute(0x09);
                }
            }
            ([], 'Z') => {
                for _ in 0..n(0) {
                    let prev = (1..self.cursor.col).rev().find(|&c| self.tabs[c]);
                    self.cursor.col = prev.unwrap_or(0);
                }
            }
            ([], 'J') | ([b'?'], 'J') => self.erase_display(raw(0)),
            ([], 'K') | ([b'?'], 'K') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                match raw(0) {
                    0 => self.erase_cells(row, col, self.cols),
                    1 => self.erase_cells(row, 0, col + 1),
                    2 => self.erase_cells(row, 0, self.cols),
                    _ => {}
                }
            }
            ([], 'L') | ([], 'M') => {
                let row = self.cursor.row;
                if (self.top..=self.bottom).contains(&row) {
                    if action == 'L' {
                        self.scroll_down(row, self.bottom, n(0));
                    } else {
                        self.scroll_up_no_save(row, n(0));
                    }
                    self.cursor.col = 0;
                    self.cursor.pending_wrap = false;
                }
            }
            ([], '@') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                let blank = Cell::blank(self.cursor.style.bg);
                let cells = &mut self.screen[row].cells;
                for _ in 0..n(0).min(self.cols - col) {
                    cells.insert(col, blank.clone());
                    cells.pop();
                }
            }
            ([], 'P') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                let blank = Cell::blank(self.cursor.style.bg);
                let cells = &mut self.screen[row].cells;
                for _ in 0..n(0).min(self.cols - col) {
                    cells.remove(col);
                    cells.push(blank.clone());
                }
            }
            ([], 'X') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                self.erase_cells(row, col, col + n(0));
            }
            ([], 'S') => self.scroll_up(self.top, self.bottom, n(0)),
            ([], 'T') if params.len() <= 1 => self.scroll_down(self.top, self.bottom, n(0)),
            ([], 'b') => {
                if let Some(c) = self.last_char {
                    for _ in 0..n(0).min(65_535) {
                        self.put_char(c);
                    }
                }
            }
            ([], 'm') => self.sgr(&params),
            // modifyOtherKeys: CSI > 4 ; level m
            ([b'>'], 'm') if raw(0) == 4 => self.keyboard.modify_other_keys = raw(1).min(2) as u8,
            ([b'>'], 'n') if raw(0) == 4 => self.keyboard.modify_other_keys = 0,
            // Kitty keyboard protocol: query, push, pop, set.
            ([b'?'], 'u') => {
                let flags = self.keyboard.kitty_flags(self.alt_active());
                self.respond(&format!("\x1b[?{flags}u"));
            }
            ([b'>'], 'u') => {
                let alt = self.alt_active();
                self.keyboard.push(alt, raw(0) as u8);
            }
            ([b'<'], 'u') => {
                let alt = self.alt_active();
                self.keyboard.pop(alt, n(0));
            }
            ([b'='], 'u') => {
                let alt = self.alt_active();
                self.keyboard.set(alt, raw(0) as u8, raw(1));
            }
            // DECRQM: is a mode set?
            ([b'?', b'$'], 'p') | ([b'$'], 'p') => {
                let mode = raw(0);
                let status = self.mode_status(private, mode);
                let marker = if private { "?" } else { "" };
                self.respond(&format!("\x1b[{marker}{mode};{status}$y"));
            }
            ([], 'r') => {
                let top = n(0) - 1;
                let bottom = if raw(1) == 0 { self.rows } else { n(1) }.min(self.rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.set_cursor(0, 0);
                }
            }
            ([b'?'], 'h') | ([], 'h') => self.set_mode(&params, private, true),
            ([b'?'], 'l') | ([], 'l') => self.set_mode(&params, private, false),
            ([], 'n') => match raw(0) {
                5 => self.respond("\x1b[0n"),
                6 => {
                    let row = if self.cursor.origin {
                        self.cursor.row - self.top
                    } else {
                        self.cursor.row
                    };
                    let reply = format!("\x1b[{};{}R", row + 1, self.cursor.col + 1);
                    self.respond(&reply);
                }
                _ => {}
            },
            // A VT220 with Sixel graphics (4) and ANSI colors (22).
            ([], 'c') if raw(0) == 0 => self.respond("\x1b[?62;4;22c"),
            ([b'>'], 'c') => self.respond("\x1b[>1;10;0c"),
            ([], 's') => self.saved_cursor = self.cursor,
            ([], 'u') => self.restore_cursor(),
            ([], 't') => {
                let (cw, ch) = self.cell_px;
                let reply = match raw(0) {
                    14 => Some(format!(
                        "\x1b[4;{};{}t",
                        (ch * self.rows as f64) as u32,
                        (cw * self.cols as f64) as u32
                    )),
                    16 => Some(format!("\x1b[6;{};{}t", ch as u32, cw as u32)),
                    18 => Some(format!("\x1b[8;{};{}t", self.rows, self.cols)),
                    _ => None,
                };
                if let Some(reply) = reply {
                    self.respond(&reply);
                }
            }
            // DECSCUSR: odd numbers blink, even ones don't, 0 is the default.
            ([b' '], 'q') => {
                let style = raw(0);
                self.modes.cursor_shape = match style {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
                self.modes.cursor_blink = match style {
                    0 => None,
                    1..=6 => Some(style % 2 == 1),
                    _ => self.modes.cursor_blink,
                };
            }
            ([], 'g') => match raw(0) {
                0 => {
                    let col = self.cursor.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            ([b'!'], 'p') => {
                self.modes = Modes::default();
                self.cursor.style = Style::default();
                self.top = 0;
                self.bottom = self.rows - 1;
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'7') => self.saved_cursor = self.cursor,
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.cursor.col = 0;
                self.linefeed();
            }
            ([], b'H') => {
                let col = self.cursor.col;
                self.tabs[col] = true;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.reset(),
            ([b'('], b'0') => self.cursor.line_drawing[0] = true,
            ([b'('], _) => self.cursor.line_drawing[0] = false,
            ([b')'], b'0') => self.cursor.line_drawing[1] = true,
            ([b')'], _) => self.cursor.line_drawing[1] = false,
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(code) = params.first() else { return };
        match *code {
            b"0" | b"2" => {
                let title: String = join_params(&params[1..])
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_TITLE_CHARS)
                    .collect();
                self.title = Some(title);
            }
            // OSC 133: shell integration (fish sends these on its own).
            b"133" if self.primary.is_none() => {
                let mark = params.get(1).and_then(|p| p.first()).copied();
                self.shell_mark(mark, params.get(2..).unwrap_or_default());
            }
            // OSC 7 ; file://host/path: the shell's current folder.
            b"7" => {
                let url = join_params(&params[1..]);
                if let Some(rest) = url.strip_prefix("file://")
                    && let Some(slash) = rest.find('/')
                    && url.len() <= MAX_URL_LEN
                {
                    let (host, path) = rest.split_at(slash);
                    self.reported_cwd = Some((host.to_string(), percent_decode(path)));
                }
            }
            // OSC 9 ; text and OSC 777 ; notify ; title ; body: notifications.
            // OSC 9 ; 4 ; … reports progress instead.
            b"9" if params.get(1) == Some(&&b"4"[..]) => {
                let number = |i: usize| {
                    params
                        .get(i)
                        .and_then(|p| std::str::from_utf8(p).ok())
                        .and_then(|p| p.parse::<u32>().ok())
                };
                let percent = number(3).unwrap_or(0).min(100) as u8;
                let progress = match number(2) {
                    Some(1) => Some(Progress::Normal(percent)),
                    Some(2) => Some(Progress::Error(percent)),
                    Some(3) => Some(Progress::Indeterminate),
                    Some(4) => Some(Progress::Paused(percent)),
                    _ => None,
                };
                self.set_progress(progress);
            }
            b"9" => self.notify(join_params(&params[1..])),
            b"777" if params.get(1) == Some(&&b"notify"[..]) => {
                let parts: Vec<String> = params[2..]
                    .iter()
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .filter(|p| !p.is_empty())
                    .collect();
                self.notify(parts.join(": "));
            }
            // OSC 8 ; params ; URI starts a link, an empty URI ends it.
            b"8" => {
                let uri = join_params(params.get(2..).unwrap_or_default());
                self.cursor.link = if uri.is_empty() || uri.len() > MAX_URL_LEN {
                    0
                } else {
                    self.link_id(uri)
                };
            }
            // OSC 52 ; targets ; base64: copy. Reading the clipboard (`?`)
            // isn't allowed: programs can't see what you copied elsewhere.
            b"52" if params.len() >= 3 => {
                if let Some(copy) = Copy::parse(params[1], params[2]) {
                    self.copied = Some(copy);
                }
            }
            // OSC 4 ; index ; color (or `?`), as often as wanted: the palette.
            b"4" => {
                for pair in params[1..].chunks(2) {
                    let index = std::str::from_utf8(pair[0])
                        .ok()
                        .and_then(|i| i.parse::<u8>().ok());
                    let (Some(index), Some(spec)) = (index, pair.get(1)) else {
                        break;
                    };
                    if *spec == b"?" {
                        let color = self.colors.palette[index as usize]
                            .unwrap_or(self.report_palette[index as usize]);
                        self.report_color(&format!("4;{index}"), color);
                    } else if let Some(color) = parse_color(spec) {
                        self.colors.palette[index as usize] = Some(color);
                        self.colors_changed = true;
                    }
                }
            }
            // OSC 104 (; index …): back to the theme's palette colors.
            b"104" => {
                let indices: Vec<u8> = params[1..]
                    .iter()
                    .filter_map(|p| std::str::from_utf8(p).ok()?.parse().ok())
                    .collect();
                if params.len() == 1 || params[1].is_empty() {
                    self.colors.palette = [None; 256];
                }
                for index in indices {
                    self.colors.palette[index as usize] = None;
                }
                self.colors_changed = true;
            }
            // OSC 10/11/12 ; color (or `?`): text, background, cursor.
            b"10" | b"11" | b"12" => {
                let Some(spec) = params.get(1) else { return };
                let (slot, base) = match *code {
                    b"10" => (&mut self.colors.fg, self.report_fg),
                    b"11" => (&mut self.colors.bg, self.report_bg),
                    _ => (&mut self.colors.cursor, self.report_fg),
                };
                if *spec == b"?" {
                    let color = slot.unwrap_or(base);
                    let code = String::from_utf8_lossy(code).into_owned();
                    self.report_color(&code, color);
                } else if let Some(color) = parse_color(spec) {
                    *slot = Some(color);
                    self.colors_changed = true;
                }
            }
            // OSC 110/111/112: back to the theme's.
            b"110" | b"111" | b"112" => {
                match *code {
                    b"110" => self.colors.fg = None,
                    b"111" => self.colors.bg = None,
                    _ => self.colors.cursor = None,
                }
                self.colors_changed = true;
            }
            _ => {}
        }
    }
}

impl Term {
    /// A shell integration mark (OSC 133 A, B, C or D) at the cursor, with
    /// what comes after it: D's exit status, C's command line (fish's
    /// `cmdline_url=`, percent-encoded, or `cmdline=`).
    fn shell_mark(&mut self, mark: Option<u8>, rest: &[&[u8]]) {
        let exit = rest
            .first()
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|p| p.parse().ok());
        let given = rest.iter().find_map(|p| {
            let p = String::from_utf8_lossy(p);
            if let Some(url) = p.strip_prefix("cmdline_url=") {
                Some(percent_decode(url))
            } else {
                p.strip_prefix("cmdline=").map(str::to_string)
            }
        });
        let here = (self.scrolled_off + self.cursor.row as i64, self.cursor.col);
        match mark {
            Some(b'A') => {
                // A prompt without D before it: the last command ended here.
                self.end_command(here, None);
                if self.prompts.back().map(|c| c.prompt) != Some(here.0) {
                    self.prompts.push_back(Command::new(here.0));
                    if self.prompts.len() > MAX_PROMPTS {
                        self.prompts.pop_front();
                    }
                }
            }
            Some(b'B') => {
                if let Some(last) = self.prompts.back_mut() {
                    last.input = Some(here);
                }
            }
            Some(b'C') => {
                let typed = given
                    .or_else(|| {
                        let input = self.prompts.back()?.input?;
                        Some(self.text_between(input, here))
                    })
                    .map(|text| {
                        let text = text.trim().replace(|c: char| c.is_control(), " ");
                        text.chars().take(MAX_COMMAND_CHARS).collect::<String>()
                    });
                if let Some(last) = self.prompts.back_mut()
                    && last.output.is_none()
                {
                    last.output = Some(here);
                    last.command = typed.filter(|t| !t.is_empty());
                    last.started = Some(Instant::now());
                }
            }
            Some(b'D') => self.end_command(here, Some(exit.unwrap_or(0))),
            _ => {}
        }
    }

    /// Ends the running command (one with output and no end yet).
    fn end_command(&mut self, at: (i64, usize), exit: Option<i32>) {
        let Some(last) = self.prompts.back_mut() else {
            return;
        };
        if last.output.is_none() || last.end.is_some() {
            return;
        }
        last.end = Some(at);
        last.exit = exit;
        self.finished = Some(Finished {
            command: last.command.clone(),
            exit,
            took: last.started.map_or(Duration::ZERO, |s| s.elapsed()),
        });
        // Whatever it reported is over now.
        self.set_progress(None);
    }

    fn report_color(&mut self, code: &str, (r, g, b): (u8, u8, u8)) {
        let reply = format!("\x1b]{code};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\");
        self.respond(&reply);
    }

    /// The colors the program set, if they changed since the last call.
    pub fn take_colors(&mut self) -> Option<Colors> {
        std::mem::take(&mut self.colors_changed).then_some(self.colors)
    }

    pub fn colors(&self) -> &Colors {
        &self.colors
    }

    fn set_progress(&mut self, progress: Option<Progress>) {
        if self.progress != progress {
            self.progress = progress;
            self.progress_changed = true;
        }
    }

    /// The program's progress, if it changed since the last call.
    pub fn take_progress(&mut self) -> Option<Option<Progress>> {
        std::mem::take(&mut self.progress_changed).then_some(self.progress)
    }

    /// The folder the shell reported last (OSC 7), as (host, path).
    pub fn reported_cwd(&self) -> Option<&(String, String)> {
        self.reported_cwd.as_ref()
    }

    /// A command that finished since the last call.
    pub fn take_finished(&mut self) -> Option<Finished> {
        self.finished.take()
    }

    /// The commands the shell marked, oldest first.
    pub fn commands(&self) -> impl DoubleEndedIterator<Item = &Command> + '_ {
        self.prompts.iter()
    }

    /// The output of the last command that printed something, or of the
    /// one still running.
    pub fn last_output(&self) -> Option<String> {
        let cursor = (self.scrolled_off + self.cursor.row as i64, self.cursor.col);
        self.prompts.iter().rev().find_map(|command| {
            let start = command.output?;
            let end = command.end.unwrap_or(cursor);
            // Up to just before the end: it's where the next prompt starts.
            let end = match end {
                (line, 0) => (line - 1, usize::MAX / 2),
                (line, col) => (line, col - 1),
            };
            (end >= start)
                .then(|| self.text_between(start, end))
                .map(|text| text.trim_end().to_string())
                .filter(|text| !text.is_empty())
        })
    }

    fn notify(&mut self, text: String) {
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_TITLE_CHARS)
            .collect();
        self.attention = Some(text);
    }
}

/// Text a program copied, and where to (OSC 52).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    pub text: String,
    pub clipboard: bool,
    /// The primary selection (middle-click paste).
    pub primary: bool,
}

impl Copy {
    fn parse(targets: &[u8], data: &[u8]) -> Option<Self> {
        use base64::Engine;
        use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
        if data == b"?" || data.len() > MAX_CLIPBOARD_BYTES.div_ceil(3) * 4 {
            return None;
        }
        let engine = GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
        );
        let text = String::from_utf8(engine.decode(data).ok()?).ok()?;
        if text.len() > MAX_CLIPBOARD_BYTES || text.contains('\0') {
            return None;
        }
        // No targets means the clipboard; `s` (selection) is it too.
        let primary = targets.contains(&b'p');
        let clipboard = targets.is_empty() || targets.iter().any(|t| matches!(t, b'c' | b's'));
        (clipboard || primary).then_some(Self {
            text,
            clipboard,
            primary,
        })
    }
}

/// The SGR sequence that sets exactly `style`.
fn sgr_sequence(style: &Style) -> String {
    let mut codes = vec!["0".to_string()];
    for (bit, code) in [
        (attr::BOLD, "1"),
        (attr::DIM, "2"),
        (attr::ITALIC, "3"),
        (attr::BLINK, "5"),
        (attr::INVERSE, "7"),
        (attr::HIDDEN, "8"),
        (attr::STRIKE, "9"),
    ] {
        if style.attrs & bit != 0 {
            codes.push(code.into());
        }
    }
    if style.attrs & attr::UNDERLINE != 0 {
        let kind = [
            (attr::DOUBLE_UNDERLINE, "4:2"),
            (attr::CURLY_UNDERLINE, "4:3"),
            (attr::DOTTED_UNDERLINE, "4:4"),
            (attr::DASHED_UNDERLINE, "4:5"),
        ]
        .into_iter()
        .find(|(bit, _)| style.attrs & bit != 0)
        .map_or("4", |(_, code)| code);
        codes.push(kind.into());
    }
    let color = |color: Color, base: u16, extended: u16| match color {
        Color::Default => None,
        Color::Indexed(i) if i < 8 && base != 0 => Some((base + i as u16).to_string()),
        Color::Indexed(i) if i < 16 && base != 0 => Some((base + 60 + i as u16 - 8).to_string()),
        Color::Indexed(i) => Some(format!("{extended};5;{i}")),
        Color::Rgb(r, g, b) => Some(format!("{extended};2;{r};{g};{b}")),
    };
    codes.extend(color(style.fg, 30, 38));
    codes.extend(color(style.bg, 40, 48));
    codes.extend(color(style.underline, 0, 58));
    format!("\x1b[{}m", codes.join(";"))
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// OSC parameters joined back together (they were split at `;`).
fn join_params(params: &[&[u8]]) -> String {
    params
        .iter()
        .map(|p| String::from_utf8_lossy(p))
        .collect::<Vec<_>>()
        .join(";")
}

/// A link under some cell: its columns and target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: usize,
    /// Exclusive.
    pub end: usize,
    pub url: String,
}

impl Term {
    fn link_id(&mut self, uri: String) -> u32 {
        if let Some(id) = self.link_ids.get(&uri) {
            return *id;
        }
        if self.links.len() >= MAX_LINKS {
            return 0;
        }
        self.links.push(uri.clone());
        let id = self.links.len() as u32;
        self.link_ids.insert(uri, id);
        id
    }

    /// The first line still in the scrollback, and the last screen line.
    pub fn line_range(&self) -> (i64, i64) {
        (
            self.scrolled_off - self.scrollback.len() as i64,
            self.scrolled_off + self.rows as i64 - 1,
        )
    }

    pub fn prompts(&self) -> impl DoubleEndedIterator<Item = i64> + '_ {
        self.prompts.iter().map(|command| command.prompt)
    }

    /// A line's characters (one per cell; wide ones cover two) and the
    /// column each starts at.
    pub fn line_chars(&self, abs: i64) -> Option<(Vec<char>, Vec<usize>)> {
        let cells = &self.line(abs)?.cells;
        let mut chars = Vec::with_capacity(cells.len());
        let mut columns = Vec::with_capacity(cells.len());
        for (c, cell) in cells.iter().enumerate() {
            if cell.width != Width::Spacer {
                chars.push(cell.ch);
                columns.push(c);
            }
        }
        Some((chars, columns))
    }

    /// The word around a cell, for double-click selection: a run of
    /// characters without spaces or brackets, quotes and separators (so
    /// paths and URLs stay whole).
    pub fn word_at(&self, abs: i64, col: usize) -> Option<((i64, usize), (i64, usize))> {
        let cells = &self.line(abs)?.cells;
        let in_word = |c: usize| {
            cells.get(c).is_some_and(|cell| {
                let ch = cell.ch;
                cell.width == Width::Spacer
                    || !(ch.is_whitespace() || "()[]{}<>'\"`|,;".contains(ch))
            })
        };
        if !in_word(col) {
            return None;
        }
        let start = (0..=col).rev().take_while(|&c| in_word(c)).last()?;
        let end = (col..cells.len()).take_while(|&c| in_word(c)).last()?;
        Some(((abs, start), (abs, end)))
    }

    /// The whole line around a row, following autowrap across rows, for
    /// triple-click selection.
    pub fn line_span(&self, abs: i64) -> ((i64, usize), (i64, usize)) {
        let wrapped = |line: i64| self.line(line).is_some_and(|row| row.wrapped);
        let mut first = abs;
        while wrapped(first - 1) {
            first -= 1;
        }
        let mut last = abs;
        while wrapped(last) && self.line(last + 1).is_some() {
            last += 1;
        }
        ((first, 0), (last, self.cols - 1))
    }

    /// Whether the cell at a column is the first half of a wide character.
    pub fn is_wide(&self, abs: i64, col: usize) -> bool {
        self.line(abs)
            .and_then(|row| row.cells.get(col))
            .is_some_and(|cell| cell.width == Width::Wide)
    }

    /// The link at a cell: an OSC 8 link, or else a URL in the text.
    pub fn link_at(&self, abs: i64, col: usize) -> Option<Link> {
        let row = self.line(abs)?;
        let cells = &row.cells;
        let id = cells.get(col)?.link;
        if id != 0 {
            let start = (0..=col)
                .rev()
                .take_while(|&c| cells[c].link == id)
                .last()?;
            let end = (col..cells.len())
                .take_while(|&c| cells[c].link == id)
                .last()?
                + 1;
            let url = self.links.get(id as usize - 1)?.clone();
            return Some(Link { start, end, url });
        }
        let (chars, columns) = self.line_chars(abs)?;
        let at = columns.iter().rposition(|&c| c <= col)?;
        let (from, to) = find_url(&chars, at)?;
        let end_col = columns[to - 1]
            + if cells[columns[to - 1]].width == Width::Wide {
                2
            } else {
                1
            };
        Some(Link {
            start: columns[from],
            end: end_col,
            url: chars[from..to].iter().collect(),
        })
    }

    /// Deletes lines at `row` (DL): like scrolling up inside the region
    /// starting at `row`, without saving anything to the scrollback.
    fn scroll_up_no_save(&mut self, row: usize, n: usize) {
        let n = n.min(self.bottom + 1 - row);
        for _ in 0..n {
            self.screen.remove(row);
            self.screen.insert(self.bottom, self.blank_row());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cols: usize, rows: usize, input: &str) -> Term {
        let mut term = Term::new(cols, rows, 100);
        let mut parser = vte::Parser::new();
        parser.advance(&mut term, input.as_bytes());
        term
    }

    fn screen_text(term: &Term) -> Vec<String> {
        (0..term.rows as i64)
            .map(|r| term.line(term.scrolled_off + r).unwrap().text(0, term.cols))
            .collect()
    }

    #[test]
    fn regression_combining_characters_are_bounded() {
        let term = run(10, 2, &format!("a{}b", "\u{301}".repeat(10_000)));
        let cell = &term.screen[0].cells[0];
        assert!(cell.extra.as_ref().unwrap().len() <= 256);
        assert_eq!(term.screen[0].cells[1].ch, 'b');
        assert!(cell.extra.as_ref().unwrap().starts_with('\u{301}'));
    }

    #[test]
    fn emoji_sequences_take_one_wide_cell() {
        // Each followed by "x", which must land right after it.
        for emoji in [
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", // family
            "\u{1F44D}\u{1F3FD}",                          // thumbs up, skin tone
            "\u{1F1E9}\u{1F1EA}",                          // flag DE
            "\u{2764}\u{FE0F}",                            // red heart
            "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}",          // rainbow flag
        ] {
            let term = run(10, 1, &format!("{emoji}x"));
            let cells = &term.screen[0].cells;
            assert_eq!(cells[0].width, Width::Wide, "{emoji}");
            assert_eq!(cells[1].width, Width::Spacer, "{emoji}");
            assert_eq!(cells[2].ch, 'x', "{emoji}");
            let mut text = String::new();
            cells[0].push_text(&mut text);
            assert_eq!(text, emoji);
        }
        // Two flags are two cells; a lone indicator stays narrow.
        let term = run(10, 1, "\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}x");
        assert_eq!(term.screen[0].cells[4].ch, 'x');
        let term = run(10, 1, "\u{1F1E9}x");
        assert_eq!(term.screen[0].cells[1].ch, 'x');
        // A skin tone after plain text is its own emoji.
        let term = run(10, 1, "a\u{1F3FD}x");
        assert_eq!(term.screen[0].cells[3].ch, 'x');
        // Accents still combine; text-style hearts stay narrow.
        let term = run(10, 1, "e\u{301}\u{2764}\u{FE0E}x");
        assert_eq!(term.screen[0].cells[2].ch, 'x');
        // A heart widened in the last column stays narrow and wraps on.
        let term = run(3, 2, "ab\u{2764}\u{FE0F}x");
        assert_eq!(term.screen[0].cells[2].width, Width::Narrow);
        assert_eq!(term.screen[1].cells[0].ch, 'x');
    }

    #[test]
    fn export_round_trips_text_and_colors() {
        let input = "plain \x1b[1;31mbold red\x1b[0m \x1b[38;2;1;2;3;48;5;200mrgb\x1b[0m\r\n\
                     \x1b[4:3;58;5;1mcurly\x1b[0m \x1b[95;104mbright\x1b[0m \u{1F44D}\u{1F3FD}e\u{301}\r\n\
                     a long line that wraps around\r\n";
        let original = run(10, 8, input);
        let saved = original.export(100);
        let restored = run(10, 8, &saved);
        assert_eq!(screen_text(&restored), screen_text(&original));
        for row in 0..original.rows {
            assert_eq!(
                restored.screen[row].cells, original.screen[row].cells,
                "row {row}"
            );
            assert_eq!(restored.screen[row].wrapped, original.screen[row].wrapped);
        }
        // Wrapped rows are one line again, to re-wrap at another width.
        assert!(saved.contains("a long line that wraps around\r\n"));
    }

    #[test]
    fn export_keeps_the_last_lines_and_the_normal_screen() {
        let lines: String = (1..=50).map(|i| format!("line {i}\r\n")).collect();
        let term = run(20, 5, &lines);
        assert_eq!(term.export(3), "line 48\r\nline 49\r\nline 50\r\n");
        // A full-screen program's screen isn't saved, the shell output is.
        let term = run(20, 5, &format!("{lines}\x1b[?1049hTUI"));
        assert!(term.export(3).ends_with("line 50\r\n"));
        // A wrapped line isn't cut in half.
        let term = run(4, 5, "abcdefgh\r\nx\r\n");
        assert_eq!(term.export(2), "abcdefgh\r\nx\r\n");
    }

    #[test]
    fn reports_grapheme_cluster_mode() {
        let mut term = run(10, 1, "\x1b[?2027$p");
        assert_eq!(term.take_responses(), b"\x1b[?2027;3$y");
    }

    #[test]
    fn prints_and_wraps() {
        let term = run(5, 3, "hello world");
        assert_eq!(screen_text(&term), ["hello", " worl", "d"]);
        assert!(term.screen[0].wrapped);
    }

    #[test]
    fn scrolls_into_scrollback() {
        let term = run(10, 2, "one\r\ntwo\r\nthree");
        assert_eq!(screen_text(&term), ["two", "three"]);
        assert_eq!(term.scrollback_len(), 1);
        assert_eq!(term.line(0).unwrap().text(0, 10), "one");
    }

    #[test]
    fn moves_cursor_and_erases() {
        let term = run(10, 3, "abcdef\x1b[1;3H\x1b[KX\x1b[3;1Hz");
        assert_eq!(screen_text(&term), ["abX", "", "z"]);
    }

    #[test]
    fn colors_and_attributes() {
        let term = run(10, 1, "\x1b[1;31mR\x1b[38;2;1;2;3mT\x1b[0mn");
        let cells = &term.screen[0].cells;
        assert_eq!(cells[0].style.fg, Color::Indexed(1));
        assert_ne!(cells[0].style.attrs & attr::BOLD, 0);
        assert_eq!(cells[1].style.fg, Color::Rgb(1, 2, 3));
        assert_eq!(cells[2].style, Style::default());
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let term = run(4, 2, "a漢b");
        let cells = &term.screen[0].cells;
        assert_eq!((cells[1].ch, cells[1].width), ('漢', Width::Wide));
        assert_eq!(cells[2].width, Width::Spacer);
        assert_eq!(cells[3].ch, 'b');
    }

    #[test]
    fn alternate_screen_restores_primary() {
        let term = run(10, 2, "keep\x1b[?1049hfull screen\x1b[?1049l");
        assert_eq!(screen_text(&term)[0], "keep");
        assert!(!term.alt_active());
    }

    #[test]
    fn scroll_region_keeps_lines_outside() {
        let term = run(
            10,
            4,
            "head\r\n\x1b[2;3r\x1b[2;1Ha\r\nb\r\nc\x1b[r\x1b[4;1Hfoot",
        );
        assert_eq!(screen_text(&term), ["head", "b", "c", "foot"]);
        assert_eq!(term.scrollback_len(), 0, "region scrolling isn't saved");
    }

    #[test]
    fn answers_cursor_position_query() {
        let mut term = run(10, 5, "\x1b[3;4H\x1b[6n");
        assert_eq!(term.take_responses(), b"\x1b[3;4R");
    }

    #[test]
    fn line_drawing_charset() {
        let term = run(5, 1, "\x1b(0lqk\x1b(B");
        assert_eq!(screen_text(&term)[0], "┌─┐");
    }

    #[test]
    fn finds_links() {
        let term = run(
            40,
            2,
            "go to https://example.com now\r\n\x1b]8;;https://real.target\x1b\\click me\x1b]8;;\x1b\\ x",
        );
        let link = term.link_at(0, 8).unwrap();
        assert_eq!(
            (link.start, link.end, link.url.as_str()),
            (6, 25, "https://example.com")
        );
        let link = term.link_at(1, 2).unwrap();
        assert_eq!(
            (link.start, link.end, link.url.as_str()),
            (0, 8, "https://real.target")
        );
        assert_eq!(term.link_at(1, 9), None);
    }

    #[test]
    fn mouse_modes_say_which_motion_is_reported() {
        let modes = |input: &str| run(10, 1, input).modes;
        assert_eq!(modes("\x1b[?1000h").mouse_motion, MouseMotion::None);
        assert_eq!(modes("\x1b[?1002h").mouse_motion, MouseMotion::Drags);
        assert_eq!(modes("\x1b[?1003h").mouse_motion, MouseMotion::All);
        // The last one asked for wins, as in xterm.
        assert_eq!(
            modes("\x1b[?1003h\x1b[?1002h").mouse_motion,
            MouseMotion::Drags
        );
        let off = modes("\x1b[?1002h\x1b[?1002l");
        assert!(!off.mouse_reporting);
        assert_eq!(off.mouse_motion, MouseMotion::None);
    }

    #[test]
    fn underline_styles_and_colors() {
        let style = |input: &str| run(10, 1, input).line(0).unwrap().cells[0].style;
        assert_eq!(style("\x1b[4mx").attrs, attr::UNDERLINE);
        let curly = style("\x1b[4:3;58:2::255:0:0mx");
        assert_eq!(curly.attrs, attr::UNDERLINE | attr::CURLY_UNDERLINE);
        assert_eq!(curly.underline, Color::Rgb(255, 0, 0));
        assert_eq!(
            style("\x1b[4:3m\x1b[4:2mx").attrs,
            attr::UNDERLINE | attr::DOUBLE_UNDERLINE,
            "a new style replaces the old"
        );
        assert_eq!(
            style("\x1b[21mx").attrs,
            attr::UNDERLINE | attr::DOUBLE_UNDERLINE
        );
        assert_eq!(style("\x1b[58;5;3mx").underline, Color::Indexed(3));
        for off in ["\x1b[4:3m\x1b[24mx", "\x1b[4:5m\x1b[4:0mx"] {
            assert_eq!(style(off).attrs, 0, "{off:?}");
        }
        assert_eq!(style("\x1b[58;5;3m\x1b[59mx").underline, Color::Default);
        assert_eq!(style("\x1b[4:4;58;5;1m\x1b[0mx"), Style::default());
    }

    #[test]
    fn programs_can_copy_but_not_read_the_clipboard() {
        let mut term = run(10, 1, "\x1b]52;c;aGVsbG8gd29ybGQ=\x07");
        let copy = term.take_copied().unwrap();
        assert_eq!(copy.text, "hello world");
        assert!(copy.clipboard && !copy.primary);
        assert_eq!(term.take_copied(), None);
        // tmux sends no target; Neovim may name both; padding is optional.
        let mut term = run(10, 1, "\x1b]52;;aGk\x1b\\");
        assert_eq!(term.take_copied().unwrap().text, "hi");
        let mut term = run(10, 1, "\x1b]52;pc;aGk=\x07");
        let copy = term.take_copied().unwrap();
        assert!(copy.clipboard && copy.primary);
        // Queries and garbage are ignored, and nothing is answered.
        for input in ["\x1b]52;c;?\x07", "\x1b]52;c;!!!\x07", "\x1b]52;q;aGk=\x07"] {
            let mut term = run(10, 1, input);
            assert_eq!(term.take_copied(), None, "{input:?}");
            assert!(term.take_responses().is_empty());
        }
    }

    #[test]
    fn bells_and_notifications_ask_for_attention() {
        let mut term = run(10, 1, "a\x07b");
        assert_eq!(term.take_attention().as_deref(), Some(""));
        assert_eq!(term.take_attention(), None);
        // A bell ending an OSC is not a bell.
        let mut term = run(10, 1, "\x1b]2;title\x07");
        assert_eq!(term.take_attention(), None);
        let mut term = run(10, 1, "\x1b]9;Build done\x07");
        assert_eq!(term.take_attention().as_deref(), Some("Build done"));
        let mut term = run(10, 1, "\x1b]777;notify;opencode;Needs approval\x1b\\");
        assert_eq!(
            term.take_attention().as_deref(),
            Some("opencode: Needs approval")
        );
        // Progress reports and bells don't replace a message.
        let mut term = run(10, 1, "\x1b]9;4;1;50\x07");
        assert_eq!(term.take_attention(), None);
        let mut term = run(10, 1, "\x1b]9;hi\x07\x07");
        assert_eq!(term.take_attention().as_deref(), Some("hi"));
    }

    #[test]
    fn limits_title_length() {
        let mut term = run(10, 1, &format!("\x1b]2;{}\x07", "x".repeat(500)));
        assert_eq!(term.take_title().unwrap().chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn kitty_keyboard_protocol() {
        let mut term = run(10, 2, "\x1b[?u");
        assert_eq!(term.take_responses(), b"\x1b[?0u");
        let mut parser = vte::Parser::new();
        // Push "disambiguate" + an unsupported flag; only supported ones stick.
        parser.advance(&mut term, b"\x1b[>3u\x1b[?u");
        assert_eq!(term.take_responses(), b"\x1b[?1u");
        // The alternate screen has its own stack, cleared when leaving it.
        parser.advance(&mut term, b"\x1b[?1049h\x1b[>9u\x1b[?u");
        assert_eq!(term.take_responses(), b"\x1b[?9u");
        parser.advance(&mut term, b"\x1b[?1049l\x1b[?u");
        assert_eq!(term.take_responses(), b"\x1b[?1u");
        parser.advance(&mut term, b"\x1b[<u\x1b[?u");
        assert_eq!(term.take_responses(), b"\x1b[?0u");
        parser.advance(&mut term, b"\x1b[>4;1m");
        assert_eq!(term.keyboard.modify_other_keys, 1);
    }

    #[test]
    fn reports_modes_and_synchronized_output() {
        let mut term = run(10, 2, "\x1b[?2026$p\x1b[?2026h\x1b[?2026$p\x1b[?9999$p");
        assert_eq!(
            term.take_responses(),
            b"\x1b[?2026;2$y\x1b[?2026;1$y\x1b[?9999;0$y"
        );
        assert!(term.modes.synchronized);
    }

    #[test]
    fn selects_words_and_wrapped_lines() {
        let term = run(10, 3, "ls (~/src/a.rs) next\r\nshort");
        // Row 0: "ls (~/src/", row 1 (wrapped on): "a.rs) next".
        assert_eq!(term.word_at(0, 5), Some(((0, 4), (0, 9))));
        assert_eq!(term.word_at(0, 2), None, "a space");
        assert_eq!(term.line_span(1), ((0, 0), (1, 9)));
        assert_eq!(term.line_span(2), ((2, 0), (2, 9)));
    }

    #[test]
    fn rewraps_lines_when_the_width_changes() {
        let mut term = run(20, 4, "hello world\r\nab");
        term.resize(5, 4);
        assert_eq!(screen_text(&term), ["hello", " worl", "d", "ab"]);
        assert_eq!(term.cursor(), Some((3, 2)), "still after \"ab\"");
        term.resize(20, 4);
        assert_eq!(screen_text(&term), ["hello world", "ab", "", ""]);
        assert_eq!(term.cursor(), Some((1, 2)));
    }

    #[test]
    fn rewraps_into_and_out_of_the_scrollback() {
        let mut term = run(6, 2, "abcdefghijkl\r\nend");
        // "abcdef" and "ghijkl" were one line; "end" has the cursor.
        term.resize(12, 2);
        assert_eq!(screen_text(&term), ["abcdefghijkl", "end"]);
        assert_eq!(term.scrollback_len(), 0);
        term.resize(4, 2);
        assert_eq!(screen_text(&term), ["ijkl", "end"]);
        assert_eq!(term.scrollback_len(), 2);
        assert_eq!(
            term.line(term.scrolled_off() - 2).unwrap().text(0, 4),
            "abcd"
        );
    }

    #[test]
    fn rewrap_keeps_wide_characters_whole() {
        let mut term = run(6, 2, "ab漢字");
        term.resize(5, 2);
        // "ab漢" fits in 4 columns; "字" would straddle the edge, so it moves.
        assert_eq!(screen_text(&term), ["ab漢", "字"]);
    }

    #[test]
    fn rewrap_moves_prompt_marks_with_their_lines() {
        let mut term = run(10, 3, "0123456789abcdef\r\n\x1b]133;A\x07$ ");
        let prompt = term.prompts().next().unwrap();
        assert_eq!(term.line(prompt).unwrap().text(0, 10), "$");
        term.resize(20, 3);
        let prompt = term.prompts().next().unwrap();
        assert_eq!(term.line(prompt).unwrap().text(0, 20), "$");
    }

    #[test]
    fn shell_marks_give_commands_their_output_and_exit_status() {
        let a = "\x1b]133;A\x07";
        let b = "\x1b]133;B\x07";
        let c = "\x1b]133;C\x07";
        let mut term = run(
            20,
            8,
            &format!("{a}$ {b}make\r\n{c}built\r\nall\r\n\x1b]133;D;2\x07{a}$ {b}ls"),
        );
        let finished = term.take_finished().unwrap();
        assert_eq!(finished.command.as_deref(), Some("make"));
        assert_eq!(finished.exit, Some(2));
        assert_eq!(term.take_finished(), None, "only once");
        let commands: Vec<&Command> = term.commands().collect();
        assert_eq!(commands.len(), 2);
        assert!(commands[0].failed());
        assert_eq!(commands[0].output, Some((1, 0)));
        assert!(!commands[1].failed());
        assert_eq!(term.last_output().as_deref(), Some("built\nall"));

        // A command running now: its output so far. One without output
        // doesn't count.
        let mut parser = vte::Parser::new();
        parser.advance(&mut term, format!("\r\n{c}x\r\n").as_bytes());
        assert_eq!(term.last_output().as_deref(), Some("x"));
        // A new prompt without D ends the command, with no status.
        parser.advance(
            &mut term,
            format!("{a}$ {b}\r\n{c}\x1b]133;D;0\x07{a}").as_bytes(),
        );
        assert_eq!(term.take_finished().unwrap().exit, Some(0));
        assert_eq!(term.commands().nth(1).unwrap().exit, None);

        // Fish says what the command was, also when it spans lines.
        let mut fish = run(
            20,
            4,
            &format!("{a}$ {b}\x1b]133;C;cmdline_url=echo%20hi%0Ax\x07\x1b]133;D;0\x07"),
        );
        assert_eq!(
            fish.take_finished().unwrap().command.as_deref(),
            Some("echo hi x")
        );
        assert_eq!(term.last_output().as_deref(), Some("x"));
    }

    #[test]
    fn shells_report_their_folder() {
        let term = run(10, 2, "\x1b]7;file://box/home/me/my%20dir\x07");
        assert_eq!(
            term.reported_cwd(),
            Some(&("box".to_string(), "/home/me/my dir".to_string()))
        );
        let term = run(10, 2, "\x1b]7;nonsense\x07");
        assert_eq!(term.reported_cwd(), None);
    }

    #[test]
    fn programs_report_progress() {
        let mut term = run(10, 2, "\x1b]9;4;1;42\x07");
        assert_eq!(term.take_progress(), Some(Some(Progress::Normal(42))));
        assert_eq!(term.take_progress(), None, "unchanged");
        let mut parser = vte::Parser::new();
        parser.advance(&mut term, b"\x1b]9;4;3\x07\x1b]9;4;2;250\x07");
        assert_eq!(term.take_progress(), Some(Some(Progress::Error(100))));
        assert_eq!(term.take_attention(), None, "not a notification");
        parser.advance(&mut term, b"\x1b]9;4;0\x07");
        assert_eq!(term.take_progress(), Some(None));
    }

    #[test]
    fn programs_change_and_query_colors() {
        assert_eq!(parse_color(b"rgb:ff/80/0"), Some((255, 128, 0)));
        assert_eq!(parse_color(b"rgb:ffff/0000/8080"), Some((255, 0, 128)));
        assert_eq!(parse_color(b"#102030"), Some((0x10, 0x20, 0x30)));
        assert_eq!(parse_color(b"#fff"), Some((0xf0, 0xf0, 0xf0)));
        assert_eq!(parse_color(b"red"), Some((255, 0, 0)));
        assert_eq!(parse_color(b"Dark Gray"), Some((169, 169, 169)));
        assert_eq!(parse_color(b"no such color"), None);

        let mut term = run(
            10,
            2,
            "\x1b]4;1;#ff0000;2;rgb:00/ff/00\x07\x1b]11;#000000\x07",
        );
        let colors = term.take_colors().unwrap();
        assert_eq!(colors.palette[1], Some((255, 0, 0)));
        assert_eq!(colors.palette[2], Some((0, 255, 0)));
        assert_eq!(colors.bg, Some((0, 0, 0)));
        assert_eq!(term.take_colors(), None, "unchanged");

        let mut parser = vte::Parser::new();
        term.report_palette[3] = (1, 2, 3);
        parser.advance(&mut term, b"\x1b]4;1;?;3;?\x07\x1b]11;?\x07");
        assert_eq!(
            String::from_utf8(term.take_responses()).unwrap(),
            "\x1b]4;1;rgb:ffff/0000/0000\x1b\\\x1b]4;3;rgb:0101/0202/0303\x1b\\\
             \x1b]11;rgb:0000/0000/0000\x1b\\"
        );

        parser.advance(&mut term, b"\x1b]104;1\x07\x1b]111\x07");
        let colors = term.take_colors().unwrap();
        assert_eq!(colors.palette[1], None);
        assert_eq!(colors.palette[2], Some((0, 255, 0)));
        assert_eq!(colors.bg, None);
        parser.advance(&mut term, b"\x1b]104\x07");
        assert_eq!(term.take_colors().unwrap(), Colors::default());
    }

    #[test]
    fn programs_choose_whether_the_cursor_blinks() {
        let term = run(10, 2, "\x1b[5 q");
        assert_eq!(term.modes.cursor_shape, CursorShape::Bar);
        assert_eq!(term.modes.cursor_blink, Some(true));
        let term = run(10, 2, "\x1b[5 q\x1b[2 q");
        assert_eq!(term.modes.cursor_blink, Some(false));
        let term = run(10, 2, "\x1b[5 q\x1b[0 q");
        assert_eq!(term.modes.cursor_blink, None);
    }

    #[test]
    fn shows_sixel_images_and_moves_below_them() {
        // 8x16 pixel cells; an image 2 pixels wide and 12 high: one row.
        let mut term = Term::new(10, 4, 100);
        term.set_cell_px(8.0, 16.0);
        let mut parser = vte::Parser::new();
        parser.advance(&mut term, b"ab\x1bP0;1q#1;2;100;0;0#1~~-~~\x1b\\x");
        let placement = &term.graphics.placements[0];
        assert_eq!((placement.image.width, placement.image.height), (2, 12));
        assert_eq!(
            (placement.abs_row, placement.col, placement.rows),
            (0, 2, 1)
        );
        assert_eq!(
            screen_text(&term)[0],
            "abx",
            "on its last row, in its column"
        );
        parser.advance(&mut term, b"\x1b[c");
        assert_eq!(term.take_responses(), b"\x1b[?62;4;22c");
    }

    #[test]
    fn resize_keeps_cursor_line() {
        let mut term = run(10, 4, "1\r\n2\r\n3\r\n4");
        term.resize(10, 2);
        assert_eq!(screen_text(&term), ["3", "4"]);
        term.resize(10, 4);
        assert_eq!(screen_text(&term), ["1", "2", "3", "4"]);
    }
}
