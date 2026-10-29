//! Applying settings to the windows, and the Settings window.

use super::*;

mod form;
mod presets;
mod profiles;
mod shortcuts;

pub(super) use form::*;
pub(super) use presets::*;
pub(super) use profiles::*;
pub(super) use shortcuts::*;

pub(super) const MIN_ZOOM: f64 = 0.5;
pub(super) const MAX_ZOOM: f64 = 3.0;

/// Changes the text size of all terminals by `factor` (`None`: back to
/// normal), and keeps it for the next start.
pub(super) fn zoom(state: &App, factor: Option<f64>) {
    let shared = &state.shared;
    let zoom = factor.map_or(1.0, |f| (shared.zoom.get() * f).clamp(MIN_ZOOM, MAX_ZOOM));
    shared.zoom.set(zoom);
    shared.session.borrow_mut().zoom = (zoom != 1.0).then_some(zoom);
    shared.session_dirty.set(true);
    apply_settings(shared);
}

/// The detected terminal theme with the user's overrides applied.
pub(super) fn effective_theme(settings: &Settings) -> TerminalTheme {
    let mut theme = TerminalTheme::detect();
    if let Some(scheme) = settings.theme.as_deref().and_then(theme::named_scheme) {
        theme.set_colors_from(scheme);
    }
    if let Some(font) = &settings.font {
        theme.font = Some(pango::FontDescription::from_string(font));
    }
    theme
}

pub(super) fn opacity(shared: &Shared) -> f64 {
    shared
        .settings
        .borrow()
        .background_opacity
        .or(shared.theme.borrow().opacity)
        .unwrap_or(1.0)
        .clamp(MIN_OPACITY, 1.0)
}

/// Colors the whole interface from the terminal theme: the window
/// background (at the chosen opacity), and the sidebar, buttons and pane
/// headers in the theme's text and accent colors instead of the GTK
/// theme's greys. Terminals themselves have a transparent background.
pub(super) fn apply_colors(shared: &Shared) {
    let scale = shared.settings.borrow().ui_scale();
    let css = theme_css(&shared.theme.borrow(), opacity(shared), scale);
    shared.background_css.load_from_string(&css);
    for window in shared.windows() {
        for ws in window.workspaces.borrow().values() {
            if let Some(plan) = ws.plan_panel.borrow().as_ref() {
                plan.recolor(&shared.theme.borrow());
            }
        }
    }
}

pub(super) fn theme_css(theme: &TerminalTheme, opacity: f64, scale: f64) -> String {
    let rgba = |color: gdk::RGBA, alpha: f64| {
        let channel = |c: f32| (c * 255.0).round() as u8;
        format!(
            "rgba({}, {}, {}, {alpha:.3})",
            channel(color.red()),
            channel(color.green()),
            channel(color.blue())
        )
    };
    let bg = theme.background();
    let fg = theme.foreground();
    let ansi = theme.ansi_colors();
    // How far apart two colors are to the eye (WCAG contrast ratio).
    let contrast = |a: gdk::RGBA, b: gdk::RGBA| {
        let luminance = |c: gdk::RGBA| {
            let linear = |x: f32| {
                if x <= 0.03928 {
                    x / 12.92
                } else {
                    ((x + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.red()) + 0.7152 * linear(c.green()) + 0.0722 * linear(c.blue())
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    };
    // A scheme color that marks things, or its bright variant when it is
    // too close to the background (e.g. xterm's dark blue), else the text.
    let visible = |normal: usize| {
        [ansi[normal], ansi[normal + 8]]
            .into_iter()
            .find(|&color| contrast(color, bg) >= 3.0)
            .unwrap_or(fg)
    };
    // The scheme's blue: the accent most themes are designed around.
    let accent = visible(4);
    let red = visible(1);
    format!(
        "window.soi {{ background-color: {window}; color: {fg}; font-size: {percent}%; }}
.soi-sidebar, .soi-sidebar list, .soi-sidebar row {{ color: {fg}; }}
.soi-sidebar row:hover {{ background-color: {hover}; }}
.soi-sidebar row:selected, .soi-sidebar row:selected:hover {{ background-color: {selected}; color: {fg}; }}
/* One faded text color, also for GTK's dim-label (which fades with opacity). */
.dim-label {{ opacity: 1; color: {dim}; }}
.ws-worktree, .soi-windows-title, .agents-title, .soi-sidebar editablelabel.group-name {{ color: {dim}; }}
.soi-sidebar editablelabel.editing, .soi-sidebar editablelabel.editing:focus-within {{
  background: none; color: {fg}; opacity: 1; box-shadow: none; outline: none;
}}
.soi-sidebar editablelabel > stack > text {{
  background: none; background-image: none; border: none; box-shadow: none; outline: none;
  margin: 0; padding: 0; min-height: 0;
  color: {fg}; caret-color: {fg};
}}
.soi-sidebar editablelabel > stack > text selection {{ background-color: {selection}; color: {fg}; }}
.soi-sidebar .group-toggle {{ color: {dim}; }}
.soi-sidebar .group-toggle:hover {{ background-color: {hover}; color: {fg}; }}
.soi-sidebar .group-add {{ color: {dim}; }}
.soi-sidebar .group-add:hover {{ background-color: {hover}; color: {fg}; }}
.soi-windows button.destructive-action {{ background: {red_soft}; color: {fg}; box-shadow: none; }}
.soi-windows button.destructive-action:hover {{ background: {red_hover}; }}
.soi-sidebar .ws-close {{ color: {dim}; }}
.soi-sidebar .ws-close:hover {{ background-color: {hover}; color: {fg}; }}
.soi-toolbar button, .soi-toolbar dropdown > button, .soi-window-button > button {{
  background: none; background-image: none; border: none; box-shadow: none; outline: none; color: {fg};
}}
.soi-toolbar button:hover, .soi-toolbar dropdown > button:hover, .soi-window-button > button:hover,
.soi-window-button > button:checked {{ background-color: {hover}; }}
.pane-header {{ background-color: {field}; color: {dim}; }}
.pane-header button {{ color: {dim}; }}
.pane-header button:hover {{ background-color: {hover}; color: {fg}; }}
.pane.focused {{ border-color: {focus_border}; }}
.drop-hint {{ background-color: {drop}; border: 2px solid {accent}; border-radius: 6px; margin: 4px; }}
.pane.zoomed .zoom-button {{ color: {accent}; }}
.pane.focused .pane-header {{ background-color: {focused_header}; color: {fg}; }}
.broadcast .pane {{ border-color: {red}; }}
.tab-progress progress {{ background-color: {accent}; }}
.tab-progress.error progress {{ background-color: {red}; }}
.tab-progress.paused progress {{ background-color: {yellow}; }}
.tab-progress trough {{ background-color: {border}; }}
.broadcast .pane-header {{ color: {red}; }}
.pane-tab:hover {{ background-color: {hover}; }}
.pane-tab.active {{ background-color: {tab_active}; color: {fg}; box-shadow: inset 0 -2px 0 0 {accent}; }}
.pane-tabs.single .pane-tab {{ background: none; box-shadow: none; }}
row.drop-before {{ box-shadow: inset 0 2px 0 0 {accent}; }}
row.drop-after {{ box-shadow: inset 0 -2px 0 0 {accent}; }}
.pane-tab.drop-before {{ box-shadow: inset 2px 0 0 0 {accent}; }}
.pane-tabs.drop-end > button {{ box-shadow: inset 2px 0 0 0 {accent}; }}
row.drop-into {{ background-color: {drop}; }}

window.soi-settings, popover > contents, popover > arrow {{ background-color: {solid}; color: {fg}; }}
window.soi-settings button, window.soi-settings entry, window.soi-settings spinbutton,
window.soi-settings dropdown > button {{
  background-color: {field}; background-image: none; color: {fg};
  border-color: {border}; box-shadow: none;
}}
window.soi-settings spinbutton button {{ background-color: transparent; border: none; }}
window.soi-settings button:hover, window.soi-settings dropdown > button:hover {{ background-color: {hover}; }}
window.soi-settings entry:focus-within, window.soi-settings spinbutton:focus-within,
window.soi-settings button:focus-visible {{ outline-color: {accent}; border-color: {accent}; }}
window.soi-settings scale trough {{ background-color: {border}; }}
window.soi-settings scale highlight {{ background-color: {accent}; }}
window.soi-settings scale slider {{ background-color: {fg}; border-color: {border}; }}
window.soi-settings separator {{ background-color: {border}; }}
popover modelbutton:hover, popover row:hover {{ background-color: {hover}; }}
popover row:selected {{ background-color: {selected}; }}
window.soi-settings stackswitcher button:checked {{ background-color: {selected}; }}
window.soi-settings button.capturing {{ border-color: {accent}; color: {accent}; }}
window.soi-settings .soi-section {{ font-weight: bold; color: {accent}; }}
window.soi-settings .soi-note {{ color: {accent}; }}
window.soi-settings button.changed {{ color: {accent}; }}
button.soi-chip {{ padding: 2px 10px; border-radius: 999px; }}
window.soi-settings button.soi-chip:checked, window.soi-tools button.soi-chip:checked {{
  background-color: {accent}; color: {solid}; border-color: {accent};
}}
.agent-working {{ color: {yellow}; }}
.agent-input {{ color: {red}; }}
.agent-done {{ color: {green}; }}
.agent-idle {{ color: {dim}; }}
.agent-exited {{ color: {dim}; }}
.agents-panel row.unseen {{ background-color: {selected}; }}
.soi-toolbar button.agents-waiting {{ color: {red}; }}
.soi-window-button {{ margin: 0 8px 4px 8px; }}
.soi-window-button > button {{ padding: 4px 8px; color: {dim}; }}
.soi-window-button > button:hover, .soi-window-button > button:checked {{ color: {fg}; }}

window.soi-tools, window.message {{ background-color: {solid}; color: {fg}; }}
window.soi-tools button, window.soi-tools entry, window.soi-tools dropdown > button,
window.soi-tools textview, window.soi-tools textview text,
window.message button, .agents-panel dropdown > button {{
  background-color: {field}; background-image: none; color: {fg};
  border-color: {border}; box-shadow: none;
}}
window.soi-tools button:hover, window.soi-tools dropdown > button:hover, window.message button:hover,
.agents-panel dropdown > button:hover {{ background-color: {hover}; }}
window.soi-tools button:disabled, window.soi-tools dropdown > button:disabled {{ color: {dim}; opacity: 1; }}
window.soi-tools entry:focus-within, window.soi-tools button:focus-visible,
window.soi-tools textview:focus-within, window.soi-tools dropdown > button:focus-visible,
.agents-panel dropdown > button:focus-visible {{ outline-color: {accent}; border-color: {accent}; }}
window.soi-tools list, window.soi-tools row {{ background-color: transparent; color: {fg}; }}
window.soi-tools row:hover {{ background-color: {hover}; }}
window.soi-tools button:checked {{ background-color: {selected}; color: {fg}; }}
window.soi-tools check {{ background-color: {field}; border-color: {border}; color: {fg}; }}
window.soi-tools check:checked {{ background-color: {selected}; }}
window.soi-tools .soi-plan-conflict {{ background-color: {selected}; border-radius: 6px; padding: 8px; }}
window.soi-tools row:selected, window.soi-tools textview text selection {{ background-color: {selected}; color: {fg}; }}
.soi-plan textview, .soi-plan textview text {{ background-color: transparent; color: {fg}; caret-color: {fg}; }}
.soi-plan textview text selection {{ background-color: {fg}; color: {solid}; }}
.soi-plan .soi-plan-conflict {{ background-color: {selected}; border-radius: 6px; padding: 8px; }}
popover list, popover listview, popover row {{ background-color: transparent; color: {fg}; }}
popover row:selected, popover row:selected:hover {{ background-color: {selected}; color: {fg}; }}
popover check, popover radio {{ color: {fg}; }}
.agents-search {{ background-color: {field}; border: none; color: {fg}; }}
.agents-filters dropdown > button {{ background-color: {field}; border: none; color: {dim}; }}
.agents-filters dropdown > button:hover {{ background-color: {hover}; color: {fg}; }}
.agents-notices .agents-clear {{ color: {dim}; }}
.agents-notices .agents-clear:hover {{ background-color: {hover}; color: {fg}; }}
.agents-notices list row {{ background-color: {field}; }}
.agents-notices list row:hover {{ background-color: {hover}; }}
.agents-panel {{ border-left: 1px solid {border}; }}
.agents-notices {{ border-top: 1px solid {border}; }}
",
        window = rgba(bg, opacity),
        percent = (scale * 100.0).round(),
        solid = rgba(bg, 1.0),
        field = rgba(fg, 0.06),
        border = rgba(fg, 0.14),
        fg = rgba(fg, 1.0),
        dim = rgba(fg, 0.65),
        hover = rgba(fg, 0.08),
        selected = rgba(accent, 0.22),
        selection = rgba(accent, 0.45),
        focused_header = rgba(accent, 0.14),
        focus_border = rgba(accent, 0.35),
        drop = rgba(accent, 0.3),
        tab_active = rgba(fg, 0.12),
        accent = rgba(accent, 1.0),
        yellow = rgba(visible(3), 1.0),
        green = rgba(visible(2), 1.0),
        red_soft = rgba(red, 0.25),
        red_hover = rgba(red, 0.35),
        red = rgba(red, 1.0),
    )
}

/// Re-applies theme, font, scrollback and opacity to every open terminal.
pub(super) fn apply_settings(shared: &Shared) {
    let mut theme = effective_theme(&shared.settings.borrow());
    // The window scale from the settings, and zoom in/out on top of it.
    let zoom = shared.zoom.get() * shared.settings.borrow().ui_scale();
    if zoom != 1.0 {
        let mut font = theme.font.clone().unwrap_or_else(crate::term::default_font);
        let size = (font.size() as f64 * zoom).round() as i32;
        if font.is_size_absolute() {
            font.set_absolute_size(size as f64);
        } else {
            font.set_size(size);
        }
        theme.font = Some(font);
    }
    let scrollback = shared.settings.borrow().scrollback_lines() as usize;
    let blink = shared.settings.borrow().cursor_blink();
    for window in shared.windows() {
        for pane in window.panes.borrow().values() {
            for tab in pane.tabs.borrow().iter() {
                tab.terminal.set_theme(&theme);
                tab.terminal.set_scrollback(scrollback);
                tab.terminal.set_cursor_blink(blink);
            }
        }
    }
    *shared.theme.borrow_mut() = theme;
    apply_app_font(&shared.settings.borrow());
    apply_colors(shared);
}

/// The font of everything but the terminals, which draw their own: the
/// chosen one, or the system's when none is.
fn apply_app_font(settings: &Settings) {
    thread_local! {
        /// The font we set, if any: GTK can't tell ours from the system's.
        static SET: RefCell<Option<String>> = const { RefCell::new(None) };
    }
    let Some(gtk_settings) = gtk::Settings::default() else {
        return;
    };
    let font = settings.app_font.clone();
    // Only on a change: either one restyles every widget.
    if SET.with_borrow(|set| *set == font) {
        return;
    }
    match &font {
        Some(font) => gtk_settings.set_gtk_font_name(Some(font)),
        // Back to the system's font, which GTK reads again from now on.
        None => gtk_settings.reset_property("gtk-font-name"),
    }
    SET.set(font);
}

/// The font everything but the terminals uses now.
pub(super) fn app_font() -> pango::FontDescription {
    gtk::Settings::default()
        .and_then(|settings| settings.gtk_font_name())
        .map_or_else(pango::FontDescription::new, |name| {
            pango::FontDescription::from_string(&name)
        })
}

pub(super) fn update_settings(state: &App, change: impl FnOnce(&mut Settings)) {
    let shared = &state.shared;
    {
        let mut settings = shared.settings.borrow_mut();
        change(&mut settings);
        if let Err(err) = settings.save_profile(&shared.profile.borrow()) {
            crate::log::write(&format!("failed to save settings: {err}"));
        }
    }
    apply_settings(shared);
}

pub(super) fn open_settings(state: &App) {
    if let Some(window) = state.shared.settings_window.upgrade() {
        window.present();
        return;
    }
    let window = gtk::Window::builder()
        .title("Soi Settings")
        .css_classes(["soi-settings"])
        .transient_for(&state.window)
        .destroy_with_parent(true)
        .default_width(600)
        .default_height(760)
        .child(&settings_content(state))
        .build();
    let close = gtk::ShortcutController::new();
    close.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    window.add_controller(close);
    state.shared.settings_window.set(Some(&window));
    window.present();
}

/// The settings window's content: the General and Shortcuts pages.
pub(super) fn settings_content(state: &App) -> gtk::Widget {
    let previous = state
        .shared
        .settings_stack
        .upgrade()
        .and_then(|stack| stack.visible_child_name());
    let stack = gtk::Stack::new();
    let scrolled = |child: &gtk::Widget| {
        gtk::ScrolledWindow::builder()
            .child(child)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build()
    };
    stack.add_titled(&scrolled(&settings_form(state)), Some("general"), "General");
    stack.add_titled(
        &scrolled(&shortcuts_page(state)),
        Some("shortcuts"),
        "Shortcuts",
    );
    stack.add_titled(
        &scrolled(&panel::settings_page(state)),
        Some("agents"),
        "Agents",
    );
    stack.add_titled(
        &scrolled(&super::requests::collections_page()),
        Some("collections"),
        "Collections",
    );
    if let Some(name) = previous {
        stack.set_visible_child_name(&name);
    }
    let switcher = gtk::StackSwitcher::builder()
        .stack(&stack)
        .halign(gtk::Align::Center)
        .margin_top(12)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&switcher);
    content.append(&stack);
    state.shared.settings_stack.set(Some(&stack));
    content.upcast()
}
