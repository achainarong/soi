//! The plan beside the terminal, a live Markdown editor; and native Markdown
//! rendering for Task details. HTML and link targets are not executed; images
//! use their alt text and never trigger filesystem/network fetches.
use super::*;
use std::ops::Range;

use crate::markdown::Style;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub(super) fn markup(markdown: &str) -> String {
    let markdown: String = markdown
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' && c != '\r' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect();
    let mut result = String::new();
    for event in Parser::new_ext(
        &markdown,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES,
    ) {
        match event {
            Event::Start(Tag::Heading { .. }) => result.push_str("<big><b>"),
            Event::End(TagEnd::Heading(_)) => result.push_str("</b></big>\n\n"),
            Event::Start(Tag::Strong) => result.push_str("<b>"),
            Event::End(TagEnd::Strong) => result.push_str("</b>"),
            Event::Start(Tag::Emphasis) => result.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => result.push_str("</i>"),
            Event::Start(Tag::Strikethrough) => result.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => result.push_str("</s>"),
            Event::Start(Tag::CodeBlock(_)) => result.push_str("<tt>"),
            Event::End(TagEnd::CodeBlock) => result.push_str("</tt>\n"),
            Event::Code(text) => {
                result.push_str("<tt>");
                result.push_str(&glib::markup_escape_text(&text));
                result.push_str("</tt>");
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                result.push_str(&glib::markup_escape_text(&text))
            }
            Event::Start(Tag::Item) => result.push_str("• "),
            Event::End(TagEnd::Paragraph | TagEnd::Item | TagEnd::TableRow) | Event::HardBreak => {
                result.push('\n')
            }
            Event::End(TagEnd::TableCell) => result.push_str("    "),
            Event::SoftBreak => result.push(' '),
            Event::Rule => result.push_str("\n────────────\n"),
            Event::TaskListMarker(done) => result.push_str(if done { "☑ " } else { "☐ " }),
            _ => {}
        }
    }
    result
}

pub(super) fn toggle(state: &App) {
    if let Some(ws) = active_workspace(state) {
        toggle_for(state, &ws);
    }
}

pub(super) fn toggle_for(state: &App, ws: &Rc<WorkspaceUi>) {
    if ws.plan_split.end_child().is_some() {
        close_panel(ws);
        return;
    }
    let Some(path) = ws.task.borrow().as_ref().and_then(|t| t.plan.clone()) else {
        gtk::AlertDialog::builder()
            .message("Create or link a plan in Tasks → Task details first.")
            .build()
            .show(Some(&state.window));
        return;
    };
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.set_widget_name("task-plan-preview");
    panel.add_css_class("soi-plan");
    panel.set_width_request(PLAN_MIN_WIDTH);
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    title.add_css_class("soi-toolbar");
    let label = gtk::Label::builder()
        .label(path_text(&path))
        .ellipsize(pango::EllipsizeMode::Middle)
        .hexpand(true)
        .xalign(0.0)
        .tooltip_text(path_text(&path))
        .build();
    title.append(&label);
    let status = gtk::Label::builder()
        .css_classes(["dim-label", "caption"])
        .build();
    title.append(&status);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("flat");
    close.set_tooltip_text(Some("Close the plan (it saves first)"));
    title.append(&close);
    panel.append(&title);

    // Shown when the file changed on disk while there were unsaved edits.
    let conflict = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    conflict.add_css_class("soi-plan-conflict");
    conflict.set_margin_start(12);
    conflict.set_margin_end(12);
    conflict.set_margin_top(4);
    conflict.set_visible(false);
    conflict.append(
        &gtk::Label::builder()
            .label("The plan changed outside this editor.")
            .wrap(true)
            .hexpand(true)
            .xalign(0.0)
            .build(),
    );
    let theirs = gtk::Button::with_label("Load theirs");
    theirs.set_tooltip_text(Some("Drop your edits and load the version on disk"));
    let mine = gtk::Button::with_label("Keep mine");
    mine.set_tooltip_text(Some("Replace the version on disk with yours"));
    conflict.append(&theirs);
    conflict.append(&mine);
    panel.append(&conflict);

    let editor = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .editable(false)
        .left_margin(16)
        .right_margin(16)
        .top_margin(12)
        .bottom_margin(12)
        .pixels_below_lines(2)
        .build();
    editor.buffer().set_text("Loading plan…");
    panel.append(
        &gtk::ScrolledWindow::builder()
            .child(&editor)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
    );
    // A set position stays put: the plan doesn't follow what the terminals
    // would like, which changes with every title.
    ws.plan_split.set_end_child(Some(&panel));
    place_divider(ws);

    let plan = Rc::new(Plan {
        ws: Rc::downgrade(ws),
        editor,
        status,
        conflict,
        disk: RefCell::new(None),
        base: RefCell::new(None),
        loading: Cell::new(false),
        saving: Cell::new(false),
        edits: Cell::new(0),
        checkboxes: RefCell::new(Vec::new()),
        restyle_queued: Cell::new(false),
    });
    plan.create_tags(&state.shared.theme.borrow());
    ws.plan_panel.replace(Some(plan.clone()));

    let weak = Rc::downgrade(&plan);
    plan.editor.buffer().connect_changed(move |_| {
        if let Some(plan) = weak.upgrade() {
            plan.changed();
        }
    });
    let keys = gtk::EventControllerKey::new();
    // Before the text view, which would insert a plain newline or tab.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&plan);
    keys.connect_key_pressed(move |_, key, _, modifiers| match weak.upgrade() {
        Some(plan) => plan.key(key, modifiers),
        None => glib::Propagation::Proceed,
    });
    plan.editor.add_controller(keys);
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .build();
    let weak = Rc::downgrade(&plan);
    click.connect_pressed(move |gesture, presses, x, y| {
        if let Some(plan) = weak.upgrade()
            && presses == 1
            && plan.toggle_checkbox_at(x, y)
        {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    plan.editor.add_controller(click);
    let weak = Rc::downgrade(&plan);
    theirs.connect_clicked(move |_| {
        if let Some(plan) = weak.upgrade() {
            plan.load_theirs();
        }
    });
    let weak = Rc::downgrade(&plan);
    mine.connect_clicked(move |_| {
        if let Some(plan) = weak.upgrade() {
            plan.keep_mine();
        }
    });
    let weak_ws = Rc::downgrade(ws);
    close.connect_clicked(move |_| {
        if let Some(ws) = weak_ws.upgrade() {
            close_panel(&ws);
        }
    });

    let busy = Rc::new(Cell::new(false));
    let update = {
        let weak = Rc::downgrade(&plan);
        move || {
            let Some(plan) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if plan.saving.get() || busy.replace(true) {
                return glib::ControlFlow::Continue;
            }
            let path = plan.path();
            let (weak, busy) = (Rc::downgrade(&plan), busy.clone());
            tasks::background(
                move || {
                    path.ok_or_else(|| "No plan linked.".into())
                        .and_then(|p| crate::plan::read(&p))
                },
                move |result| {
                    busy.set(false);
                    if let Some(plan) = weak.upgrade() {
                        plan.loaded(result);
                    }
                },
            );
            glib::ControlFlow::Continue
        }
    };
    update();
    glib::timeout_add_local(std::time::Duration::from_secs(1), update);
}

/// How wide the plan opens at first, and how narrow it can be dragged.
pub(super) const PLAN_WIDTH: i32 = 380;
const PLAN_MIN_WIDTH: i32 = 240;

/// How long typing pauses before the plan is saved.
const AUTOSAVE: std::time::Duration = std::time::Duration::from_millis(700);

/// The plan beside the terminal: always editable Markdown, styled as it is
/// typed, and saved shortly after typing stops. Saving refuses to replace a
/// version of the file the editor hasn't seen.
pub(super) struct Plan {
    ws: Weak<WorkspaceUi>,
    editor: gtk::TextView,
    status: gtk::Label,
    conflict: gtk::Box,
    /// The file as last read or written.
    disk: RefCell<Option<String>>,
    /// The file as the editor's text started from; saving expects it on disk.
    base: RefCell<Option<String>>,
    /// The text is being replaced by soi, not typed.
    loading: Cell<bool>,
    saving: Cell<bool>,
    /// Counts edits, so only the last one in a burst saves.
    edits: Cell<u64>,
    /// Where the clickable `[ ]`/`[x]` are, in characters.
    checkboxes: RefCell<Vec<Range<i32>>>,
    restyle_queued: Cell<bool>,
}

impl Plan {
    fn path(&self) -> Option<PathBuf> {
        self.ws
            .upgrade()
            .and_then(|ws| ws.task.borrow().as_ref().and_then(|t| t.plan.clone()))
    }

    pub(super) fn buffer(&self) -> gtk::TextBuffer {
        self.editor.buffer()
    }

    pub(super) fn text(&self) -> String {
        let buffer = self.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .into()
    }

    fn dirty(&self) -> bool {
        self.base
            .borrow()
            .as_deref()
            .is_some_and(|base| base != self.text())
    }

    fn note(&self, text: &str) {
        self.status.set_label(text);
    }

    fn create_tags(&self, theme: &crate::theme::TerminalTheme) {
        let table = self.buffer().tag_table();
        let add = |tag: gtk::TextTag| {
            table.add(&tag);
        };
        for (level, scale) in [(1, 1.6), (2, 1.35), (3, 1.2), (4, 1.1), (5, 1.0), (6, 1.0)] {
            add(gtk::TextTag::builder()
                .name(format!("h{level}"))
                .scale(scale)
                .weight(700)
                .pixels_above_lines(6)
                .build());
        }
        add(gtk::TextTag::builder().name("strong").weight(700).build());
        add(gtk::TextTag::builder()
            .name("emphasis")
            .style(pango::Style::Italic)
            .build());
        add(gtk::TextTag::builder()
            .name("strike")
            .strikethrough(true)
            .build());
        add(gtk::TextTag::builder()
            .name("code")
            .family("monospace")
            .build());
        add(gtk::TextTag::builder()
            .name("codeblock")
            .family("monospace")
            .build());
        add(gtk::TextTag::builder()
            .name("link")
            .underline(pango::Underline::Single)
            .build());
        add(gtk::TextTag::builder()
            .name("quote")
            .style(pango::Style::Italic)
            .build());
        add(gtk::TextTag::builder().name("bullet").weight(700).build());
        add(gtk::TextTag::builder()
            .name("checkbox")
            .weight(700)
            .family("monospace")
            .build());
        // Last, so markup characters stay faint inside links and headings.
        add(gtk::TextTag::builder().name("syntax").build());
        self.recolor(theme);
    }

    /// Gives the tags the theme's colors; again whenever the theme changes.
    pub(super) fn recolor(&self, theme: &crate::theme::TerminalTheme) {
        let fg = theme.foreground();
        let accent = theme.ansi_colors()[4];
        let faint = |alpha: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), alpha);
        let table = self.buffer().tag_table();
        #[expect(
            clippy::expect_used,
            reason = "every name used here is created in create_tags"
        )]
        let tag = |name: &str| table.lookup(name).expect("created in create_tags");
        tag("code").set_background_rgba(Some(&faint(0.10)));
        tag("codeblock").set_paragraph_background_rgba(Some(&faint(0.06)));
        tag("quote").set_foreground_rgba(Some(&faint(0.75)));
        for name in ["link", "bullet", "checkbox"] {
            tag(name).set_foreground_rgba(Some(&accent));
        }
        tag("syntax").set_foreground_rgba(Some(&faint(0.38)));
    }

    /// Restyles the text after the current batch of changes.
    fn queue_restyle(self: &Rc<Self>) {
        if self.restyle_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(plan) = weak.upgrade() {
                plan.restyle_queued.set(false);
                plan.restyle();
            }
        });
    }

    fn restyle(&self) {
        let text = self.text();
        // Byte offsets to character offsets, for the buffer's iterators.
        let mut chars = vec![0i32; text.len() + 1];
        let mut count = 0;
        for (at, c) in text.char_indices() {
            chars[at..at + c.len_utf8()].fill(count);
            count += 1;
        }
        chars[text.len()] = count;
        let buffer = self.buffer();
        buffer.remove_all_tags(&buffer.start_iter(), &buffer.end_iter());
        let mut checkboxes = Vec::new();
        for span in crate::markdown::spans(&text) {
            let (start, end) = (chars[span.range.start], chars[span.range.end]);
            let name = match span.style {
                Style::Heading(level) => format!("h{}", level.clamp(1, 6)),
                Style::Strong => "strong".into(),
                Style::Emphasis => "emphasis".into(),
                Style::Strike => "strike".into(),
                Style::Code => "code".into(),
                Style::CodeBlock => "codeblock".into(),
                Style::Link => "link".into(),
                Style::Quote => "quote".into(),
                Style::Syntax => "syntax".into(),
                Style::Bullet => "bullet".into(),
                Style::Checkbox(_) => {
                    checkboxes.push(start..end);
                    "checkbox".into()
                }
            };
            buffer.apply_tag_by_name(
                &name,
                &buffer.iter_at_offset(start),
                &buffer.iter_at_offset(end),
            );
        }
        self.checkboxes.replace(checkboxes);
    }

    /// Replaces the text without it counting as an edit or an undo step.
    fn replace_text(self: &Rc<Self>, text: &str) {
        let buffer = self.buffer();
        let cursor = buffer.iter_at_mark(&buffer.get_insert()).offset();
        self.loading.set(true);
        buffer.begin_irreversible_action();
        buffer.set_text(text);
        buffer.end_irreversible_action();
        buffer.place_cursor(&buffer.iter_at_offset(cursor));
        self.loading.set(false);
        self.queue_restyle();
    }

    /// Takes in what was read from disk; a clean editor follows along.
    fn loaded(self: &Rc<Self>, result: Result<String, String>) {
        let text = match result {
            Ok(text) => text,
            Err(e) => {
                if self.disk.borrow().is_none() {
                    self.buffer().set_text(&e);
                }
                return self.note("Can't read the plan");
            }
        };
        if self.disk.borrow().as_ref() == Some(&text) {
            return;
        }
        self.disk.replace(Some(text.clone()));
        if self.base.borrow().is_none() {
            self.base.replace(Some(text.clone()));
            self.replace_text(&text);
            self.editor.set_editable(true);
            return;
        }
        if !self.dirty() {
            self.base.replace(Some(text.clone()));
            self.replace_text(&text);
            self.note("Updated from disk");
        }
        // With edits pending, the next save notices the change and asks.
    }

    fn changed(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        self.queue_restyle();
        if !self.dirty() {
            return;
        }
        self.note("Edited");
        let edit = self.edits.get() + 1;
        self.edits.set(edit);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(AUTOSAVE, move || {
            if let Some(plan) = weak.upgrade()
                && plan.edits.get() == edit
            {
                plan.write(|_| {});
            }
        });
    }

    /// Saves the text; `done` learns whether nothing is left unsaved.
    fn write(self: &Rc<Self>, done: impl FnOnce(bool) + 'static) {
        if !self.dirty() {
            return done(true);
        }
        if self.conflict.is_visible() || self.saving.get() {
            return done(false);
        }
        let Some(path) = self.path() else {
            self.note("No plan is linked any more");
            return done(false);
        };
        self.saving.set(true);
        self.note("Saving…");
        let contents = self.text();
        let base = self.base.borrow().clone();
        let (weak, written) = (Rc::downgrade(self), contents.clone());
        tasks::background(
            move || crate::plan::save(&path, &contents, base.as_deref()),
            move |result| {
                let Some(plan) = weak.upgrade() else { return };
                plan.saving.set(false);
                match result {
                    Ok(()) => {
                        plan.disk.replace(Some(written.clone()));
                        plan.base.replace(Some(written));
                        if plan.dirty() {
                            // Typed while saving: save that too.
                            plan.changed();
                            return done(false);
                        }
                        plan.note("Saved");
                        done(true);
                    }
                    Err(crate::plan::SaveError::Conflict(disk)) => {
                        plan.disk.replace(disk);
                        plan.conflict.set_visible(true);
                        plan.note("Not saved");
                        done(false);
                    }
                    Err(crate::plan::SaveError::Failed(e)) => {
                        plan.note("Not saved");
                        plan.status.set_tooltip_text(Some(&e));
                        done(false);
                    }
                }
            },
        );
    }

    fn load_theirs(self: &Rc<Self>) {
        let disk = self.disk.borrow().clone().unwrap_or_default();
        self.conflict.set_visible(false);
        self.base.replace(Some(disk.clone()));
        self.replace_text(&disk);
        self.note("Loaded the version on disk");
    }

    fn keep_mine(self: &Rc<Self>) {
        // Saving now expects the version on disk, so it replaces it.
        self.base.replace(self.disk.borrow().clone());
        self.conflict.set_visible(false);
        self.write(|_| {});
    }

    pub(super) fn key(
        self: &Rc<Self>,
        key: gdk::Key,
        modifiers: gdk::ModifierType,
    ) -> glib::Propagation {
        if !self.editor.is_editable() {
            return glib::Propagation::Proceed;
        }
        let modifiers = modifiers & gtk::accelerator_get_default_mod_mask();
        let control = gdk::ModifierType::CONTROL_MASK;
        // Cmd+arrows as on a Mac: to the line's ends, or the plan's; with
        // Shift, selecting.
        let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        if modifiers - gdk::ModifierType::SHIFT_MASK == gdk::ModifierType::META_MASK {
            let (step, count) = match key {
                gdk::Key::Left => (gtk::MovementStep::DisplayLineEnds, -1),
                gdk::Key::Right => (gtk::MovementStep::DisplayLineEnds, 1),
                gdk::Key::Up => (gtk::MovementStep::BufferEnds, -1),
                gdk::Key::Down => (gtk::MovementStep::BufferEnds, 1),
                _ => return glib::Propagation::Proceed,
            };
            self.editor.emit_move_cursor(step, count, shift);
            return glib::Propagation::Stop;
        }
        let handled = match (key.to_lower(), modifiers) {
            (gdk::Key::b, m) if m == control => self.wrap("**", "**"),
            (gdk::Key::i, m) if m == control => self.wrap("*", "*"),
            (gdk::Key::k, m) if m == control => self.link(),
            (gdk::Key::s, m) if m == control => {
                self.edits.set(self.edits.get() + 1);
                self.write(|_| {});
                true
            }
            (gdk::Key::Return | gdk::Key::KP_Enter, m) if m.is_empty() => self.enter(),
            (gdk::Key::Tab, m) if m.is_empty() => self.indent(true),
            (gdk::Key::ISO_Left_Tab | gdk::Key::Tab, m) if m == gdk::ModifierType::SHIFT_MASK => {
                self.indent(false)
            }
            _ => false,
        };
        if handled {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    fn line_start(&self) -> gtk::TextIter {
        let buffer = self.buffer();
        let mut start = buffer.iter_at_mark(&buffer.get_insert());
        start.set_line_offset(0);
        start
    }

    /// Continues a list (or quote) on Enter, or ends it on an empty item.
    fn enter(&self) -> bool {
        let buffer = self.buffer();
        if buffer.has_selection() {
            return false;
        }
        let start = self.line_start();
        let mut cursor = buffer.iter_at_mark(&buffer.get_insert());
        let before = buffer.text(&start, &cursor, false);
        let Some(enter) = crate::markdown::on_enter(&before) else {
            return false;
        };
        buffer.begin_user_action();
        match enter {
            crate::markdown::Enter::Continue(prefix) => {
                buffer.insert(&mut cursor, &format!("\n{prefix}"));
            }
            crate::markdown::Enter::End => {
                let mut start = start;
                buffer.delete(&mut start, &mut cursor);
            }
        }
        buffer.end_user_action();
        self.editor.scroll_mark_onscreen(&buffer.get_insert());
        true
    }

    /// Indents a list item by two spaces, or outdents it.
    fn indent(&self, deeper: bool) -> bool {
        let buffer = self.buffer();
        let mut start = self.line_start();
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let line = buffer.text(&start, &end, false);
        if crate::markdown::list_item(&line).is_none() {
            return false;
        }
        buffer.begin_user_action();
        if deeper {
            buffer.insert(&mut start, "  ");
        } else {
            let spaces = line.len() - line.trim_start_matches(' ').len();
            let mut until = start;
            until.forward_chars(spaces.min(2) as i32);
            buffer.delete(&mut start, &mut until);
        }
        buffer.end_user_action();
        true
    }

    /// Puts `before` and `after` around the selection, or around the cursor.
    fn wrap(&self, before: &str, after: &str) -> bool {
        let buffer = self.buffer();
        let (from, to) = match buffer.selection_bounds() {
            Some((from, to)) => (from.offset(), to.offset()),
            None => {
                let at = buffer.iter_at_mark(&buffer.get_insert()).offset();
                (at, at)
            }
        };
        let shift = before.chars().count() as i32;
        buffer.begin_user_action();
        buffer.insert(&mut buffer.iter_at_offset(to), after);
        buffer.insert(&mut buffer.iter_at_offset(from), before);
        buffer.end_user_action();
        buffer.select_range(
            &buffer.iter_at_offset(from + shift),
            &buffer.iter_at_offset(to + shift),
        );
        true
    }

    /// Makes the selection a link and puts the cursor where the address goes.
    fn link(&self) -> bool {
        let buffer = self.buffer();
        let selected = buffer.selection_bounds().is_some();
        self.wrap("[", "]()");
        let (_, end) = buffer.selection_bounds().unwrap_or_else(|| {
            let at = buffer.iter_at_mark(&buffer.get_insert());
            (at, at)
        });
        if selected {
            // Inside the parentheses.
            let mut at = end;
            at.forward_chars(2);
            buffer.place_cursor(&at);
        }
        true
    }

    /// Ticks or unticks the task checkbox under a click.
    fn toggle_checkbox_at(&self, x: f64, y: f64) -> bool {
        if !self.editor.is_editable() {
            return false;
        }
        let (bx, by) =
            self.editor
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let Some(at) = self.editor.iter_at_location(bx, by) else {
            return false;
        };
        let offset = at.offset();
        let Some(range) = self
            .checkboxes
            .borrow()
            .iter()
            .find(|range| range.contains(&offset))
            .cloned()
        else {
            return false;
        };
        let buffer = self.buffer();
        let mut from = buffer.iter_at_offset(range.start + 1);
        let mut to = buffer.iter_at_offset(range.start + 2);
        let mark = if buffer.text(&from, &to, false) == " " {
            "x"
        } else {
            " "
        };
        buffer.begin_user_action();
        buffer.delete(&mut from, &mut to);
        buffer.insert(&mut buffer.iter_at_offset(range.start + 1), mark);
        buffer.end_user_action();
        true
    }

    /// Runs `then` once nothing is left unsaved, asking if saving fails.
    fn finish(self: &Rc<Self>, then: impl FnOnce() + 'static) {
        let weak = Rc::downgrade(self);
        self.edits.set(self.edits.get() + 1);
        self.write(move |saved| {
            let Some(plan) = weak.upgrade() else { return };
            if saved {
                return then();
            }
            let dialog = gtk::AlertDialog::builder()
                .message("The plan couldn't be saved")
                .detail("Close it anyway and lose your edits?")
                .buttons(["Keep editing", "Close without saving"])
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let window = plan.editor.root().and_downcast::<gtk::Window>();
            dialog.choose(window.as_ref(), gio::Cancellable::NONE, move |answer| {
                if answer == Ok(1) {
                    then();
                }
            });
        });
    }
}

/// Removes the plan panel, saving first.
fn close_panel(ws: &Rc<WorkspaceUi>) {
    let Some(plan) = ws.plan_panel.borrow().clone() else {
        return;
    };
    let weak = Rc::downgrade(ws);
    plan.clone().finish(move || {
        if let Some(ws) = weak.upgrade() {
            let split = &ws.plan_split;
            ws.plan_width.set(split.width() - split.position());
            split.set_end_child(gtk::Widget::NONE);
            ws.plan_panel.replace(None);
        }
    });
}

/// Opens the plan at its last width, once the workspace has one.
fn place_divider(ws: &Rc<WorkspaceUi>) {
    let width = ws.plan_split.width();
    if width > 0 {
        ws.plan_split
            .set_position((width - ws.plan_width.get()).max(0));
        return;
    }
    let weak = Rc::downgrade(ws);
    ws.plan_split.add_tick_callback(move |split, _| {
        let Some(ws) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if split.width() == 0 {
            return glib::ControlFlow::Continue;
        }
        place_divider(&ws);
        glib::ControlFlow::Break
    });
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markup_is_native_and_untrusted_content_is_escaped() {
        let rendered = markup(
            "# Plan\n\n**bold** and `code`\n\n<script>alert(1)</script>\n\n![alt](https://bad/image)\n\n[link](javascript:bad)",
        );
        assert!(rendered.contains("<big><b>Plan</b></big>"));
        assert!(rendered.contains("<b>bold</b>"));
        assert!(rendered.contains("&lt;script&gt;"));
        assert!(!rendered.contains("https://bad"));
        assert!(!rendered.contains("javascript:"));
        pango::parse_markup(&rendered, '\0').unwrap();
        pango::parse_markup(&markup("nul\0 and control\u{0001}"), '\0').unwrap();
        assert_eq!(markup(""), "");
    }
}
