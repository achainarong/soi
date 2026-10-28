//! The GTK user interface: a sidebar of (optionally grouped) workspaces and,
//! per workspace, main tabs with independent trees of terminal split panes.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, gio, glib, graphene, pango};
use uuid::Uuid;

use crate::agents::{Agent, SessionMetadata};
use crate::keybindings::{self, BINDINGS, Keybindings};
use crate::platform::Snap;
use crate::session::{
    self, Closed, Entry, Group, Layout, Session, WindowSession, Workspace, home_dir,
};
use crate::settings::{self, Settings};
use crate::term::{Input, Progress, TermView};
use crate::theme::{self, TerminalTheme};

mod actions;
mod changes;
mod layout_drag;
mod panel;
mod panes;
mod persist;
mod preview;
mod requests;
mod settings_window;
mod sidebar;
mod switcher;
mod task_view;
mod tasks;
mod window;
mod workspace_tabs;
mod workspaces;

#[cfg(feature = "gui-tests")]
#[expect(clippy::unwrap_used, reason = "a failed check should panic")]
mod css_tests;
#[cfg(feature = "gui-tests")]
pub(crate) mod gui_tests;
#[cfg(feature = "gui-tests")]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed check should panic"
)]
mod window_tests;

use actions::*;
use panes::*;
use persist::*;
use settings_window::*;
use sidebar::*;
use window::*;
use workspace_tabs::*;
use workspaces::*;

const APP_ID: &str = "dev.soi.Soi";
const DEV_APP_ID: &str = "dev.soi.Soi.Dev";
// Also catches shell cwd changes and divider drags without writing unchanged
// snapshots to disk. New windows and closing windows are saved immediately.
const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// How often terminal output is saved, in case soi doesn't quit normally.
const OUTPUT_SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(120);
/// How long after start the terminals not shown start (see
/// `start_unshown_terminals`): the shown ones get their size first.
const UNSHOWN_START_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
/// How much of each terminal's output comes back after a restart.
const SAVED_LINES: usize = 2000;
const MIN_OPACITY: f64 = 0.2;
/// How many closed tabs and workspaces can be reopened.
const CLOSED: usize = 20;

/// Layout only; colors come from the terminal theme (see `apply_colors`).
const CSS: &str = "
.pane { border: 1px solid transparent; }
/* One scale: text normal or 0.8em; spacing 2, 4, 8, 12px (24 above an empty list); corners 6px;
   small buttons a 16px icon with 4px around it. */
.caption { font-size: 0.8em; }
.group-name, .soi-windows-title, .agents-title { font-weight: bold; font-size: 0.8em; }
.pane-header { padding: 2px 4px; }
.pane-header button, .group-toggle, .group-add, .ws-close, .soi-windows-action {
  min-width: 16px; min-height: 16px; padding: 4px; border-radius: 6px;
}
.group-toggle, .group-add, .ws-close { border: none; background: none; box-shadow: none; }
.pane-tab { padding: 0 0 0 8px; margin: 2px 0; border-radius: 6px; }
.ws-name { font-weight: bold; }
.ws-worktree { font-weight: bold; }
.ws-close { opacity: 0; }
row:hover .ws-close, row:selected .ws-close, .ws-close:focus-visible { opacity: 1; }
.soi-sidebar, .soi-sidebar list { background-color: transparent; }
.soi-toolbar { padding: 4px 8px; }
.soi-windows-title { padding: 4px 8px 0; }
.agent-dot { font-size: 0.8em; }
.tab-progress { min-width: 36px; }
.tab-progress trough, .tab-progress progress { min-height: 3px; }
.agents-title { padding-left: 4px; }
.agents-count { font-weight: bold; font-size: 0.8em; }
.agents-notice { padding: 4px; }
.agents-notices list row { margin: 2px 6px; border-radius: 6px; }
.agents-filters { margin: 0 8px 4px; }
.agents-filters dropdown > button { padding: 2px 8px; min-height: 0; font-size: 0.8em; font-weight: normal; }
.agents-clear { font-size: 0.8em; padding: 2px 8px; min-height: 0; }
";

// Linux signal numbers, used to save the session before quitting.
const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

/// A terminal session in a pane, with stable identity for agent reports and output.
struct Tab {
    /// Tells agents' reports apart (`SOI_TAB_ID`).
    id: Uuid,
    terminal: TermView,
    /// The coding agent running here, if any.
    agent: RefCell<Agent>,
    /// The session of the agent running here, as it reported it, to resume
    /// it after a restart.
    resume: RefCell<Option<SessionMetadata>>,
    /// Marks the tab when its agent needs you.
    dot: gtk::Label,
    /// Terminal title and status in the pane header.
    button: gtk::Box,
    title: gtk::Label,
    /// What the program reports of its progress (OSC 9;4), when it does
    /// (shown in `button`).
    reported_progress: Cell<Option<Progress>>,
    close: gtk::Button,
    /// Directory the shell was started in.
    cwd: PathBuf,
    /// `TermView::output_serial` when the output was last saved.
    saved_output: Cell<u64>,
    /// Restored, and nothing happened since but the shell starting: its saved
    /// output stays as it is (else each restart adds a greeting to it).
    untouched: Cell<bool>,
}

/// Live state of one terminal pane. The pane widget is a `gtk::Box`
/// (header + terminal) and is the key in `State::panes`.
struct Pane {
    tabs: RefCell<Vec<Rc<Tab>>>,
    active: Cell<usize>,
    /// Shows the active tab's terminal.
    stack: gtk::Stack,
    /// The tabs in the header.
    strip: gtk::Box,
    drop_hint: gtk::Box,
}

impl Pane {
    fn current(&self) -> Option<Rc<Tab>> {
        self.tabs.borrow().get(self.active.get()).cloned()
    }

    fn terminal(&self) -> Option<TermView> {
        self.current().map(|tab| tab.terminal.clone())
    }
}

/// What all windows share: settings, theme and profile.
struct Shared {
    /// Open windows, oldest first (closed ones drop out).
    windows: RefCell<Vec<Weak<State>>>,
    /// All contexts, including closed windows that can be reopened.
    session: RefCell<Session>,
    /// Retry failed saves, and persist legacy sessions with new identities.
    session_dirty: Cell<bool>,
    settings: RefCell<Settings>,
    /// Detected theme with the user's settings applied on top.
    theme: RefCell<TerminalTheme>,
    /// Interface colors (from the terminal theme, at the chosen opacity).
    background_css: gtk::CssProvider,
    settings_window: glib::WeakRef<gtk::Window>,
    /// Name of the active settings profile.
    profile: RefCell<String>,
    keybindings: RefCell<Keybindings>,
    /// The settings window's pages, to stay on the same page when rebuilt.
    settings_stack: glib::WeakRef<gtk::Stack>,
    /// Shown once on the shortcuts page (e.g. that a key moved).
    shortcut_note: RefCell<Option<String>>,
    /// Text size factor (zoom in/out), for all terminals.
    zoom: Cell<f64>,
    /// Every tab by id, for agents' reports and the agents panel.
    tabs: RefCell<HashMap<Uuid, Weak<Tab>>>,
    /// What the agents panels show now (see `panel::refresh`).
    agents_shown: RefCell<String>,
    report_listener: RefCell<Option<panel::ReportListener>>,
    layout_drag: RefCell<layout_drag::LayoutDrag>,
}

impl Shared {
    /// The open windows, oldest first, without those being closed.
    fn windows(&self) -> Vec<App> {
        self.windows
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|window| !window.closing.get())
            .collect()
    }
}

struct WorkspaceUi {
    name: RefCell<String>,
    root_dir: PathBuf,
    task: RefCell<Option<crate::tasks::TaskContext>>,
    /// Stack page, holding `plan_split`.
    holder: gtk::Box,
    /// The terminals, and on the right the plan while it is shown, with a
    /// divider between them to drag.
    plan_split: gtk::Paned,
    tab_strip: gtk::Box,
    tab_stack: gtk::Stack,
    tabs: RefCell<Vec<Rc<MainTabUi>>>,
    active_tab: Cell<usize>,
    /// How wide the plan was last, for the next time it opens.
    plan_width: Cell<i32>,
    /// The plan beside the terminal, while it is shown (see `preview::toggle_for`).
    plan_panel: RefCell<Option<Rc<preview::Plan>>>,
}

struct MainTabUi {
    id: Uuid,
    name: RefCell<String>,
    button: gtk::Box,
    label: gtk::Label,
    /// Its agents' state and its programs' progress, for when a pane alone
    /// in the tab hides its header (see `update_header`).
    dot: gtk::Label,
    progress: gtk::ProgressBar,
    layout: gtk::Box,
    last_focus: glib::WeakRef<gtk::Box>,
    zoomed: glib::WeakRef<gtk::Box>,
    broadcast: Cell<bool>,
}

impl WorkspaceUi {
    /// A workspace always has a tab: closing its last one adds a fresh one
    /// (see `remove_main_tab_ui`). The index is clamped anyway, so a stale
    /// `active_tab` shows the last tab rather than aborting the app.
    fn current_tab(&self) -> Rc<MainTabUi> {
        let tabs = self.tabs.borrow();
        let last = tabs.len().saturating_sub(1);
        tabs[self.active_tab.get().min(last)].clone()
    }
}

/// Sidebar structure; workspace details live in `State::workspaces`.
enum Node {
    Workspace(Uuid),
    Group {
        id: Uuid,
        name: String,
        collapsed: bool,
        /// See `crate::session::Group::collection`.
        collection: Option<String>,
        workspaces: Vec<Uuid>,
    },
}

struct State {
    /// The context shown, which can change (see `swap_context`).
    id: Cell<Uuid>,
    window: gtk::ApplicationWindow,
    sidebar: gtk::ScrolledWindow,
    list: gtk::ListBox,
    stack: gtk::Stack,
    tree: RefCell<Vec<Node>>,
    workspaces: RefCell<HashMap<Uuid, Rc<WorkspaceUi>>>,
    panes: RefCell<HashMap<gtk::Box, Rc<Pane>>>,
    /// Divider ratio for each split, kept until the split has a size to apply it to.
    ratios: RefCell<HashMap<gtk::Paned, Rc<Cell<Option<f64>>>>>,
    active: Cell<Option<Uuid>>,
    focused: glib::WeakRef<gtk::Box>,
    rebuilding: Cell<bool>,
    closing: Cell<bool>,
    /// The user agreed to close the window although programs still run.
    close_confirmed: Cell<bool>,
    /// When the window was last typed in (see `WindowSession::last_used`).
    last_used: Cell<u64>,
    shared: Rc<Shared>,
    /// The window's name, shown in its title and sidebar.
    name: RefCell<String>,
    /// Shows the name; opens the window switcher.
    window_button: gtk::MenuButton,
    window_label: gtk::Label,
    /// The window switcher's contents (see `fill_switcher`).
    window_list: gtk::Box,
    /// The window's keyboard shortcuts, rebuilt when they're changed.
    shortcuts: RefCell<Option<gtk::ShortcutController>>,
    drop_hint: RefCell<Option<(gtk::ListBoxRow, &'static str)>>,
    /// The workspace last shown in each group (`None`: ungrouped ones), to
    /// come back to it when switching groups.
    last_in_group: RefCell<HashMap<Option<Uuid>, Uuid>>,
    /// Holds the profile switcher in the sidebar toolbar.
    profile_slot: gtk::Box,
    /// The agents panel, its toggle and notifications.
    agents: panel::AgentsUi,
}

type App = Rc<State>;

#[derive(Clone, Copy)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

pub fn run() -> glib::ExitCode {
    glib::set_application_name("Soi");
    // In dev mode a separate app, so it runs next to the installed soi
    // instead of opening a window in it.
    let id = if crate::dev_mode() {
        DEV_APP_ID
    } else {
        APP_ID
    };
    let app = gtk::Application::builder().application_id(id).build();
    let shared: Rc<OnceCell<Rc<Shared>>> = Rc::default();
    let shutdown = shared.clone();
    app.connect_activate(move |app| match shared.get() {
        // Started again while running (e.g. from the app menu).
        Some(shared) => reopen_last(shared),
        None => {
            let shared = shared.get_or_init(|| init_shared(app));
            panel::listen(shared);
            restore_windows(shared);
        }
    });
    let result = app.run();
    // app.quit() bypasses window close requests. Save handlers have already
    // run; release live PTYs and finish their reaping before Rust exits.
    if let Some(shared) = shutdown.get() {
        for window in shared.windows() {
            release_terminals(&window);
        }
    }
    crate::term::wait_for_shutdown();
    result
}

/// Brings back the saved context used last or, with none saved, the window
/// used last. A new, empty window only when there's no context at all.
fn reopen_last(shared: &Rc<Shared>) {
    let saved = shared
        .session
        .borrow()
        .last_closed()
        .map(|context| context.id);
    if let Some(id) = saved {
        open_window_context(shared, id);
    } else if let Some(window) = shared
        .windows()
        .into_iter()
        .max_by_key(|window| window.last_used.get())
    {
        window.window.present();
    } else {
        new_window(shared, None, true);
    }
}

fn restore_windows(shared: &Rc<Shared>) {
    let windows = shared.session.borrow().windows_to_open();
    if windows.is_empty() {
        new_window(shared, None, true);
    }
    for window in &windows {
        new_window(shared, Some(window), true);
    }
    // Once the shown terminals have their size, the others start too, so
    // agents in every tab come back without visiting it.
    let weak = Rc::downgrade(shared);
    glib::timeout_add_local_once(UNSHOWN_START_DELAY, move || {
        if let Some(shared) = weak.upgrade() {
            start_unshown_terminals(&shared);
        }
    });
}

/// Sets up what all windows share: styles, settings, saving.
fn init_shared(app: &gtk::Application) -> Rc<Shared> {
    // Matches the installed icon (`make install`), for windows and the
    // taskbar. Only possible once GTK is running, i.e. in here.
    gtk::Window::set_default_icon_name(APP_ID);
    #[expect(clippy::expect_used, reason = "GTK started, so it has a display")]
    let display = gdk::Display::default().expect("no display");
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let background_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &display,
        &background_css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let profile = settings::active_profile();
    let settings = Settings::load_profile(&profile);
    let theme = effective_theme(&settings);
    let session = Session::restored();
    let zoom = session
        .zoom
        .map_or(1.0, |zoom| zoom.clamp(MIN_ZOOM, MAX_ZOOM));
    let shared = Rc::new(Shared {
        windows: RefCell::default(),
        session: RefCell::new(session),
        session_dirty: Cell::new(true),
        settings: RefCell::new(settings),
        theme: RefCell::new(theme),
        background_css,
        settings_window: glib::WeakRef::new(),
        profile: RefCell::new(profile),
        keybindings: RefCell::new(Keybindings::load()),
        settings_stack: glib::WeakRef::new(),
        shortcut_note: RefCell::default(),
        zoom: Cell::new(zoom),
        tabs: RefCell::default(),
        agents_shown: RefCell::default(),
        report_listener: RefCell::default(),
        layout_drag: RefCell::default(),
    });
    // The terminals' font with the zoom and window scale, before any opens.
    apply_settings(&shared);
    panel::start(&shared);
    panel::install_app_action(app, &shared);

    let weak = Rc::downgrade(&shared);
    glib::timeout_add_local(SAVE_INTERVAL, move || match weak.upgrade() {
        Some(shared) => {
            save_all(&shared);
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
    let weak = Rc::downgrade(&shared);
    glib::timeout_add_local(OUTPUT_SAVE_INTERVAL, move || match weak.upgrade() {
        Some(shared) => {
            save_output(&shared, &shared.windows());
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
    for signal in [SIGHUP, SIGINT, SIGTERM] {
        let weak = Rc::downgrade(&shared);
        let app = app.clone();
        glib_unix::unix_signal_add_local(signal, move || {
            match weak.upgrade() {
                Some(shared) => save_and_quit(&shared),
                None => app.quit(),
            }
            glib::ControlFlow::Break
        });
    }
    // Also what macOS runs for Cmd+Q, the app menu and the Dock's Quit.
    let quit_action = gio::SimpleAction::new("quit", None);
    let weak = Rc::downgrade(&shared);
    quit_action.connect_activate(move |_, _| {
        if let Some(shared) = weak.upgrade() {
            quit(&shared);
        }
    });
    app.add_action(&quit_action);
    shared
}

/// Quits soi (the quit shortcut, or the Dock's Quit on a Mac). Unlike
/// closing the windows one by one, all of them come back next time. Asks
/// first when programs would be stopped.
fn quit(shared: &Rc<Shared>) {
    let windows = shared.windows();
    let Some(front) = windows
        .iter()
        .find(|state| state.window.is_active())
        .or(windows.first())
    else {
        save_and_quit(shared);
        return;
    };
    let mut running: Vec<String> = windows
        .iter()
        .flat_map(|state| running_in(state, &all_panes(state)))
        .collect();
    running.sort();
    running.dedup();
    let weak = Rc::downgrade(shared);
    confirm_close(front, "soi", running, move || {
        if let Some(shared) = weak.upgrade() {
            save_and_quit(&shared);
        }
    });
}

/// Saves every window's layout and output, then quits.
fn save_and_quit(shared: &Shared) {
    save_all(shared);
    save_output(shared, &shared.windows());
    for window in shared.windows() {
        window.closing.set(true);
    }
    if let Some(app) = gio::Application::default() {
        app.quit();
    }
}

/// `~/foo` for paths under the home directory.
fn tilde(path: &Path) -> String {
    match path.strip_prefix(home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Current branch (or short commit) of the git repo at `dir`, if any.
fn git_branch(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_file() {
        // Worktree or submodule: `.git` is a file pointing at the real dir.
        let content = fs::read_to_string(&dot_git).ok()?;
        dir.join(content.strip_prefix("gitdir:")?.trim())
    } else {
        dot_git
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => Some(branch.to_string()),
        None => Some(head.get(..7)?.to_string()),
    }
}
