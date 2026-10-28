//! The GTK widget for soi's terminal engine: draws the grid and images with
//! Cairo and Pango, and turns keyboard and mouse input into bytes for the
//! shell.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, graphene, pango};
use portable_pty::PtySize;

use super::Feeder;
use super::area::TermArea;
use super::boxdraw;
use super::browse::{self, Action, Browse};
use super::graphics::{Image, Placement};
use super::keys::{self, Encoding, WordJumps};
use super::links;
use super::pty::{self, Event, Pty};
use super::screen::{
    Cell as GridCell, Color, Colors, CursorShape, Finished, Link, Progress, Term, Width, attr,
};
use crate::theme::TerminalTheme;

type Rgb = (f64, f64, f64);
type TitleCallback = RefCell<Option<Box<dyn Fn(&str)>>>;
type ExitCallback = RefCell<Option<Box<dyn Fn()>>>;
type AttentionCallback = RefCell<Option<Box<dyn Fn(&str)>>>;
type InputCallback = RefCell<Option<Box<dyn Fn(&Input)>>>;
type FinishedCallback = RefCell<Option<Box<dyn Fn(&Finished)>>>;
type ProgressCallback = RefCell<Option<Box<dyn Fn(Option<Progress>)>>>;

/// Something typed into a terminal, to type into others too (see
/// `TermView::type_input`). Keys are encoded for each terminal's own modes.
#[derive(Clone, Debug)]
pub enum Input {
    Key {
        key: gdk::Key,
        state: gdk::ModifierType,
        /// What the key types without modifiers (see the Kitty protocol).
        base: Option<char>,
    },
    /// From the input method (accents, compose, other scripts).
    Text(String),
    Paste(String),
}
/// A position in the terminal: absolute line and column.
type Pos = (i64, usize);

/// Space between the left edge of a pane and the text, in pixels: keeps
/// text off the sidebar and split dividers, and holds the marks of failed
/// commands.
const GUTTER: f64 = 5.0;
/// Most output handled at once before the screen gets a chance to redraw.
const MAX_BATCH_BYTES: usize = 64 * 1024;
/// How long a program may hold back redraws with synchronized output.
const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);
/// Output this soon after input (typing, focus, resizing) is the program
/// answering it, not the program being busy on its own.
const ECHO_WINDOW: Duration = Duration::from_millis(300);
/// Screen readers hear about new output at most this often.
const ACCESSIBLE_UPDATE: Duration = Duration::from_millis(200);
/// How long the cursor is shown, then hidden, when it blinks.
const BLINK_INTERVAL: Duration = Duration::from_millis(600);
/// It stops blinking (and stays shown) this long after the last key, like
/// GTK's own text fields, so an idle terminal doesn't redraw all the time.
const BLINK_FOR: Duration = Duration::from_secs(15);

/// Textures are a second copy of the pixels, independently of the
/// graphics store (on the GPU once drawn).
const MAX_TEXTURE_BYTES: usize = 128 * 1024 * 1024;

/// The images' pixels as textures, made once per image.
struct TextureCache {
    entries: HashMap<u64, gdk::Texture>,
    bytes: usize,
    limit: usize,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            limit: MAX_TEXTURE_BYTES,
        }
    }
}

impl TextureCache {
    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn retain_placements(&mut self, placements: &[Placement]) {
        let live: HashSet<u64> = placements.iter().map(|p| p.image.serial).collect();
        self.entries.retain(|serial, _| live.contains(serial));
        self.bytes = self.entries.values().map(texture_bytes).sum();
    }

    fn texture(&mut self, image: &Image) -> Option<gdk::Texture> {
        if let Some(texture) = self.entries.get(&image.serial) {
            return Some(texture.clone());
        }
        let size = image.pixels.len();
        // Evict before allocating the new buffer, including when all the old
        // images still have placements. Creation order makes eviction stable.
        while self.bytes.saturating_add(size) > self.limit {
            let Some(oldest) = self.entries.keys().min().copied() else {
                break;
            };
            if let Some(texture) = self.entries.remove(&oldest) {
                self.bytes -= texture_bytes(&texture);
            }
        }
        if image.width == 0 || image.height == 0 {
            return None;
        }
        // Cairo's ARGB32, native-endian: B, G, R, A in memory on little-endian.
        let format = if cfg!(target_endian = "little") {
            gdk::MemoryFormat::B8g8r8a8Premultiplied
        } else {
            gdk::MemoryFormat::A8r8g8b8Premultiplied
        };
        let texture = gdk::MemoryTexture::new(
            image.width as i32,
            image.height as i32,
            format,
            &glib::Bytes::from(&image.pixels[..]),
            image.width as usize * 4,
        )
        .upcast::<gdk::Texture>();
        if size <= self.limit {
            self.entries.insert(image.serial, texture.clone());
            self.bytes += size;
        }
        Some(texture)
    }
}

fn texture_bytes(texture: &gdk::Texture) -> usize {
    texture.width() as usize * texture.height() as usize * 4
}

/// Fonts, colors and cell metrics.
struct Look {
    /// Regular, bold, italic, bold italic.
    fonts: [pango::FontDescription; 4],
    fg: Rgb,
    bg: Rgb,
    palette: [Rgb; 256],
    /// The theme's colors, under those the program set (see `with_colors`).
    theme_fg: Rgb,
    theme_bg: Rgb,
    theme_palette: [Rgb; 256],
    /// The program set the background (OSC 11): painted, not transparent.
    own_bg: bool,
    /// The program's cursor color (OSC 12); else the text's.
    cursor: Option<Rgb>,
    cell_w: f64,
    cell_h: f64,
    /// Distance from the top of a cell to the text baseline.
    baseline: f64,
}

struct SpawnRequest {
    argv: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    /// Typed once the program runs (see `TermView::type_ahead`).
    input: String,
}

struct Inner {
    area: glib::WeakRef<TermArea>,
    term: RefCell<Term>,
    feeder: RefCell<Feeder>,
    pty: RefCell<Option<Pty>>,
    /// The program started instead of a shell, if any (see `set_program`).
    program: RefCell<Option<String>>,
    /// The shell starts once the widget has a size, so programs see the
    /// real terminal size from the beginning.
    spawn: RefCell<Option<SpawnRequest>>,
    look: RefCell<Look>,
    shaped: RefCell<Shaped>,
    /// Lines scrolled back from the bottom.
    view_offset: Cell<usize>,
    scroll_remainder: Cell<f64>,
    selection: Cell<Option<(Pos, Pos)>>,
    drag_anchor: Cell<Option<Pos>>,
    /// Where a selection made with Shift and the arrow keys started; the
    /// cursor, which the program moves, is its other end.
    marking: Cell<Option<Pos>>,
    pointer: Cell<(f64, f64)>,
    /// The link under the mouse pointer and its absolute line.
    hovered_link: RefCell<Option<(i64, Link)>>,
    /// A redraw is scheduled for when synchronized output times out.
    sync_timer: Rc<Cell<bool>>,
    /// Browse mode (vim keys over the output), when active.
    browse: RefCell<Option<Browse>>,
    /// The key controller, to take the input method away while browsing
    /// (otherwise typed letters would reach the shell).
    keys: RefCell<Option<gtk::EventControllerKey>>,
    focused: Cell<bool>,
    /// The cursor blinks unless the program says otherwise (a setting).
    blink_default: Cell<bool>,
    /// In the hidden half of a blink.
    blink_hidden: Cell<bool>,
    blink_timer: RefCell<Option<glib::SourceId>>,
    /// Screen readers will be told about new output (see `ACCESSIBLE_UPDATE`).
    accessible_pending: Rc<Cell<bool>>,
    textures: RefCell<TextureCache>,
    im: gtk::IMMulticontext,
    on_title: TitleCallback,
    on_exit: ExitCallback,
    on_attention: AttentionCallback,
    on_input: InputCallback,
    on_finished: FinishedCallback,
    on_progress: ProgressCallback,
    /// When input (or a resize) last reached the program.
    last_input: Cell<Option<Instant>>,
    /// When the program last printed something on its own.
    last_activity: Cell<Option<Instant>>,
    /// Counts output, so savers can tell whether there's anything new.
    output_serial: Cell<u64>,
}

#[derive(Clone)]
pub struct TermView {
    area: TermArea,
    inner: Rc<Inner>,
}

impl TermView {
    pub fn new(
        argv: Vec<String>,
        cwd: &Path,
        env: &[(&str, &str)],
        theme: &TerminalTheme,
        scrollback: usize,
    ) -> Self {
        let area = TermArea::default();
        area.add_css_class("soi-terminal");
        let im = gtk::IMMulticontext::new();
        im.set_client_widget(Some(&area));
        let mut env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        env.extend([
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ]);
        let inner = Rc::new(Inner {
            area: area.downgrade(),
            term: RefCell::new(Term::new(80, 24, scrollback)),
            feeder: RefCell::default(),
            pty: RefCell::default(),
            program: RefCell::default(),
            spawn: RefCell::new(Some(SpawnRequest {
                argv,
                cwd: cwd.to_path_buf(),
                env,
                input: String::new(),
            })),
            look: RefCell::new(make_look(&area, theme)),
            shaped: RefCell::default(),
            view_offset: Cell::new(0),
            scroll_remainder: Cell::new(0.0),
            selection: Cell::new(None),
            drag_anchor: Cell::new(None),
            marking: Cell::new(None),
            pointer: Cell::new((0.0, 0.0)),
            hovered_link: RefCell::default(),
            sync_timer: Rc::default(),
            browse: RefCell::default(),
            keys: RefCell::default(),
            focused: Cell::new(false),
            blink_default: Cell::new(false),
            blink_hidden: Cell::new(false),
            blink_timer: RefCell::default(),
            accessible_pending: Rc::default(),
            textures: RefCell::default(),
            im,
            on_title: RefCell::default(),
            on_exit: RefCell::default(),
            on_attention: RefCell::default(),
            on_input: RefCell::default(),
            on_finished: RefCell::default(),
            on_progress: RefCell::default(),
            last_input: Cell::new(None),
            last_activity: Cell::new(None),
            output_serial: Cell::new(0),
        });
        inner.apply_look_to_term();
        install_handlers(&area, &inner);
        area.update_property(&[gtk::accessible::Property::Label("Terminal")]);
        let weak = Rc::downgrade(&inner);
        area.set_text_source(move || match weak.upgrade() {
            Some(inner) => inner.screen_text(),
            None => Default::default(),
        });
        Self { area, inner }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.area.upcast_ref()
    }

    /// The program running in the foreground, or `None` when the shell is
    /// just waiting at its prompt.
    /// The program in the foreground. For a terminal running a program
    /// instead of a shell, that program while nothing else is.
    pub fn running_program(&self) -> Option<String> {
        let pid = self.pid()?;
        pty::foreground_program(pid).or_else(|| self.inner.program.borrow().clone())
    }

    /// Every program in the foreground, the leading one first: also those a
    /// wrapper started. Like `running_program` when only one runs.
    pub fn running_programs(&self) -> Vec<String> {
        let Some(pid) = self.pid() else {
            return Vec::new();
        };
        let programs = pty::foreground_programs(pid);
        if programs.is_empty() {
            return self.inner.program.borrow().iter().cloned().collect();
        }
        programs
    }

    /// Says that the terminal runs `program` instead of a shell, so that it
    /// counts as running even when it's the only process: an agent started
    /// directly, say.
    pub fn set_program(&self, program: &str) {
        let name = Path::new(program)
            .file_name()
            .map_or_else(|| program.to_string(), |n| n.to_string_lossy().into_owned());
        self.inner.program.replace(Some(name));
    }

    /// The folder the shell reported (OSC 7), if it's on this computer.
    pub fn reported_cwd(&self) -> Option<PathBuf> {
        let term = self.inner.term.borrow();
        let (host, path) = term.reported_cwd()?;
        let local = host.is_empty() || host == "localhost" || *host == hostname();
        local.then(|| PathBuf::from(path))
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.pty.borrow().as_ref().and_then(|p| p.pid)
    }

    pub fn set_theme(&self, theme: &TerminalTheme) {
        let mut look = make_look(&self.area, theme);
        look.with_colors(self.inner.term.borrow().colors());
        self.inner.look.replace(look);
        self.inner.shaped.borrow_mut().clear();
        self.inner.apply_look_to_term();
        self.inner.textures.borrow_mut().clear();
        self.inner.relayout(self.area.width(), self.area.height());
        self.area.queue_draw();
    }

    /// Whether the cursor blinks when the program doesn't say.
    pub fn set_cursor_blink(&self, blink: bool) {
        self.inner.blink_default.set(blink);
        self.inner.restart_blink();
    }

    pub fn set_scrollback(&self, lines: usize) {
        self.inner.term.borrow_mut().set_scrollback_limit(lines);
    }

    pub fn copy_clipboard(&self) {
        let Some((start, end)) = self.inner.selection.get() else {
            return;
        };
        let text = self.inner.term.borrow().text_between(start, end);
        if !text.is_empty() {
            self.area.clipboard().set_text(&text);
        }
    }

    /// Copies the selection and, when it was made with the keyboard,
    /// deletes it from the command line.
    pub fn cut_clipboard(&self) {
        self.copy_clipboard();
        self.inner.delete_marked();
    }

    pub fn paste_clipboard(&self) {
        let inner = Rc::downgrade(&self.inner);
        self.area
            .clipboard()
            .read_text_async(gio::Cancellable::NONE, move |result| {
                if let (Some(inner), Ok(Some(text))) = (inner.upgrade(), result) {
                    inner.paste(&text);
                }
            });
    }

    pub fn connect_title_changed(&self, f: impl Fn(&str) + 'static) {
        self.inner.on_title.replace(Some(Box::new(f)));
    }

    pub fn connect_exited(&self, f: impl Fn() + 'static) {
        self.inner.on_exit.replace(Some(Box::new(f)));
    }

    /// A bell (empty text) or a notification from the program (OSC 9/777).
    pub fn connect_attention(&self, f: impl Fn(&str) + 'static) {
        self.inner.on_attention.replace(Some(Box::new(f)));
    }

    /// A shell command ended (the shell marks commands, OSC 133).
    pub fn connect_command_finished(&self, f: impl Fn(&Finished) + 'static) {
        self.inner.on_finished.replace(Some(Box::new(f)));
    }

    /// The program reported its progress (OSC 9;4), or that it's over.
    pub fn connect_progress(&self, f: impl Fn(Option<Progress>) + 'static) {
        self.inner.on_progress.replace(Some(Box::new(f)));
    }

    /// Copies the output of the last command (see `Term::last_output`).
    /// Whether there was any.
    pub fn copy_last_output(&self) -> bool {
        let output = self.inner.term.borrow().last_output();
        match output {
            Some(text) => {
                self.area.clipboard().set_text(&text);
                true
            }
            None => {
                self.area.error_bell();
                false
            }
        }
    }

    /// What's typed or pasted here.
    pub fn connect_input(&self, f: impl Fn(&Input) + 'static) {
        self.inner.on_input.replace(Some(Box::new(f)));
    }

    /// Types what was typed into another terminal, as if typed here.
    pub fn type_input(&self, input: &Input) {
        match input {
            Input::Key { key, state, base } => {
                if let Some(bytes) = keys::encode(*key, *state, *base, self.inner.encoding()) {
                    self.inner.send_input(&bytes);
                }
            }
            Input::Text(text) => self.inner.send_input(text.as_bytes()),
            Input::Paste(text) => self.inner.send_paste(&normalize_paste(text)),
        }
    }

    /// Types `text` for the program: now, or as soon as it runs (a terminal
    /// starts its program once it has a size).
    pub fn type_ahead(&self, text: &str) {
        match self.inner.spawn.borrow_mut().as_mut() {
            Some(request) => request.input.push_str(text),
            None => self.inner.write(text.as_bytes()),
        }
    }

    /// Exit statuses of the commands the shell marked, oldest first.
    #[cfg(feature = "gui-tests")]
    pub fn exit_statuses(&self) -> Vec<Option<i32>> {
        let term = self.inner.term.borrow();
        term.commands().map(|command| command.exit).collect()
    }

    /// What screen readers read (see `Inner::screen_text`).
    #[cfg(feature = "gui-tests")]
    pub fn accessible_text(&self) -> (String, u32) {
        self.inner.screen_text()
    }

    /// The output so far, to save (see `Term::export`).
    pub fn export(&self, max_lines: usize) -> String {
        self.inner.term.borrow().export(max_lines)
    }

    /// Shows saved output (from `export`) above what the program prints.
    pub fn restore(&self, saved: &str) {
        let mut term = self.inner.term.borrow_mut();
        self.inner
            .feeder
            .borrow_mut()
            .feed(&mut term, saved.as_bytes());
        // Only text and colors are saved; nothing in it is acted on.
        term.take_responses();
        term.take_title();
        term.take_attention();
        term.take_copied();
        term.take_finished();
        term.take_progress();
        term.take_colors();
    }

    /// Changes whenever the program prints something.
    pub fn output_serial(&self) -> u64 {
        self.inner.output_serial.get()
    }

    /// When the program last printed output that wasn't an answer to input.
    pub fn last_activity(&self) -> Option<Instant> {
        self.inner.last_activity.get()
    }

    /// Enters browse mode (or leaves it); with `search`, starts typing a
    /// search right away.
    pub fn browse(&self, search: bool) {
        if self.inner.browse.borrow().is_some() && !search {
            self.inner.exit_browse();
        } else {
            self.inner.enter_browse(search);
        }
        self.area.grab_focus();
    }
}

/// This computer's name, as shells put it in OSC 7.
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; the result is cut at
    // the first NUL below.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    if ok {
        String::from_utf8_lossy(&buf[..end]).into_owned()
    } else {
        String::new()
    }
}

/// Line ends as Enter sends them, with line-editor commands stripped.
/// In particular, never let clipboard ESC terminate bracketed paste (even
/// nested end markers), or control keys execute/edit input behind the preview.
fn normalize_paste(text: &str) -> String {
    text.chars()
        .filter(|&c| !c.is_control() || matches!(c, '\t' | '\r' | '\n'))
        .collect::<String>()
        .replace("\r\n", "\r")
        .replace('\n', "\r")
}

#[cfg(test)]
mod paste_security_tests {
    use super::normalize_paste;

    #[test]
    fn clipboard_cannot_inject_terminal_or_line_editor_controls() {
        for text in [
            "\x1b[201~echo injected\n",
            "\x1b[20\x1b[201~1~echo injected\n",
            "\u{009b}201~echo injected\n",
            "safe\x08\x7f\x15\x04\x00hidden",
        ] {
            let normalized = normalize_paste(text);
            assert!(!normalized.chars().any(|c| c.is_control() && c != '\r'));
            assert!(!normalized.contains("\x1b[201~"));
            assert_eq!(normalize_paste(&normalized), normalized);
        }
    }

    #[test]
    fn clipboard_preserves_text_tabs_and_normalizes_line_endings() {
        assert_eq!(
            normalize_paste("ไทย\ttext\r\nnext\nlast\r"),
            "ไทย\ttext\rnext\rlast\r"
        );
    }
}

fn rgb(color: &gdk::RGBA) -> Rgb {
    (
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
    )
}

/// The font used when no setting names one.
pub fn default_font() -> pango::FontDescription {
    let family = crate::platform::current().monospace_family();
    pango::FontDescription::from_string(&format!("{family} 11"))
}

fn make_look(area: &TermArea, theme: &TerminalTheme) -> Look {
    let base = theme.font.clone().unwrap_or_else(default_font);
    let variant = |bold: bool, italic: bool| {
        let mut font = base.clone();
        if bold && font.weight() < pango::Weight::Bold {
            font.set_weight(pango::Weight::Bold);
        }
        if italic {
            font.set_style(pango::Style::Italic);
        }
        font
    };
    let fonts = [
        base.clone(),
        variant(true, false),
        variant(false, true),
        variant(true, true),
    ];

    let layout = pango::Layout::new(&area.pango_context());
    layout.set_font_description(Some(&base));
    layout.set_text(&"M".repeat(20));
    let (_, logical) = layout.extents();
    let scale = pango::SCALE as f64;
    let cell_w = (logical.width() as f64 / scale / 20.0).max(1.0);
    let cell_h = (logical.height() as f64 / scale).ceil().max(1.0);
    let baseline = layout.baseline() as f64 / scale;

    let ansi = theme.ansi_colors();
    let palette = std::array::from_fn(|i| match i {
        0..16 => rgb(&ansi[i]),
        16..232 => {
            let i = i - 16;
            let level = |v: usize| {
                if v == 0 {
                    0.0
                } else {
                    (55 + v * 40) as f64 / 255.0
                }
            };
            (level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        _ => {
            let gray = (8 + (i - 232) * 10) as f64 / 255.0;
            (gray, gray, gray)
        }
    });
    let (fg, bg) = (rgb(&theme.foreground()), rgb(&theme.background()));
    Look {
        fonts,
        fg,
        bg,
        palette,
        theme_fg: fg,
        theme_bg: bg,
        theme_palette: palette,
        own_bg: false,
        cursor: None,
        cell_w,
        cell_h,
        baseline,
    }
}

fn from_u8((r, g, b): (u8, u8, u8)) -> Rgb {
    (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
}

fn to_u8(rgb: Rgb) -> (u8, u8, u8) {
    let c = |v: f64| (v * 255.0).round() as u8;
    (c(rgb.0), c(rgb.1), c(rgb.2))
}

impl Look {
    /// The theme's colors with those the program set on top.
    fn with_colors(&mut self, colors: &Colors) {
        for (i, color) in self.palette.iter_mut().enumerate() {
            *color = colors.palette[i].map_or(self.theme_palette[i], from_u8);
        }
        self.fg = colors.fg.map_or(self.theme_fg, from_u8);
        self.bg = colors.bg.map_or(self.theme_bg, from_u8);
        self.own_bg = colors.bg.is_some();
        self.cursor = colors.cursor.map(from_u8);
    }

    fn resolve(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Indexed(i) => self.palette[i as usize],
            Color::Rgb(r, g, b) => (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0),
        }
    }

    /// Foreground and background (`None`: transparent) of a cell.
    fn colors(&self, cell: &GridCell, selected: bool) -> (Rgb, Option<Rgb>) {
        let style = cell.style;
        let mut fg = self.resolve(style.fg, self.fg);
        let mut bg = match style.bg {
            Color::Default => None,
            color => Some(self.resolve(color, self.bg)),
        };
        if (style.attrs & attr::INVERSE != 0) != selected {
            let old_fg = fg;
            fg = bg.unwrap_or(self.bg);
            bg = Some(old_fg);
        }
        if style.attrs & attr::DIM != 0 {
            let back = bg.unwrap_or(self.bg);
            fg = (
                fg.0 * 0.6 + back.0 * 0.4,
                fg.1 * 0.6 + back.1 * 0.4,
                fg.2 * 0.6 + back.2 * 0.4,
            );
        }
        (fg, bg)
    }

    fn font_for(&self, attrs: u16) -> &pango::FontDescription {
        &self.fonts[attr_font(attrs)]
    }
}

/// Which of `Look::fonts` the attributes call for.
fn attr_font(attrs: u16) -> usize {
    let bold = attrs & attr::BOLD != 0;
    let italic = attrs & attr::ITALIC != 0;
    bold as usize + 2 * italic as usize
}

/// Text runs laid out by Pango in the last frames, by font and text, so
/// lines that stay on screen (or only scroll) aren't shaped again. Colors
/// and positions come from Cairo, so a layout fits wherever the text
/// shows up. Keeps what the last two frames drew and nothing else.
#[derive(Default)]
struct Shaped {
    current: [HashMap<String, pango::Layout>; 4],
    previous: [HashMap<String, pango::Layout>; 4],
    /// The Pango context's serial the layouts were made with; they're
    /// dropped when it changes (resolution, font options).
    serial: u32,
}

impl Shaped {
    fn layout(
        &mut self,
        context: &pango::Context,
        look: &Look,
        attrs: u16,
        text: &str,
    ) -> pango::Layout {
        let font = attr_font(attrs);
        if let Some(layout) = self.current[font].get(text) {
            return layout.clone();
        }
        let layout = self.previous[font].remove(text).unwrap_or_else(|| {
            let layout = pango::Layout::new(context);
            layout.set_font_description(Some(&look.fonts[font]));
            layout.set_text(text);
            layout
        });
        self.current[font].insert(text.to_string(), layout.clone());
        layout
    }

    /// Starts a frame: forgets what the frame before last drew, and
    /// everything if the context changed.
    fn begin_frame(&mut self, context: &pango::Context) {
        if context.serial() != self.serial {
            self.clear();
            self.serial = context.serial();
        }
        self.previous = std::mem::take(&mut self.current);
    }

    fn clear(&mut self) {
        self.current = Default::default();
        self.previous = Default::default();
    }
}

/// An underline in its style, from `y` down, staying above `bottom` (the
/// next row).
/// Curly, dotted and dashed underlines (straight ones are rectangles).
fn draw_underline(cr: &cairo::Context, attrs: u16, x: f64, width: f64, y: f64, bottom: f64) {
    if attrs & attr::CURLY_UNDERLINE != 0 {
        // A wave: one up-and-down every 4 px, 1.5 px either way.
        let amplitude = 1.5;
        let mid = (y + amplitude).min(bottom - amplitude - 0.5);
        cr.set_line_width(1.0);
        cr.move_to(x, mid);
        let mut at = x;
        while at < x + width {
            let next = (at + 2.0).min(x + width);
            let up = if ((at - x) / 2.0) as i64 % 2 == 0 {
                -1.0
            } else {
                1.0
            };
            cr.curve_to(
                at + 1.0,
                mid + up * amplitude * 2.0,
                at + 1.0,
                mid,
                next,
                mid,
            );
            at = next;
        }
        let _ = cr.stroke();
        return;
    }
    if attrs & (attr::DOTTED_UNDERLINE | attr::DASHED_UNDERLINE) != 0 {
        let dashes: &[f64] = if attrs & attr::DOTTED_UNDERLINE != 0 {
            &[1.0, 2.0]
        } else {
            &[4.0, 3.0]
        };
        cr.set_dash(dashes, 0.0);
        cr.set_line_width(1.0);
        cr.move_to(x, y + 0.5);
        cr.line_to(x + width, y + 0.5);
        let _ = cr.stroke();
    }
}

fn ordered(selection: (Pos, Pos)) -> (Pos, Pos) {
    if selection.0 <= selection.1 {
        selection
    } else {
        (selection.1, selection.0)
    }
}

impl Inner {
    fn area(&self) -> Option<TermArea> {
        self.area.upgrade()
    }

    fn queue_draw(&self) {
        if let Some(area) = self.area() {
            area.queue_draw();
        }
    }

    fn apply_look_to_term(&self) {
        let look = self.look.borrow();
        let mut term = self.term.borrow_mut();
        term.set_cell_px(look.cell_w, look.cell_h);
        term.report_fg = to_u8(look.theme_fg);
        term.report_bg = to_u8(look.theme_bg);
        term.report_palette = look.theme_palette.map(to_u8);
    }

    /// Input for the program, from the user or on their behalf.
    fn write(&self, bytes: &[u8]) {
        self.last_input.set(Some(Instant::now()));
        self.respond(bytes);
    }

    /// Bytes for the program that it asked for (query answers).
    fn respond(&self, bytes: &[u8]) {
        if let Some(pty) = self.pty.borrow_mut().as_mut()
            && let Err(error) = pty.write(bytes)
            && let Some(area) = self.area()
        {
            area.error_bell();
            area.set_tooltip_text(Some(&format!("Input was not sent: {error}")));
        }
    }

    /// Input from the user: also jumps back to the bottom of the output.
    fn send_input(self: &Rc<Self>, bytes: &[u8]) {
        if self.view_offset.replace(0) != 0 {
            self.queue_draw();
        }
        self.write(bytes);
        self.restart_blink();
    }

    /// What screen readers read: the lines on screen, and the cursor's
    /// place in them (a character offset).
    fn screen_text(&self) -> (String, u32) {
        let term = self.term.borrow();
        let (cols, rows) = term.size();
        let top = term.scrolled_off() - self.view_offset.get().min(term.scrollback_len()) as i64;
        let cursor = term.cursor();
        let mut text = String::new();
        let mut caret = 0;
        for y in 0..rows {
            let Some(row) = term.line(top + y as i64) else {
                continue;
            };
            if let Some((row_at, col)) = cursor
                && row_at == y
            {
                caret = text.chars().count() + row.text(0, col).chars().count();
            }
            text.push_str(&row.text(0, cols));
            text.push('\n');
        }
        (text, caret as u32)
    }

    /// Tells screen readers about new output, soon (not for every chunk).
    fn schedule_accessible_update(&self) {
        if self.accessible_pending.replace(true) {
            return;
        }
        let (area, pending) = (self.area.clone(), self.accessible_pending.clone());
        glib::timeout_add_local_once(ACCESSIBLE_UPDATE, move || {
            pending.set(false);
            if let Some(area) = area.upgrade() {
                area.text_changed();
            }
        });
    }

    fn cursor_blinks(&self) -> bool {
        let wanted = self.term.borrow().modes.cursor_blink;
        wanted.unwrap_or(self.blink_default.get())
    }

    /// Shows the cursor and, while focused, blinks it from now on (the
    /// program can turn blinking on or off meanwhile).
    fn restart_blink(self: &Rc<Self>) {
        if let Some(timer) = self.blink_timer.take() {
            timer.remove();
        }
        if self.blink_hidden.replace(false) {
            self.queue_draw();
        }
        if !self.focused.get() {
            return;
        }
        let started = Instant::now();
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local(BLINK_INTERVAL, move || {
            let Some(inner) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let blink = inner.cursor_blinks() && started.elapsed() < BLINK_FOR;
            if blink || inner.blink_hidden.get() {
                inner.blink_hidden.set(blink && !inner.blink_hidden.get());
                inner.queue_draw();
            }
            if started.elapsed() >= BLINK_FOR {
                // Over: this source ends itself, so it's no longer removed.
                inner.blink_timer.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.blink_timer.replace(Some(timer));
    }

    fn paste(self: &Rc<Self>, text: &str) {
        let text = normalize_paste(text);
        // Without bracketed paste, every line runs as soon as it arrives:
        // ask first, in case the clipboard holds more than expected.
        let lines = text.matches('\r').count();
        let window = self
            .area()
            .and_then(|a| a.root())
            .and_downcast::<gtk::Window>();
        if self.term.borrow().modes.bracketed_paste || lines == 0 || window.is_none() {
            self.send_paste(&text);
            self.emit_input(&Input::Paste(text));
            return;
        }
        let preview: String = text
            .chars()
            .take(300)
            .collect::<String>()
            .replace('\r', "\n");
        let dialog = gtk::AlertDialog::builder()
            .message(format!(
                "Paste {} line{}?",
                lines,
                if lines == 1 { "" } else { "s" }
            ))
            .detail(format!(
                "This program doesn't protect against pasted text, so each line will run \
                 like a command as soon as it is pasted:\n\n{preview}"
            ))
            .buttons(["Cancel", "Paste"])
            .cancel_button(0)
            .default_button(0)
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        dialog.choose(window.as_ref(), gio::Cancellable::NONE, move |answer| {
            if let (Some(inner), Ok(1)) = (weak.upgrade(), answer) {
                inner.send_paste(&text);
                inner.emit_input(&Input::Paste(text));
            }
        });
    }

    /// Sends pasted text (from `normalize_paste`), marked as pasted when
    /// the program asked for that.
    fn send_paste(self: &Rc<Self>, text: &str) {
        // It replaces a selection made with the keyboard, as in a text field.
        self.delete_marked();
        if self.term.borrow().modes.bracketed_paste {
            self.send_input(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.send_input(text.as_bytes());
        }
    }

    /// The cursor as an absolute position, while it is shown.
    fn cursor_pos(&self) -> Option<Pos> {
        let term = self.term.borrow();
        term.cursor()
            .map(|(row, col)| (term.scrolled_off() + row as i64, col))
    }

    /// Selects from where Shift+arrow selecting started to the cursor.
    fn follow_marking(&self) {
        let (Some(anchor), Some(cursor)) = (self.marking.get(), self.cursor_pos()) else {
            return;
        };
        let cols = self.term.borrow().size().0;
        // The selection covers cells; the cursor is between characters.
        let before = |(row, col): Pos| match col {
            0 => (row - 1, cols.saturating_sub(1)),
            col => (row, col - 1),
        };
        self.selection.set(match cursor.cmp(&anchor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((cursor, before(anchor))),
            std::cmp::Ordering::Greater => Some((anchor, before(cursor))),
        });
        self.queue_draw();
    }

    /// Deletes a selection made with Shift and the arrow keys from the
    /// command line, with the program's own Backspace or Delete. Whether
    /// there was one.
    fn delete_marked(self: &Rc<Self>) -> bool {
        let (Some(anchor), Some((start, end)), Some(cursor)) =
            (self.marking.take(), self.selection.get(), self.cursor_pos())
        else {
            return false;
        };
        self.selection.set(None);
        self.queue_draw();
        let count = self.term.borrow().text_between(start, end).chars().count();
        // Delete isn't bound in every shell (zsh): step over the selection
        // when the cursor is at its start, then take it back with Backspace.
        let mut bytes = Vec::new();
        if cursor < anchor
            && let Some(right) = keys::encode(
                gdk::Key::Right,
                gdk::ModifierType::empty(),
                None,
                self.encoding(),
            )
        {
            bytes.extend(right.repeat(count));
        }
        bytes.extend(b"\x7f".repeat(count));
        self.send_input(&bytes);
        true
    }

    /// Shift+Left/Right select a character, with the word-jump modifier a
    /// word, at the shell's prompt; a selection made so is replaced by
    /// typing and deleted by Backspace or Delete. Whether the key was used.
    fn marking_key(self: &Rc<Self>, key: gdk::Key, state: gdk::ModifierType) -> bool {
        use gdk::ModifierType as M;
        let encoding = self.encoding();
        let Some(jumps) = encoding.word_jumps else {
            return false;
        };
        let mods =
            state & (M::SHIFT_MASK | M::CONTROL_MASK | M::ALT_MASK | M::SUPER_MASK | M::META_MASK);
        let word = match jumps {
            WordJumps::Alt => M::ALT_MASK,
            WordJumps::Ctrl => M::CONTROL_MASK,
        };
        let arrow = matches!(
            key,
            gdk::Key::Left | gdk::Key::KP_Left | gdk::Key::Right | gdk::Key::KP_Right
        );
        if arrow && (mods == M::SHIFT_MASK || mods == M::SHIFT_MASK | word) {
            if self.marking.get().is_none() {
                let Some(cursor) = self.cursor_pos() else {
                    return false;
                };
                self.selection.set(None);
                self.marking.set(Some(cursor));
            }
            // The plain movement; the selection follows the cursor.
            if let Some(bytes) = keys::encode(key, mods - M::SHIFT_MASK, None, encoding) {
                self.send_input(&bytes);
            }
            self.follow_marking();
            return true;
        }
        if self.marking.get().is_none() || is_modifier(key) {
            return false;
        }
        if mods.is_empty()
            && matches!(
                key,
                gdk::Key::BackSpace | gdk::Key::Delete | gdk::Key::KP_Delete
            )
        {
            if self.delete_marked() {
                return true;
            }
        } else if key == gdk::Key::Escape && mods.is_empty() {
            self.clear_marking();
            return true;
        }
        // Any other key ends selecting; text typed replaces the selection
        // (see the input method's commit).
        if key.to_unicode().is_none_or(char::is_control) || !(mods - M::SHIFT_MASK).is_empty() {
            self.clear_marking();
        }
        false
    }

    fn clear_marking(&self) {
        if self.marking.take().is_some() {
            self.selection.set(None);
            self.queue_draw();
        }
    }

    fn emit_input(&self, input: &Input) {
        if let Some(f) = self.on_input.borrow().as_ref() {
            f(input);
        }
    }

    fn encoding(&self) -> Encoding {
        let term = self.term.borrow();
        Encoding {
            app_cursor: term.modes.app_cursor,
            kitty_flags: term.keyboard.kitty_flags(term.alt_active()),
            modify_other_keys: term.keyboard.modify_other_keys,
            // Full-screen programs read the keys themselves; the shell's
            // prompt is on the main screen.
            word_jumps: (!term.alt_active()).then(WordJumps::for_platform),
        }
    }

    fn relayout(self: &Rc<Self>, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        let (cell_w, cell_h) = {
            let look = self.look.borrow();
            (look.cell_w, look.cell_h)
        };
        let cols = (((width as f64 - GUTTER) / cell_w).floor() as usize).max(1);
        let rows = ((height as f64 / cell_h).floor() as usize).max(1);
        self.term.borrow_mut().resize(cols, rows);
        let size = PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: (cols as f64 * cell_w) as u16,
            pixel_height: (rows as f64 * cell_h) as u16,
        };
        if let Some(pty) = self.pty.borrow().as_ref() {
            self.last_input.set(Some(Instant::now()));
            pty.resize(size);
        }
        let request = self.spawn.borrow_mut().take();
        if let Some(request) = request {
            self.start(request, size);
        }
        self.queue_draw();
    }

    fn start(self: &Rc<Self>, request: SpawnRequest, size: PtySize) {
        let env: Vec<(&str, &str)> = request
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        match pty::spawn(&request.argv, &request.cwd, &env, size) {
            Ok((pty, events)) => {
                self.pty.replace(Some(pty));
                if !request.input.is_empty() {
                    self.write(request.input.as_bytes());
                }
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    while let Ok(first) = events.recv().await {
                        let Some(inner) = weak.upgrade() else { break };
                        // Take everything that has arrived meanwhile in one go:
                        // big outputs arrive in many small reads.
                        let mut output = Vec::new();
                        let mut exited = false;
                        let mut event = Some(first);
                        while let Some(next) = event {
                            match next {
                                Event::Output(bytes) => output.extend_from_slice(&bytes),
                                Event::Exited => exited = true,
                            }
                            event = if exited || output.len() >= MAX_BATCH_BYTES {
                                None
                            } else {
                                events.try_recv().ok()
                            };
                        }
                        if !output.is_empty() {
                            inner.on_output(&output);
                        }
                        if exited {
                            if let Some(on_exit) = inner.on_exit.borrow().as_ref() {
                                on_exit();
                            }
                            break;
                        }
                        // A continuously ready channel must not monopolize
                        // the GTK executor. Release the terminal before yielding.
                        drop(inner);
                        glib::timeout_future(std::time::Duration::from_millis(1)).await;
                    }
                });
            }
            Err(err) => {
                // Only the program: arguments such as an agent's brief span lines.
                let program = request.argv.first().map_or("", String::as_str);
                let message = format!(
                    "soi: could not start {program}: {}\r\n",
                    err.replace('\n', "\r\n")
                );
                self.on_output(message.as_bytes());
            }
        }
    }

    fn on_output(&self, bytes: &[u8]) {
        self.output_serial.set(self.output_serial.get() + 1);
        let (responses, title, attention, copied, finished, progress, colors, scrolled, switched) = {
            let mut term = self.term.borrow_mut();
            let before = term.scrolled_off();
            let alt = term.alt_active();
            self.feeder.borrow_mut().feed(&mut term, bytes);
            (
                term.take_responses(),
                term.take_title(),
                term.take_attention(),
                term.take_copied(),
                term.take_finished(),
                term.take_progress(),
                term.take_colors(),
                term.scrolled_off() - before,
                term.alt_active() != alt,
            )
        };
        // Like xterm: a selection belongs to the screen it was made on, so a
        // full-screen program (an agent, an editor) starts without one.
        if switched && self.browse.borrow().is_none() {
            self.selection.set(None);
            self.drag_anchor.set(None);
        }
        self.respond(&responses);
        if switched {
            self.marking.set(None);
        } else {
            self.follow_marking();
        }
        if let Some(colors) = colors {
            self.look.borrow_mut().with_colors(&colors);
        }
        if let (Some(copy), Some(area)) = (copied, self.area()) {
            if copy.clipboard {
                area.clipboard().set_text(&copy.text);
            }
            if copy.primary {
                area.primary_clipboard().set_text(&copy.text);
            }
        }
        let now = Instant::now();
        if self
            .last_input
            .get()
            .is_none_or(|input| now.duration_since(input) > ECHO_WINDOW)
        {
            self.last_activity.set(Some(now));
        }
        // Keep the view where it is while the user reads the scrollback.
        let offset = self.view_offset.get();
        if offset > 0 && scrolled > 0 {
            let max = self.term.borrow().scrollback_len();
            self.view_offset.set((offset + scrolled as usize).min(max));
        }
        if let Some(title) = &title
            && let Some(area) = self.area()
        {
            area.update_property(&[gtk::accessible::Property::Label(title)]);
        }
        self.schedule_accessible_update();
        if let Some(title) = title
            && let Some(on_title) = self.on_title.borrow().as_ref()
        {
            on_title(&title);
        }
        if let Some(text) = attention
            && let Some(on_attention) = self.on_attention.borrow().as_ref()
        {
            on_attention(&text);
        }
        if let Some(finished) = finished
            && let Some(on_finished) = self.on_finished.borrow().as_ref()
        {
            on_finished(&finished);
        }
        if let Some(progress) = progress
            && let Some(on_progress) = self.on_progress.borrow().as_ref()
        {
            on_progress(progress);
        }
        self.redraw_unless_synchronized();
    }

    /// While a program draws a frame (synchronized output, mode 2026) the
    /// screen isn't redrawn, so half-drawn frames don't flicker. A timeout
    /// redraws anyway, in case the program never finishes the frame.
    fn redraw_unless_synchronized(&self) {
        if !self.term.borrow().modes.synchronized {
            self.queue_draw();
            return;
        }
        if self.sync_timer.get() {
            return;
        }
        self.sync_timer.set(true);
        let weak = self.area.clone();
        let timer = self.sync_timer.clone();
        glib::timeout_add_local_once(SYNC_TIMEOUT, move || {
            timer.set(false);
            if let Some(area) = weak.upgrade() {
                area.queue_draw();
            }
        });
    }

    /// Like other Linux terminals: selecting also fills the primary
    /// selection, for middle-click paste.
    fn copy_to_primary(&self) {
        if let (Some((start, end)), Some(area)) = (self.selection.get(), self.area()) {
            let text = self.term.borrow().text_between(start, end);
            if !text.is_empty() {
                area.primary_clipboard().set_text(&text);
            }
        }
    }

    /// Cell under a point in widget coordinates.
    fn cell_at(&self, x: f64, y: f64) -> Pos {
        let look = self.look.borrow();
        let term = self.term.borrow();
        let (cols, rows) = term.size();
        let col = (((x - GUTTER) / look.cell_w).floor().max(0.0) as usize).min(cols - 1);
        let row = ((y / look.cell_h).floor().max(0.0) as usize).min(rows - 1);
        let top = term.scrolled_off() - self.view_offset.get() as i64;
        (top + row as i64, col)
    }

    fn scroll_view(&self, lines: isize) {
        let max = self.term.borrow().scrollback_len() as isize;
        let offset = (self.view_offset.get() as isize + lines).clamp(0, max);
        self.view_offset.set(offset as usize);
        self.queue_draw();
    }

    /// Updates the link under the pointer: underline, hand cursor, and a
    /// tooltip with the real target (OSC 8 links can show any text).
    fn update_hover(&self, x: f64, y: f64) {
        let (abs, col) = self.cell_at(x, y);
        let link = self.term.borrow().link_at(abs, col).map(|link| (abs, link));
        if *self.hovered_link.borrow() == link {
            return;
        }
        if let Some(area) = self.area() {
            match &link {
                Some((_, link)) => {
                    area.set_cursor_from_name(Some("pointer"));
                    area.set_tooltip_text(Some(&format!("{}\nCtrl+click to open", link.url)));
                }
                None => {
                    area.set_cursor_from_name(None);
                    area.set_tooltip_text(None);
                }
            }
        }
        self.hovered_link.replace(link);
        self.queue_draw();
    }

    fn clear_hover(&self) {
        if self.hovered_link.take().is_some() {
            if let Some(area) = self.area() {
                area.set_cursor_from_name(None);
                area.set_tooltip_text(None);
            }
            self.queue_draw();
        }
    }

    /// Opens the hovered link if it's safe; returns whether there was one.
    fn open_hovered_link(&self) -> bool {
        let Some((_, link)) = self.hovered_link.borrow().clone() else {
            return false;
        };
        self.open_url(&link.url);
        true
    }

    /// Opens a link if it's safe, else says why not in the tooltip.
    fn open_url(&self, url: &str) {
        let Some(area) = self.area() else { return };
        match links::check(url) {
            Ok(()) => {
                let window = area.root().and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(url).launch(
                    window.as_ref(),
                    gio::Cancellable::NONE,
                    |result| {
                        if let Err(err) = result {
                            crate::log::write(&format!("could not open link: {err}"));
                        }
                    },
                );
            }
            Err(reason) => {
                area.set_tooltip_text(Some(&format!("{url}\nNot opened: {reason}")));
            }
        }
    }

    // -- browse mode --------------------------------------------------------

    fn set_im_enabled(&self, enabled: bool) {
        if let Some(keys) = self.keys.borrow().as_ref() {
            if enabled {
                keys.set_im_context(Some(&self.im));
            } else {
                keys.set_im_context(None::<&gtk::IMContext>);
            }
        }
    }

    fn view_top(&self) -> i64 {
        self.term.borrow().scrolled_off() - self.view_offset.get() as i64
    }

    fn enter_browse(&self, search: bool) {
        if self.browse.borrow().is_none() {
            let browse = Browse::new(
                &self.term.borrow(),
                self.view_top(),
                self.view_offset.get() == 0,
            );
            self.browse.replace(Some(browse));
            self.selection.set(None);
            self.set_im_enabled(false);
        }
        if search {
            self.browse_key(browse::Key::Char('/'));
        }
        self.queue_draw();
    }

    fn exit_browse(&self) {
        self.browse.replace(None);
        self.selection.set(None);
        self.view_offset.set(0);
        self.set_im_enabled(true);
        self.queue_draw();
    }

    fn browse_key(&self, key: browse::Key) {
        let rows = self.term.borrow().size().1;
        let action = {
            let term = self.term.borrow();
            let mut browse = self.browse.borrow_mut();
            let Some(browse) = browse.as_mut() else {
                return;
            };
            let action = browse.handle(&term, key, self.view_top(), rows);
            self.selection.set(browse.selection(&term));
            action
        };
        match action {
            Action::None => self.follow_browse_cursor(),
            Action::Exit => self.exit_browse(),
            Action::Copy(text) => {
                if let Some(area) = self.area()
                    && !text.is_empty()
                {
                    area.clipboard().set_text(&text);
                }
                self.exit_browse();
            }
            Action::OpenLink(url) => self.open_url(&url),
        }
        self.queue_draw();
    }

    /// Scrolls so the browse cursor stays in view (above the status line).
    fn follow_browse_cursor(&self) {
        let Some(line) = self.browse.borrow().as_ref().map(|b| b.cursor.0) else {
            return;
        };
        let term = self.term.borrow();
        let rows = term.size().1 as i64;
        let usable = (rows - 1).max(1);
        let top = self.view_top();
        let new_top = if line < top {
            line
        } else if line >= top + usable {
            line - usable + 1
        } else {
            top
        };
        let offset = (term.scrolled_off() - new_top).clamp(0, term.scrollback_len() as i64);
        self.view_offset.set(offset as usize);
    }

    /// Reports a mouse event to the program (SGR encoding).
    fn report_mouse(&self, button: u32, pressed: bool, x: f64, y: f64, state: gdk::ModifierType) {
        let (sgr, reporting) = {
            let modes = self.term.borrow().modes;
            (modes.sgr_mouse, modes.mouse_reporting)
        };
        if !reporting || !sgr {
            return;
        }
        let (cell_w, cell_h) = {
            let look = self.look.borrow();
            (look.cell_w, look.cell_h)
        };
        let col = ((x - GUTTER) / cell_w).floor().max(0.0) as usize + 1;
        let row = (y / cell_h).floor().max(0.0) as usize + 1;
        let mut code = button;
        if state.contains(gdk::ModifierType::SHIFT_MASK) {
            code += 4;
        }
        if state.contains(gdk::ModifierType::ALT_MASK) {
            code += 8;
        }
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            code += 16;
        }
        let end = if pressed { 'M' } else { 'm' };
        self.write(format!("\x1b[<{code};{col};{row}{end}").as_bytes());
    }

    /// Hands GTK the frame as render nodes: rectangles and text runs,
    /// which it draws on the GPU (the few curved or dashed lines are
    /// drawn with Cairo, in nodes of their own).
    fn draw(&self, area: &TermArea, snapshot: &gtk::Snapshot) {
        let height = area.height();
        let term = self.term.borrow();
        let look = self.look.borrow();
        let (_, rows) = term.size();
        let offset = self.view_offset.get().min(term.scrollback_len());
        let top = term.scrolled_off() - offset as i64;
        let selection = self.selection.get().map(ordered);
        let is_selected =
            |pos: Pos| selection.is_some_and(|(start, end)| start <= pos && pos <= end);
        let (cw, ch) = (look.cell_w, look.cell_h);
        // The widget's Pango context, so text uses the same resolution and
        // font options the cell size was measured with.
        let context = area.pango_context();
        let layout = pango::Layout::new(&context);
        let mut shaped = self.shaped.borrow_mut();
        shaped.begin_frame(&context);
        let fill = |color: Rgb, x: f64, y: f64, w: f64, h: f64| {
            snapshot.append_color(&rgba(color, 1.0), &rect(x, y, w, h));
        };
        let text_at = |layout: &pango::Layout, color: Rgb, x: f64, y: f64| {
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x as f32, y as f32));
            snapshot.append_layout(layout, &rgba(color, 1.0));
            snapshot.restore();
        };
        if look.own_bg {
            snapshot.append_color(&rgba(look.bg, 1.0), &area.bounds());
        }
        // Cells start after the gutter.
        snapshot.save();
        snapshot.translate(&graphene::Point::new(GUTTER as f32, 0.0));

        for y in 0..rows {
            let abs = top + y as i64;
            let Some(row) = term.line(abs) else { continue };
            let py = y as f64 * ch;

            // Backgrounds, one rectangle per run of the same color.
            let mut run: Option<(usize, usize, Rgb)> = None;
            let fill_run = |run: Option<(usize, usize, Rgb)>| {
                if let Some((start, end, bg)) = run {
                    let x0 = (start as f64 * cw).floor();
                    let x1 = (end as f64 * cw).floor();
                    fill(bg, x0, py, x1 - x0, ch);
                }
            };
            for (x, cell) in row.cells.iter().enumerate() {
                let end = x + if cell.width == Width::Wide { 2 } else { 1 };
                match (look.colors(cell, is_selected((abs, x))).1, &mut run) {
                    (Some(bg), Some((_, run_end, run_bg))) if *run_bg == bg && *run_end == x => {
                        *run_end = end;
                    }
                    // A spacer's background is drawn by the wide cell before it.
                    (_, Some((_, run_end, _))) if cell.width == Width::Spacer && *run_end > x => {}
                    (bg, _) => fill_run(std::mem::replace(&mut run, bg.map(|bg| (x, end, bg)))),
                }
            }
            fill_run(run);

            // Text, in runs of plain ASCII with the same style; anything
            // else is drawn cell by cell so it stays on the grid.
            let simple =
                |c: &GridCell| c.ch.is_ascii() && c.extra.is_none() && c.width == Width::Narrow;
            let mut x = 0;
            while x < row.cells.len() {
                let cell = &row.cells[x];
                let attrs = cell.style.attrs;
                let decorated = attrs & (attr::UNDERLINE | attr::STRIKE) != 0;
                if cell.width == Width::Spacer
                    || attrs & attr::HIDDEN != 0
                    || (cell.ch == ' ' && cell.extra.is_none() && !decorated)
                {
                    x += 1;
                    continue;
                }
                let (fg, _) = look.colors(cell, is_selected((abs, x)));
                // On whole pixels, so neighbouring cells meet exactly.
                let (x0, x1) = ((x as f64 * cw).floor(), ((x + 1) as f64 * cw).floor());
                if cell.extra.is_none()
                    && let Some(pieces) = boxdraw::pieces(cell.ch, x1 - x0, ch)
                {
                    for (px, piece_y, w, h, alpha) in pieces {
                        let bounds = rect(x0 + px, py + piece_y, w, h);
                        snapshot.append_color(&rgba(fg, alpha), &bounds);
                    }
                    x += 1;
                    continue;
                }
                let start = x;
                let mut text = String::new();
                if simple(cell) {
                    while x < row.cells.len() {
                        let next = &row.cells[x];
                        if !simple(next)
                            || next.style.attrs != attrs
                            || next.style.underline != cell.style.underline
                            || look.colors(next, is_selected((abs, x))).0 != fg
                        {
                            break;
                        }
                        text.push(next.ch);
                        x += 1;
                    }
                } else {
                    cell.push_text(&mut text);
                    x += if cell.width == Width::Wide { 2 } else { 1 };
                }
                let run = shaped.layout(&context, &look, attrs, &text);
                text_at(&run, fg, start as f64 * cw, py);
                if decorated {
                    let baseline = run.baseline() as f64 / pango::SCALE as f64;
                    let (x0, width) = (start as f64 * cw, (x - start) as f64 * cw);
                    if attrs & attr::STRIKE != 0 {
                        fill(fg, x0, py + ch / 2.0, width, 1.0);
                    }
                    if attrs & attr::UNDERLINE != 0 {
                        let color = look.resolve(cell.style.underline, fg);
                        let y = py + baseline + 1.0;
                        if attrs & (attr::ANY_UNDERLINE & !attr::UNDERLINE) == 0 {
                            fill(color, x0, y, width, 1.0);
                        } else if attrs & attr::DOUBLE_UNDERLINE != 0 {
                            fill(color, x0, y, width, 1.0);
                            fill(color, x0, (y + 2.0).min(py + ch - 1.0), width, 1.0);
                        } else {
                            let cr = snapshot.append_cairo(&rect(x0, py, width, ch));
                            cr.set_source_rgb(color.0, color.1, color.2);
                            draw_underline(&cr, attrs, x0, width, y, py + ch);
                        }
                    }
                }
            }
        }

        // Commands that failed: a red mark in the gutter beside their prompt.
        for command in term.commands().rev().filter(|_| !term.alt_active()) {
            if command.prompt < top {
                break;
            }
            if command.failed() && command.prompt < top + rows as i64 {
                let y = (command.prompt - top) as f64 * ch;
                fill(look.palette[1], 1.0 - GUTTER, y, 2.0, ch);
            }
        }

        if let Some((abs, link)) = self.hovered_link.borrow().as_ref() {
            let y = (abs - top) as f64 * ch;
            if (0.0..rows as f64 * ch).contains(&y) {
                let width = (link.end - link.start) as f64 * cw;
                fill(
                    look.fg,
                    link.start as f64 * cw,
                    y + look.baseline + 1.0,
                    width,
                    1.0,
                );
            }
        }

        self.draw_images(snapshot, &term, top, (cw, ch), height);
        self.draw_browse(snapshot, &layout, &term, &look, top);

        if offset == 0
            && self.browse.borrow().is_none()
            && let Some((row, col)) = term.cursor()
        {
            let (x, y) = (col as f64 * cw, row as f64 * ch);
            let cell = term.line(top + row as i64).and_then(|r| r.cells.get(col));
            let (fg, bg) = cell
                .map(|c| look.colors(c, false))
                .unwrap_or((look.fg, None));
            let color = look.cursor.unwrap_or(fg);
            match (term.modes.cursor_shape, self.focused.get()) {
                (_, true) if self.blink_hidden.get() => {}
                (_, false) => outline(snapshot, color, 1.0, x, y, cw, ch),
                (CursorShape::Bar, true) => fill(color, x, y, 2.0, ch),
                (CursorShape::Underline, true) => fill(color, x, y + ch - 2.0, cw, 2.0),
                (CursorShape::Block, true) => {
                    fill(color, x, y, cw, ch);
                    if let Some(cell) = cell.filter(|c| c.ch != ' ') {
                        let mut text = String::new();
                        cell.push_text(&mut text);
                        layout.set_font_description(Some(look.font_for(cell.style.attrs)));
                        layout.set_text(&text);
                        text_at(&layout, bg.unwrap_or(look.bg), x, y);
                    }
                }
            }
        }
        snapshot.restore();
    }

    /// Search matches, the browse cursor and the status line.
    fn draw_browse(
        &self,
        snapshot: &gtk::Snapshot,
        layout: &pango::Layout,
        term: &Term,
        look: &Look,
        top: i64,
    ) {
        let browse = self.browse.borrow();
        let Some(browse) = browse.as_ref() else {
            return;
        };
        let (cw, ch) = (look.cell_w, look.cell_h);
        let rows = term.size().1;
        let yellow = look.palette[3];

        let pattern = match &browse.typing {
            Some((_, text)) if !text.is_empty() => Some(text.as_str()),
            Some(_) => None,
            None => browse.search.as_ref().map(|s| s.pattern.as_str()),
        };
        if let Some(pattern) = pattern {
            for y in 0..rows {
                let abs = top + y as i64;
                for (start, end) in browse::line_matches(term, abs, pattern) {
                    let current = (abs, start) == browse.cursor;
                    snapshot.append_color(
                        &rgba(yellow, if current { 0.7 } else { 0.35 }),
                        &rect(
                            start as f64 * cw,
                            y as f64 * ch,
                            (end - start) as f64 * cw,
                            ch,
                        ),
                    );
                }
            }
        }

        let (line, col) = browse.cursor;
        let cursor_row = line - top;
        if (0..rows as i64).contains(&cursor_row) {
            let (x, y) = (col as f64 * cw, cursor_row as f64 * ch);
            snapshot.append_color(&rgba(yellow, 0.45), &rect(x, y, cw, ch));
            outline(snapshot, yellow, 1.5, x, y, cw, ch);
        }

        // Status line on the bottom row, or the top one if the cursor is
        // on the bottom row.
        let status_row = if cursor_row == rows as i64 - 1 {
            0
        } else {
            rows - 1
        };
        let y = status_row as f64 * ch;
        let width = term.size().0 as f64 * cw;
        // Calm colors: the background tinted with the theme's blue, a thin
        // accent line on top, and normal text.
        let blue = look.palette[4];
        let mix = |a: f64, b: f64| a * 0.8 + b * 0.2;
        let tint = (
            mix(look.bg.0, blue.0),
            mix(look.bg.1, blue.1),
            mix(look.bg.2, blue.2),
        );
        snapshot.append_color(&rgba(tint, 1.0), &rect(0.0, y, width, ch));
        snapshot.append_color(&rgba(blue, 0.6), &rect(0.0, y, width, 1.0));
        layout.set_font_description(Some(&look.fonts[0]));
        layout.set_text(&format!(" {}", browse.status(term)));
        layout.set_width((width * pango::SCALE as f64) as i32);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        snapshot.save();
        snapshot.translate(&graphene::Point::new(0.0, y as f32));
        snapshot.append_layout(layout, &rgba(look.fg, 1.0));
        snapshot.restore();
        layout.set_width(-1);
        layout.set_ellipsize(pango::EllipsizeMode::None);
    }

    fn draw_images(
        &self,
        snapshot: &gtk::Snapshot,
        term: &Term,
        top: i64,
        cell: (f64, f64),
        height: i32,
    ) {
        let mut textures = self.textures.borrow_mut();
        textures.retain_placements(&term.graphics.placements);
        for placement in term
            .graphics
            .draw_order()
            .into_iter()
            .filter(|p| p.alt == term.alt_active())
        {
            let y = (placement.abs_row - top) as f64 * cell.1;
            if y > height as f64 || y + placement.height_px < 0.0 {
                continue;
            }
            let Some(texture) = textures.texture(&placement.image) else {
                continue;
            };
            let bounds = rect(
                placement.col as f64 * cell.0,
                y,
                placement.width_px,
                placement.height_px,
            );
            snapshot.append_scaled_texture(&texture, gtk::gsk::ScalingFilter::Linear, &bounds);
        }
    }
}

fn rgba(color: Rgb, alpha: f64) -> gdk::RGBA {
    gdk::RGBA::new(color.0 as f32, color.1 as f32, color.2 as f32, alpha as f32)
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> graphene::Rect {
    graphene::Rect::new(x as f32, y as f32, width as f32, height as f32)
}

/// A frame of `width` px just inside the box.
fn outline(snapshot: &gtk::Snapshot, color: Rgb, width: f64, x: f64, y: f64, w: f64, h: f64) {
    let outline = gtk::gsk::RoundedRect::from_rect(rect(x, y, w, h), 0.0);
    let width = width as f32;
    snapshot.append_border(&outline, &[width; 4], &[rgba(color, 1.0); 4]);
}

fn install_handlers(area: &TermArea, inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    area.set_draw_func(move |area, snapshot| {
        if let Some(inner) = weak.upgrade() {
            inner.draw(area, snapshot);
        }
    });
    let weak = Rc::downgrade(inner);
    area.connect_resize(move |width, height| {
        if let Some(inner) = weak.upgrade() {
            inner.relayout(width, height);
        }
    });

    // Keyboard: the input method gets keys first (dead keys, compose).
    let keys = gtk::EventControllerKey::new();
    keys.set_im_context(Some(&inner.im));
    let weak = Rc::downgrade(inner);
    keys.connect_key_pressed(move |controller, key, keycode, state| {
        let Some(inner) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if inner.browse.borrow().is_some() {
            if let Some(key) = browse_key(key, state) {
                inner.browse_key(key);
            }
            return glib::Propagation::Stop;
        }
        if inner.marking_key(key, state) {
            return glib::Propagation::Stop;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let rows = inner.term.borrow().size().1 as isize;
        match key {
            gdk::Key::Page_Up if shift => inner.scroll_view(rows - 1),
            gdk::Key::Page_Down if shift => inner.scroll_view(-(rows - 1)),
            gdk::Key::Home if shift => inner.scroll_view(isize::MAX / 2),
            gdk::Key::End if shift => inner.scroll_view(-(isize::MAX / 2)),
            _ => {
                let encoding = inner.encoding();
                // What the key types without modifiers (Shift+! → 1), which
                // the Kitty protocol reports.
                let base = controller
                    .widget()
                    .and_then(|w| {
                        w.display().translate_key(
                            keycode,
                            gdk::ModifierType::empty(),
                            controller.group() as i32,
                        )
                    })
                    .and_then(|(key, ..)| key.to_unicode());
                match keys::encode(key, state, base, encoding) {
                    Some(bytes) => {
                        inner.send_input(&bytes);
                        inner.emit_input(&Input::Key { key, state, base });
                    }
                    None => return glib::Propagation::Proceed,
                }
            }
        }
        glib::Propagation::Stop
    });
    inner.keys.replace(Some(keys.clone()));
    area.add_controller(keys);
    let weak = Rc::downgrade(inner);
    inner.im.connect_commit(move |_, text| {
        if let Some(inner) = weak.upgrade() {
            inner.delete_marked();
            inner.send_input(text.as_bytes());
            inner.emit_input(&Input::Text(text.to_string()));
        }
    });

    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(inner);
    focus.connect_enter(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.focused.set(true);
            if inner.term.borrow().modes.focus_events {
                inner.write(b"\x1b[I");
            }
            inner.im.focus_in();
            inner.queue_draw();
            inner.restart_blink();
        }
    });
    let weak = Rc::downgrade(inner);
    focus.connect_leave(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.focused.set(false);
            if inner.term.borrow().modes.focus_events {
                inner.write(b"\x1b[O");
            }
            inner.im.focus_out();
            inner.queue_draw();
            inner.restart_blink();
        }
    });
    area.add_controller(focus);

    // Mouse: clicks focus the terminal and clear the selection, or go to
    // the program when it asked for mouse events (Shift overrides that).
    let click = gtk::GestureClick::builder().button(0).build();
    let weak = Rc::downgrade(inner);
    click.connect_pressed(move |gesture, presses, x, y| {
        let Some(inner) = weak.upgrade() else { return };
        if let Some(area) = inner.area() {
            area.grab_focus();
        }
        inner.marking.set(None);
        let state = gesture.current_event_state();
        if gesture.current_button() == 1
            && state.contains(gdk::ModifierType::CONTROL_MASK)
            && inner.open_hovered_link()
        {
            return;
        }
        let reporting = inner.term.borrow().modes.mouse_reporting;
        if reporting && !state.contains(gdk::ModifierType::SHIFT_MASK) {
            let button = gesture.current_button().saturating_sub(1);
            inner.report_mouse(button, true, x, y, state);
            return;
        }
        match gesture.current_button() {
            1 if inner.browse.borrow().is_some() => {
                let pos = inner.cell_at(x, y);
                if let Some(browse) = inner.browse.borrow_mut().as_mut() {
                    browse.cursor = pos;
                }
                inner.queue_draw();
            }
            // Double-click: the word; triple-click: the whole line.
            1 if presses >= 2 => {
                let (abs, col) = inner.cell_at(x, y);
                let span = if presses == 2 {
                    inner.term.borrow().word_at(abs, col)
                } else {
                    Some(inner.term.borrow().line_span(abs))
                };
                inner.selection.set(span);
                inner.drag_anchor.set(None);
                inner.copy_to_primary();
                inner.queue_draw();
            }
            1 => {
                inner.selection.set(None);
                inner.queue_draw();
            }
            2 => {
                if let Some(area) = inner.area() {
                    let weak = Rc::downgrade(&inner);
                    area.primary_clipboard().read_text_async(
                        gio::Cancellable::NONE,
                        move |result| {
                            if let (Some(inner), Ok(Some(text))) = (weak.upgrade(), result) {
                                inner.paste(&text);
                            }
                        },
                    );
                }
            }
            _ => {}
        }
    });
    let weak = Rc::downgrade(inner);
    click.connect_released(move |gesture, _, x, y| {
        let Some(inner) = weak.upgrade() else { return };
        let state = gesture.current_event_state();
        if inner.term.borrow().modes.mouse_reporting
            && !state.contains(gdk::ModifierType::SHIFT_MASK)
        {
            let button = gesture.current_button().saturating_sub(1);
            inner.report_mouse(button, false, x, y, state);
        }
    });
    area.add_controller(click);

    let drag = gtk::GestureDrag::new();
    let weak = Rc::downgrade(inner);
    drag.connect_drag_begin(move |gesture, x, y| {
        let Some(inner) = weak.upgrade() else { return };
        let reporting = inner.term.borrow().modes.mouse_reporting;
        let state = gesture.current_event_state();
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let opening_link = state.contains(gdk::ModifierType::CONTROL_MASK)
            && inner.hovered_link.borrow().is_some();
        inner
            .drag_anchor
            .set(((!reporting || shift) && !opening_link).then(|| inner.cell_at(x, y)));
    });
    let weak = Rc::downgrade(inner);
    drag.connect_drag_update(move |gesture, dx, dy| {
        let Some(inner) = weak.upgrade() else { return };
        let (Some(anchor), Some((x, y))) = (inner.drag_anchor.get(), gesture.start_point()) else {
            return;
        };
        // Past the top or bottom edge, scroll so the selection can grow.
        let height = inner.area().map_or(0, |a| a.height()) as f64;
        if y + dy < 0.0 {
            inner.scroll_view(1);
        } else if y + dy > height {
            inner.scroll_view(-1);
        }
        let end = inner.cell_at(x + dx, y + dy);
        if end != anchor || inner.selection.get().is_some() {
            inner.selection.set(Some((anchor, end)));
            inner.queue_draw();
        }
    });
    let weak = Rc::downgrade(inner);
    drag.connect_drag_end(move |_, _, _| {
        if let Some(inner) = weak.upgrade() {
            inner.copy_to_primary();
        }
    });
    area.add_controller(drag);

    let motion = gtk::EventControllerMotion::new();
    let weak = Rc::downgrade(inner);
    motion.connect_motion(move |_, x, y| {
        if let Some(inner) = weak.upgrade() {
            inner.pointer.set((x, y));
            inner.update_hover(x, y);
        }
    });
    let weak = Rc::downgrade(inner);
    motion.connect_leave(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.clear_hover();
        }
    });
    area.add_controller(motion);

    // Wheel: scrollback on the normal screen; on the alternate screen
    // (less, vim) it becomes arrow keys or mouse wheel events.
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    let weak = Rc::downgrade(inner);
    scroll.connect_scroll(move |controller, _, dy| {
        let Some(inner) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let total = inner.scroll_remainder.get() + dy * 3.0;
        let lines = total.trunc() as isize;
        inner.scroll_remainder.set(total.fract());
        if lines == 0 {
            return glib::Propagation::Stop;
        }
        let (alt, reporting, app_cursor) = {
            let term = inner.term.borrow();
            (
                term.alt_active(),
                term.modes.mouse_reporting,
                term.modes.app_cursor,
            )
        };
        if reporting {
            let (x, y) = inner.pointer.get();
            let state = controller.current_event_state();
            let button = if lines < 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs() {
                inner.report_mouse(button, true, x, y, state);
            }
        } else if alt {
            let key = match (lines < 0, app_cursor) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            inner.write(key.repeat(lines.unsigned_abs()).as_bytes());
        } else {
            inner.scroll_view(-lines);
        }
        glib::Propagation::Stop
    });
    area.add_controller(scroll);
}

/// A key press in browse mode.
fn is_modifier(key: gdk::Key) -> bool {
    use gdk::Key;
    matches!(
        key,
        Key::Shift_L
            | Key::Shift_R
            | Key::Control_L
            | Key::Control_R
            | Key::Alt_L
            | Key::Alt_R
            | Key::Meta_L
            | Key::Meta_R
            | Key::Super_L
            | Key::Super_R
            | Key::Hyper_L
            | Key::Hyper_R
            | Key::ISO_Level3_Shift
            | Key::Caps_Lock
    )
}

fn browse_key(key: gdk::Key, state: gdk::ModifierType) -> Option<browse::Key> {
    use browse::Key as B;
    use gdk::Key;
    Some(match key {
        Key::Escape => B::Escape,
        Key::Return | Key::KP_Enter => B::Enter,
        Key::BackSpace => B::Backspace,
        Key::Up | Key::KP_Up => B::Up,
        Key::Down | Key::KP_Down => B::Down,
        Key::Left | Key::KP_Left => B::Left,
        Key::Right | Key::KP_Right => B::Right,
        Key::Page_Up | Key::KP_Page_Up => B::PageUp,
        Key::Page_Down | Key::KP_Page_Down => B::PageDown,
        Key::Home | Key::KP_Home => B::Home,
        Key::End | Key::KP_End => B::End,
        _ => {
            let c = key.to_unicode().filter(|c| !c.is_control())?;
            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                B::Ctrl(c.to_ascii_lowercase())
            } else {
                B::Char(c)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(serial: u64) -> Image {
        Image {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
            serial,
        }
    }

    #[test]
    fn regression_deleted_images_release_even_a_small_texture_cache() {
        let mut cache = TextureCache::default();
        for serial in 1..=8 {
            cache.texture(&pixel(serial)).unwrap();
        }
        assert_eq!(cache.bytes, 32);
        cache.retain_placements(&[]);
        assert_eq!(cache.bytes, 0);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn regression_texture_cache_has_an_independent_byte_budget() {
        let mut cache = TextureCache {
            limit: 8,
            ..Default::default()
        };
        cache.texture(&pixel(1)).unwrap();
        cache.texture(&pixel(2)).unwrap();
        cache.texture(&pixel(3)).unwrap();
        assert_eq!(cache.bytes, 8);
        assert!(!cache.entries.contains_key(&1));
        assert!(cache.entries.contains_key(&2));
        assert!(cache.entries.contains_key(&3));
        cache.texture(&pixel(3)).unwrap();
        assert_eq!(cache.bytes, 8, "cache hits must not duplicate storage");
        cache.clear();
        assert_eq!(cache.bytes, 0);
        assert!(cache.entries.is_empty());
    }
}

/// Timing checks: `cargo test --release perf -- --ignored --nocapture --test-threads=1`.
#[cfg(test)]
mod perf_tests {
    use super::*;
    use std::time::Instant;

    /// Lines like a colored build log or `ls`.
    fn sample_output(lines: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..lines {
            out.extend_from_slice(
                format!(
                    "\x1b[1;32m   Compiling\x1b[0m crate-{i} v0.{i}.0 (\x1b[34m/home/user/project/src/module_{i}.rs\x1b[0m) \x1b[38;2;250;179;135mwarning\x1b[0m: unused variable `x{i}`\r\n"
                )
                .as_bytes(),
            );
        }
        out
    }

    #[test]
    #[ignore]
    fn perf_parse() {
        let data = sample_output(400_000);
        let mut term = Term::new(200, 60, 10_000);
        let mut feeder = Feeder::default();
        let start = Instant::now();
        for chunk in data.chunks(64 * 1024) {
            feeder.feed(&mut term, chunk);
        }
        let secs = start.elapsed().as_secs_f64();
        println!(
            "parse: {:.1} MB in {secs:.2}s = {:.1} MB/s",
            data.len() as f64 / 1e6,
            data.len() as f64 / 1e6 / secs
        );
    }

    #[test]
    #[ignore]
    fn perf_draw() {
        gtk::init().unwrap();
        let theme = TerminalTheme::detect();
        let view = TermView::new(vec!["true".into()], Path::new("/"), &[], &theme, 1000);
        let (width, height) = (3440, 1400);
        {
            let look = view.inner.look.borrow();
            let cols = (width as f64 / look.cell_w) as usize;
            let rows = (height as f64 / look.cell_h) as usize;
            view.inner.term.borrow_mut().resize(cols, rows);
            println!("grid {cols}x{rows}");
        }
        view.inner.on_output(&sample_output(200));
        let area = &view.area;
        // Sized as on screen, without a window.
        area.size_allocate(&gtk::Allocation::new(0, 0, width, height), -1);
        let draw = || {
            let snapshot = gtk::Snapshot::new();
            view.inner.draw(area, &snapshot);
            snapshot.to_node()
        };
        let frames = 30;
        let start = Instant::now();
        for _ in 0..frames {
            draw();
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        println!("draw: {ms:.1} ms per full frame (render nodes)");

        // New lines before every frame, as while a build is printing; then
        // through GTK's renderer, as on screen (and read back, which the
        // screen doesn't need).
        let output = sample_output(2000);
        let lines: Vec<&[u8]> = output.split_inclusive(|&b| b == b'\n').collect();
        let (frames, per_frame) = (60, 10);
        let renderer = gtk::gsk::GLRenderer::new();
        renderer
            .realize_for_display(&gdk::Display::default().unwrap())
            .unwrap();
        let bounds = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
        let mut nodes_ms = 0.0;
        let start = Instant::now();
        for frame in 0..frames {
            for line in &lines[frame * per_frame..(frame + 1) * per_frame] {
                view.inner.on_output(line);
            }
            let built = Instant::now();
            let node = draw().unwrap();
            nodes_ms += built.elapsed().as_secs_f64() * 1000.0;
            let _ = renderer.render_texture(&node, Some(&bounds));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        println!(
            "while scrolling ({per_frame} new lines): {:.1} ms to build, {ms:.1} ms rendered",
            nodes_ms / frames as f64
        );
        renderer.unrealize();
    }
}
