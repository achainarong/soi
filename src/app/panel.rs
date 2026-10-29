//! The agents panel: every coding agent running in any window, with its
//! state, on the right side of each window. Agents that stop working while
//! you look elsewhere are marked (panel row, tab, sidebar, a counter and a
//! notification under the agents) until you look at them.

use gtk::cairo;
use std::time::Instant;

use super::*;
use crate::agents::{self, Agent, State as AgentState};

mod agent_settings;
mod notices;
mod reports;

use notices::*;

pub(super) use agent_settings::settings_page;
pub(super) use notices::install_app_action;
pub(super) use reports::{ReportListener, listen};

/// The share of the panel's height notifications take when they appear.
const NOTICES_SHARE: f64 = 0.25;
/// At most this many notifications are kept; the oldest go first.
const MAX_NOTICES: i32 = 30;
const TICK: std::time::Duration = std::time::Duration::from_secs(1);
/// "Working", "Working.", "Working..", "Working...": one step each.
const DOTS_STEP: std::time::Duration = std::time::Duration::from_millis(400);
const DOTS: [&str; 4] = ["", ".", "..", "..."];

/// A window's part of it.
pub(super) struct AgentsUi {
    /// The panel itself, beside the workspaces.
    pub(super) panel: gtk::Box,
    pub(super) list: gtk::ListBox,
    /// Narrows the list to agents whose task, workspace or message
    /// mentions every word typed.
    search: gtk::SearchEntry,
    activity_filter: gtk::DropDown,
    task_filter: gtk::DropDown,
    /// Shows or hides the panel; says how many agents need you.
    pub(super) toggle: gtk::Button,
    pub(super) count: gtk::Label,
    /// Agents above, notifications below.
    split: gtk::Paned,
    /// The notifications, newest first; each row is named after its tab.
    pub(super) notices: gtk::ListBox,
    notices_box: gtk::Box,
    /// The panel's rows, in the order shown. Updated in place, so they
    /// don't flicker or lose hover when an agent changes.
    rows: RefCell<Vec<AgentRow>>,
    /// From the divider to the window's edge, also while hidden; 0: the
    /// panel's natural width.
    width: Cell<i32>,
    /// `width` waits for the window's first width to be placed: until then
    /// the divider sits at the panel's natural width, which isn't to be saved.
    placing: Cell<bool>,
}

/// One agent's row in the panel.
struct AgentRow {
    id: Uuid,
    row: gtk::ListBoxRow,
    dot: gtk::Label,
    name: gtk::Label,
    time: gtk::Label,
    /// "claude · Working", then animated dots while it works, then the
    /// window when it is another one.
    status: gtk::Label,
    dots: gtk::Label,
    place: gtk::Label,
    message: gtk::Label,
    /// "worked 12m · waited 3m".
    totals: gtk::Label,
    /// "Opus 5.5 · 45k context", when the agent reports it.
    usage: gtk::Label,
    /// Answers an agent that asks, without going there.
    reply: gtk::Entry,
}

pub(super) fn build(shown: bool, width: i32) -> AgentsUi {
    let title = gtk::Label::builder()
        .label("Agents")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["agents-title"])
        .build();
    let close = toolbar_button("window-close-symbolic", "Hide the agents panel");
    close.set_action_name(Some("win.agents-toggle"));
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header.add_css_class("soi-toolbar");
    header.append(&title);
    header.append(&close);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("navigation-sidebar");
    list.set_placeholder(Some(
        &gtk::Label::builder()
            .label("No agents running.\nStart Claude Code, OpenCode or another agent in a tab.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .margin_top(24)
            .margin_start(12)
            .margin_end(12)
            .css_classes(["dim-label"])
            .build(),
    ));
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .css_classes(["soi-sidebar"])
        .build();
    let notices = gtk::ListBox::new();
    notices.set_selection_mode(gtk::SelectionMode::None);
    notices.add_css_class("navigation-sidebar");
    let clear = gtk::Button::with_label("Clear all");
    clear.add_css_class("flat");
    clear.add_css_class("agents-clear");
    clear.set_action_name(Some("win.agents-notices-clear"));
    let notices_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    notices_header.add_css_class("soi-toolbar");
    notices_header.append(
        &gtk::Label::builder()
            .label("Notifications")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["agents-title"])
            .build(),
    );
    notices_header.append(&clear);
    let notices_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    notices_box.add_css_class("agents-notices");
    notices_box.append(&notices_header);
    notices_box.append(
        &gtk::ScrolledWindow::builder()
            .child(&notices)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .css_classes(["soi-sidebar"])
            .build(),
    );
    notices_box.set_visible(false);
    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .start_child(&scrolled)
        .end_child(&notices_box)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .vexpand(true)
        .build();
    // Notifications take their share once the panel has a height.
    split.connect_map(size_notices);
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("agents-panel");
    panel.set_width_request(260);
    panel.append(&header);
    let activity_filter = gtk::DropDown::from_strings(&[
        "All activity",
        "Working",
        "Needs input",
        "Done",
        "Idle",
        "Exited",
    ]);
    let task_filter = gtk::DropDown::from_strings(&[
        "All tasks",
        "Active",
        "Blocked",
        "Ready for review",
        "Finished",
    ]);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search tasks, tickets, agents…")
        .css_classes(["agents-search"])
        .build();
    panel.append(&search);
    // Side by side: they narrow the list, not head it.
    let filters = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    filters.add_css_class("agents-filters");
    filters.set_homogeneous(true);
    filters.append(&activity_filter);
    filters.append(&task_filter);
    panel.append(&filters);
    panel.append(&split);
    // Which soi this is, faint in the bottom right corner.
    panel.append(
        &gtk::Label::builder()
            .label(concat!("Soi ", env!("CARGO_PKG_VERSION")))
            .xalign(1.0)
            .margin_end(8)
            .margin_bottom(8)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    panel.set_visible(shown);

    let count = gtk::Label::builder()
        .css_classes(["agents-count"])
        .visible(false)
        .build();
    let toggle_content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    toggle_content.append(&gtk::Label::new(Some("◨")));
    toggle_content.append(&count);
    let toggle = gtk::Button::builder()
        .child(&toggle_content)
        .action_name("win.agents-toggle")
        .build();

    AgentsUi {
        panel,
        list,
        search,
        activity_filter,
        task_filter,
        toggle,
        count,
        split,
        notices,
        notices_box,
        rows: RefCell::default(),
        width: Cell::new(width),
        placing: Cell::new(false),
    }
}

/// Wires a new window's panel, banner and focus tracking.
pub(super) fn connect(state: &App) {
    for filter in [&state.agents.activity_filter, &state.agents.task_filter] {
        let shared = Rc::downgrade(&state.shared);
        filter.connect_selected_notify(move |_| {
            if let Some(shared) = shared.upgrade() {
                refresh(&shared);
            }
        });
    }
    let shared = Rc::downgrade(&state.shared);
    state.agents.search.connect_search_changed(move |_| {
        if let Some(shared) = shared.upgrade() {
            refresh(&shared);
        }
    });
    state
        .agents
        .search
        .connect_stop_search(|search| search.set_text(""));
    let add = |name: &str, f: fn(&App)| {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(state);
        action.connect_activate(move |_, _| {
            if let Some(state) = weak.upgrade() {
                f(&state);
            }
        });
        state.window.add_action(&action);
    };
    add("agents-toggle", toggle);
    add("agents-notices-clear", |state| {
        for window in state.shared.windows() {
            window.agents.notices.remove_all();
            window.agents.notices_box.set_visible(false);
        }
    });

    let shared = Rc::downgrade(&state.shared);
    state.agents.notices.connect_row_activated(move |_, row| {
        if let (Some(shared), Ok(id)) = (shared.upgrade(), Uuid::parse_str(&row.widget_name())) {
            remove_notice(&shared, id);
            focus_tab(&shared, id);
        }
    });
    let shared = Rc::downgrade(&state.shared);
    state.agents.list.connect_row_activated(move |_, row| {
        if let (Some(shared), Ok(id)) = (shared.upgrade(), Uuid::parse_str(&row.widget_name())) {
            focus_tab(&shared, id);
        }
    });
    // Coming back to the window counts as looking at what it shows.
    let shared = Rc::downgrade(&state.shared);
    state.window.connect_is_active_notify(move |window| {
        if window.is_active()
            && let Some(shared) = shared.upgrade()
        {
            schedule_seen(&shared);
        }
    });
    update_toggle(state);
}

pub(super) fn toggle(state: &App) {
    show(state, !state.agents.panel.get_visible());
}

pub(super) fn show(state: &App, shown: bool) {
    if !shown {
        remember_width(state);
    }
    state.agents.panel.set_visible(shown);
    if shown {
        place(state);
    }
    update_toggle(state);
}

/// Shows or hides the panel as saved, with its saved width (another
/// context's: the current width isn't kept).
pub(super) fn show_saved(state: &App, shown: bool, width: i32) {
    state.agents.panel.set_visible(false);
    state.agents.width.set(width);
    show(state, shown);
}

/// The panel's width to save: as shown, or as it was last shown.
pub(super) fn width(state: &App) -> i32 {
    remember_width(state);
    state.agents.width.get()
}

fn remember_width(state: &App) {
    let Some(content) = state.agents.panel.parent().and_downcast::<gtk::Paned>() else {
        return;
    };
    if state.agents.panel.get_visible() && content.width() > 0 && !state.agents.placing.get() {
        state.agents.width.set(content.width() - content.position());
    }
}

/// Puts the divider where the panel gets its remembered width, once the
/// window has a width.
pub(super) fn place(state: &App) {
    state.agents.placing.set(false);
    let width = state.agents.width.get();
    let Some(content) = state.agents.panel.parent().and_downcast::<gtk::Paned>() else {
        return;
    };
    if width <= 0 || !state.agents.panel.get_visible() {
        return;
    }
    if content.width() > 0 {
        content.set_position((content.width() - width).max(0));
        return;
    }
    state.agents.placing.set(true);
    let weak = Rc::downgrade(state);
    content.add_tick_callback(move |content, _| {
        let Some(state) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if content.width() == 0 {
            return glib::ControlFlow::Continue;
        }
        place(&state);
        glib::ControlFlow::Break
    });
}

fn update_toggle(state: &App) {
    let hint = |label| shortcut_hint(&state.shared, label, "toggle-agents");
    state
        .agents
        .toggle
        .set_tooltip_text(Some(&if state.agents.panel.get_visible() {
            hint("Hide the agents panel")
        } else {
            hint("Show the agents panel")
        }));
}

// ---------------------------------------------------------------------------
// Tracking

/// Environment for a new tab's shell, so agents in it can report to soi.
pub(super) fn tab_env(id: Uuid) -> Vec<(String, String)> {
    let mut env = vec![("SOI_TAB_ID".to_string(), id.to_string())];
    if let Some(socket) = agents::socket_path() {
        env.push(("SOI_SOCKET".into(), socket.to_string_lossy().into_owned()));
    }
    if let Ok(exe) = std::env::current_exe() {
        env.push(("SOI_BIN".into(), installed(&exe.to_string_lossy())));
    }
    env
}

/// `exe`, or where it was reinstalled: Linux names a replaced binary
/// `… (deleted)`, which agents' hooks then can't run.
fn installed(exe: &str) -> String {
    match exe.strip_suffix(" (deleted)") {
        Some(path) if std::path::Path::new(path).is_file() => path.to_string(),
        _ => exe.to_string(),
    }
}

/// Tracks a new tab's agent.
pub(super) fn register(shared: &Rc<Shared>, tab: &Rc<Tab>) {
    shared.tabs.borrow_mut().insert(tab.id, Rc::downgrade(tab));
    let (weak_shared, weak_tab) = (Rc::downgrade(shared), Rc::downgrade(tab));
    tab.terminal.connect_attention(move |message| {
        if let (Some(shared), Some(tab)) = (weak_shared.upgrade(), weak_tab.upgrade()) {
            let changed =
                tab.agent
                    .borrow_mut()
                    .attention(message, is_visible(&tab), Instant::now());
            if changed {
                changed_agent(&shared, &tab);
            }
        }
    });
    let (weak_shared, weak_tab) = (Rc::downgrade(shared), Rc::downgrade(tab));
    tab.terminal.connect_command_finished(move |finished| {
        if let (Some(shared), Some(tab)) = (weak_shared.upgrade(), weak_tab.upgrade()) {
            command_finished(&shared, &tab, finished);
        }
    });
}

/// Announces a long command that ended while you looked elsewhere, like
/// an agent that's done (agents are announced as such).
fn command_finished(shared: &Rc<Shared>, tab: &Rc<Tab>, finished: &crate::term::Finished) {
    // The shell is back, so no agent runs in it any more (also one that
    // quit before soi saw it run).
    tab.resume.take();
    let long = shared.settings.borrow().long_command();
    if long.is_none_or(|long| finished.took < long)
        || is_visible(tab)
        || tab.agent.borrow().program.is_some()
    {
        return;
    }
    let workspace = workspace_name(shared, tab).unwrap_or_default();
    let command = finished.command.as_deref().unwrap_or("A command");
    let how = match finished.exit {
        Some(0) => "finished".to_string(),
        Some(exit) => format!("failed (exit {exit})"),
        None => "ended".to_string(),
    };
    let after = format!("after {}", elapsed(finished.took));
    // Green when it worked, red (as for an agent that asks) when not.
    let state = if finished.exit == Some(0) {
        AgentState::Done
    } else {
        AgentState::Input
    };
    let title = format!("{command} {how}");
    let body = if workspace.is_empty() {
        after.clone()
    } else {
        format!("{workspace} · {after}")
    };
    // Nothing waits for you after a failure: a cross, not the dots.
    let picture = if finished.exit == Some(0) {
        Picture::State(state)
    } else {
        Picture::Failed
    };
    notify_desktop(shared, tab.id, &title, &body, picture);
    let notice = Notice {
        heading: workspace,
        what: format!("{command} {how} {after}"),
        state: Some(state),
        message: String::new(),
        details: Vec::new(),
    };
    add_notice(shared, tab.id, &notice);
}

/// Starts watching the terminals of all windows.
pub(super) fn start(shared: &Rc<Shared>) {
    let weak = Rc::downgrade(shared);
    let checking = Rc::new(Cell::new(false));
    glib::timeout_add_local(TICK, move || match weak.upgrade() {
        Some(shared) => {
            check(&shared, &checking);
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
    let weak = Rc::downgrade(shared);
    glib::timeout_add_local(DOTS_STEP, move || match weak.upgrade() {
        Some(shared) => {
            animate_dots(&shared);
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
}

/// The live tabs, dropping those that are gone.
fn live_tabs(shared: &Shared) -> Vec<Rc<Tab>> {
    let mut tabs = shared.tabs.borrow_mut();
    tabs.retain(|_, tab| tab.strong_count() > 0);
    tabs.values().filter_map(Weak::upgrade).collect()
}

fn agent_names(shared: &Shared) -> Vec<String> {
    agents::agent_list(shared.settings.borrow().agents.as_deref())
}

/// Looks at every terminal: which agent runs there, and is it busy? Which
/// programs run is read from `/proc` on a worker thread, so however many
/// terminals there are, typing never waits for it; one check at a time.
fn check(shared: &Rc<Shared>, checking: &Rc<Cell<bool>>) {
    if checking.replace(true) {
        return;
    }
    let tabs: Vec<(Weak<Tab>, Option<u32>)> = live_tabs(shared)
        .iter()
        .map(|tab| (Rc::downgrade(tab), tab.terminal.pid()))
        .collect();
    let pids: Vec<Option<u32>> = tabs.iter().map(|(_, pid)| *pid).collect();
    let (weak, checking) = (Rc::downgrade(shared), checking.clone());
    super::tasks::background(
        move || {
            pids.into_iter()
                .map(|pid| pid.map(|pid| crate::platform::current().foreground_programs(pid)))
                .collect::<Vec<_>>()
        },
        move |found| {
            checking.set(false);
            let Some(shared) = weak.upgrade() else { return };
            let started = Instant::now();
            let programs = tabs
                .into_iter()
                .zip(found)
                .filter_map(|((tab, pid), found)| {
                    let tab = tab.upgrade()?;
                    // A terminal that started again meanwhile waits a check.
                    if tab.terminal.pid() != pid {
                        return None;
                    }
                    let programs = found
                        .map_or_else(Vec::new, |found| tab.terminal.running_programs_from(found));
                    Some((tab, programs))
                });
            tick(&shared, programs.collect());
            crate::log::if_slow("agent check", started);
        },
    );
}

/// Updates the agents from the programs their terminals run.
fn tick(shared: &Rc<Shared>, programs: Vec<(Rc<Tab>, Vec<String>)>) {
    let names = agent_names(shared);
    let now = Instant::now();
    for (tab, programs) in programs {
        observe(&tab, &programs, &names, now);
        if is_visible(&tab) && tab.agent.borrow_mut().seen() {
            withdraw_notification(tab.id);
        }
    }
    // Also catches tabs that closed or moved, and renamed workspaces.
    if signature(shared) != *shared.agents_shown.borrow() {
        refresh(shared);
    } else {
        update_times(shared);
    }
}

/// Everything the panels show, to redraw them only when it changes.
fn signature(shared: &Shared) -> String {
    let mut text = String::new();
    for (tab, agent) in sorted_agents(shared) {
        let place = locate(shared, &tab).map(|(state, workspace, _)| {
            let name = state
                .workspaces
                .borrow()
                .get(&workspace)
                .map(|ws| ws.name.borrow().clone());
            (state.id.get(), workspace, name, state.name.borrow().clone())
        });
        text.push_str(&format!(
            "{} {:?} {:?} {:?} {} {} {:?} {:?}\n",
            tab.id,
            place,
            agent.program,
            agent.state,
            agent.unseen,
            agent.message,
            tab.title.text(),
            tasks::task_for_tab(shared, &tab)
        ));
    }
    text
}

fn observe(tab: &Tab, programs: &[String], names: &[String], now: Instant) -> bool {
    // A wrapper (e.g. mate, under node) may lead; the agent runs below it.
    let agent = programs
        .iter()
        .find(|program| agents::is_known(program, names));
    let known = agent.is_some();
    let program = agent.or(programs.first()).cloned();
    let busy = tab
        .terminal
        .last_activity()
        .is_some_and(|at| now.duration_since(at) < agents::BUSY_WINDOW);
    let visible = is_visible(tab);
    let mut agent = tab.agent.borrow_mut();
    let running = agent.is_agent() && agent.state != AgentState::Exited;
    let changed = agent.observe(program.as_deref(), known, busy, visible, now);
    if running && (!agent.is_agent() || agent.state == AgentState::Exited) {
        // It quit: the tab comes back as a shell.
        tab.resume.take();
    } else if agent.state != AgentState::Exited
        && let Some(program) = &agent.program
        && tab.resume.borrow().is_none()
    {
        // Until it says which session it is (or if it never does), its
        // latest one in the folder.
        tab.resume.replace(SessionMetadata::latest(program));
    }
    changed
}

/// Shown in the window you're using.
fn is_visible(tab: &Tab) -> bool {
    let widget = tab.terminal.widget();
    widget.is_mapped()
        && widget
            .root()
            .and_downcast::<gtk::Window>()
            .is_some_and(|window| window.is_active())
}

/// Clears the marks of what's visible now, once GTK has shown it.
pub(super) fn schedule_seen(shared: &Rc<Shared>) {
    let weak = Rc::downgrade(shared);
    glib::idle_add_local_once(move || {
        let Some(shared) = weak.upgrade() else { return };
        let mut changed = false;
        for tab in live_tabs(&shared) {
            if !is_visible(&tab) {
                continue;
            }
            remove_notice(&shared, tab.id);
            if tab.agent.borrow_mut().seen() {
                withdraw_notification(tab.id);
                changed = true;
            }
        }
        if changed {
            refresh(&shared);
        }
    });
}

/// Something about one agent changed: update the UI, and announce it if it
/// now needs you.
fn changed_agent(shared: &Rc<Shared>, tab: &Rc<Tab>) {
    let agent = tab.agent.borrow().clone();
    refresh(shared);
    if agent.unseen {
        announce(shared, tab, &agent);
    }
}

// ---------------------------------------------------------------------------
// Showing it

/// Where a tab is: its window, workspace and pane.
fn locate(shared: &Shared, tab: &Tab) -> Option<(App, Uuid, gtk::Box)> {
    let widget = tab.terminal.widget();
    let state = window_of(shared, widget)?;
    let (workspace, _) = workspace_of(&state, widget)?;
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if let Ok(pane) = widget.clone().downcast::<gtk::Box>()
            && state.panes.borrow().contains_key(&pane)
        {
            return Some((state, workspace, pane));
        }
        parent = widget.parent();
    }
    None
}

/// The name of the workspace a tab is in.
pub(super) fn workspace_name(shared: &Shared, tab: &Tab) -> Option<String> {
    let (state, workspace, _) = locate(shared, tab)?;
    let name = state
        .workspaces
        .borrow()
        .get(&workspace)?
        .name
        .borrow()
        .clone();
    Some(name)
}

/// Shows a tab: raises its window, opens its workspace and focuses it.
pub(super) fn focus_tab(shared: &Rc<Shared>, id: Uuid) {
    let Some(tab) = shared.tabs.borrow().get(&id).and_then(Weak::upgrade) else {
        return;
    };
    let Some((state, workspace, pane)) = locate(shared, &tab) else {
        return;
    };
    select_workspace(&state, workspace);
    if let Some(index) = tab_index(&state, &pane, &tab) {
        activate_tab(&state, &pane, index, true);
    }
    set_focused(&state, &pane);
    // A click in this window already activated it. Avoid asking the native
    // backend to present/map it again while its workspace is being switched.
    if !state.window.is_active() {
        state.window.present();
    }
    withdraw_notification(id);
    if tab.agent.borrow_mut().seen() {
        refresh(shared);
    }
    schedule_seen(shared);
}

/// Goes to the next agent that needs you, or else the next agent.
pub(super) fn next_agent(state: &App) {
    let shared = &state.shared;
    let agents = sorted_agents(shared);
    let waiting: Vec<&(Rc<Tab>, Agent)> = agents
        .iter()
        .filter(|(_, agent)| agent.unseen || agent.state == AgentState::Input)
        .collect();
    let candidates: Vec<&(Rc<Tab>, Agent)> = if waiting.is_empty() {
        agents.iter().collect()
    } else {
        waiting
    };
    let current = current_pane(state)
        .and_then(|pane| state.panes.borrow().get(&pane)?.current())
        .map(|tab| tab.id);
    let at = candidates
        .iter()
        .position(|(tab, _)| Some(tab.id) == current);
    let next = match at {
        Some(i) => candidates.get((i + 1) % candidates.len()),
        None => candidates.first(),
    };
    if let Some((tab, _)) = next {
        focus_tab(shared, tab.id);
    }
}

/// Every agent, in the order of the sidebars: by window, workspace, pane
/// and tab. A stable order, so rows don't jump when an agent's state
/// changes; what needs you is marked instead.
pub(super) fn sorted_agents(shared: &Shared) -> Vec<(Rc<Tab>, Agent)> {
    let mut agents = Vec::new();
    for state in shared.windows() {
        let panes = state.panes.borrow();
        for workspace in workspace_order(&state) {
            for pane in workspace_panes(&state, workspace) {
                let Some(pane) = panes.get(&pane) else {
                    continue;
                };
                for tab in pane.tabs.borrow().iter() {
                    let agent = tab.agent.borrow().clone();
                    if agent.is_agent() {
                        agents.push((tab.clone(), agent));
                    }
                }
            }
        }
    }
    agents
}

fn matches_filters(
    activity_filter: u32,
    task_filter: u32,
    state: AgentState,
    task: Option<crate::tasks::TaskStatus>,
) -> bool {
    let activity = match state {
        AgentState::Working => 1,
        AgentState::Input => 2,
        AgentState::Done => 3,
        AgentState::Idle => 4,
        AgentState::Exited => 5,
    };
    (activity_filter == 0 || activity_filter == activity)
        && (task_filter == 0
            || task.is_some_and(|status| {
                crate::tasks::TaskStatus::ALL.get(task_filter as usize - 1) == Some(&status)
            }))
}

/// Whether `fields` mention every word of `query`, ignoring case.
fn matches_search(query: &str, fields: &[&str]) -> bool {
    let text = fields.join("\n").to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| text.contains(word))
}

/// What the search looks through: the task and its ticket, where the agent
/// runs, and what it says.
fn search_fields(
    owner: &App,
    workspace: Uuid,
    tab: &Tab,
    agent: &Agent,
    task: Option<&crate::tasks::TaskContext>,
) -> Vec<String> {
    let mut fields = vec![
        owner.name.borrow().clone(),
        owner
            .workspaces
            .borrow()
            .get(&workspace)
            .map(|ws| ws.name.borrow().clone())
            .unwrap_or_default(),
        agent.program.clone().unwrap_or_default(),
        agent.state.label().to_string(),
        agent.message.clone(),
        tab.title.text().to_string(),
    ];
    if let Some(task) = task {
        fields.extend([
            task.title.clone(),
            task.status.label().to_string(),
            task.description.clone(),
            task.acceptance.clone(),
            task.branch.clone().unwrap_or_default(),
        ]);
        if let Some(issue) = &task.issue {
            fields.push(issue.clone());
            fields.push(notices::ticket(issue));
        }
        fields.extend(task.checklist.iter().map(|item| item.text.clone()));
    }
    // The checkout the tab is in, which also names a branch outside a task.
    if let Some((root, branch)) = crate::git::checkout(&panes::tab_cwd(tab)) {
        fields.extend(
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        );
        fields.extend(branch);
    }
    fields
}

/// Updates every window: panel rows, counter, tab and sidebar dots.
pub(super) fn refresh(shared: &Rc<Shared>) {
    shared.agents_shown.replace(signature(shared));
    let agents = sorted_agents(shared);
    let unseen = agents.iter().filter(|(_, agent)| agent.unseen).count();
    let windows = shared.windows();
    let now = Instant::now();
    for state in &windows {
        let ui = &state.agents;
        let mut old = ui.rows.take();
        let mut rows = Vec::new();
        let query = ui.search.text();
        for (tab, agent) in &agents {
            let task = tasks::task_for_tab(shared, tab);
            if !matches_filters(
                ui.activity_filter.selected(),
                ui.task_filter.selected(),
                agent.state,
                task.as_ref().map(|task| task.status),
            ) {
                continue;
            }
            let Some((owner, workspace, _)) = locate(shared, tab) else {
                continue;
            };
            if !query.trim().is_empty() {
                let fields = search_fields(&owner, workspace, tab, agent, task.as_ref());
                let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
                if !matches_search(&query, &fields) {
                    continue;
                }
            }
            let row = match old.iter().position(|row| row.id == tab.id) {
                Some(i) => old.remove(i),
                None => agent_row(shared, tab.id),
            };
            update_row(&row, state, &owner, workspace, tab, agent, now);
            rows.push(row);
        }
        for row in &old {
            ui.list.remove(&row.row);
        }
        // Only touch the list when rows came, went or moved.
        let shown: Vec<gtk::ListBoxRow> = (0..).map_while(|i| ui.list.row_at_index(i)).collect();
        if shown.len() != rows.len() || shown.iter().zip(&rows).any(|(a, b)| *a != b.row) {
            for row in &shown {
                ui.list.remove(row);
            }
            for row in &rows {
                ui.list.append(&row.row);
            }
        }
        ui.rows.replace(rows);
        ui.count.set_label(&unseen.to_string());
        ui.count.set_visible(unseen > 0);
        if unseen > 0 {
            ui.toggle.add_css_class("agents-waiting");
        } else {
            ui.toggle.remove_css_class("agents-waiting");
        }

        // Dots on tabs, and on workspaces with an agent that needs you.
        for pane in state.panes.borrow().values() {
            for tab in pane.tabs.borrow().iter() {
                let agent = tab.agent.borrow();
                set_dot(&tab.dot, agent.unseen.then_some(agent.state));
            }
        }
        let workspaces: Vec<Rc<WorkspaceUi>> =
            state.workspaces.borrow().values().cloned().collect();
        for ws in workspaces {
            for tab in ws.tabs.borrow().iter() {
                set_dot(&tab.dot, most_urgent(state, &tab_panes(tab)));
            }
        }
        let ids: Vec<Uuid> = state.workspaces.borrow().keys().copied().collect();
        for id in ids {
            if let Some(dot) = find_row(state, &id.to_string()).and_then(|row| row_dot(&row)) {
                set_dot(&dot, workspace_state(state, id));
            }
        }
    }
}

/// The most urgent state among a workspace's agents that need you.
fn workspace_state(state: &App, id: Uuid) -> Option<AgentState> {
    most_urgent(state, &workspace_panes(state, id))
}

/// The most urgent state among the agents in `panes` that need you.
fn most_urgent(state: &App, panes_in: &[gtk::Box]) -> Option<AgentState> {
    let panes = state.panes.borrow();
    panes_in
        .iter()
        .filter_map(|pane| panes.get(pane))
        .flat_map(|pane| pane.tabs.borrow().clone())
        .filter_map(|tab| {
            let agent = tab.agent.borrow();
            agent.unseen.then_some(agent.state)
        })
        .min_by_key(|state| state.rank())
}

/// The dot for a workspace's sidebar row.
pub(super) fn workspace_dot(state: &App, id: Uuid) -> gtk::Label {
    let dot = new_dot();
    set_dot(&dot, workspace_state(state, id));
    dot
}

fn update_times(shared: &Shared) {
    let now = Instant::now();
    let tabs = shared.tabs.borrow();
    for state in shared.windows() {
        for row in state.agents.rows.borrow().iter() {
            if let Some(tab) = tabs.get(&row.id).and_then(Weak::upgrade) {
                let agent = tab.agent.borrow();
                row.time
                    .set_label(&elapsed(now.duration_since(agent.since)));
                let totals = totals_text(&agent, now);
                row.totals.set_visible(!totals.is_empty());
                row.totals.set_label(&totals);
            }
        }
    }
}

/// Steps the dots after "Working" in every panel.
fn animate_dots(shared: &Shared) {
    // From the clock, so all working agents step together.
    let step = glib::monotonic_time() / DOTS_STEP.as_micros() as i64;
    let dots = DOTS[step as usize % DOTS.len()];
    for state in shared.windows() {
        for row in state.agents.rows.borrow().iter() {
            if row.dots.is_visible() {
                row.dots.set_label(dots);
            }
        }
    }
}

fn agent_row(shared: &Rc<Shared>, id: Uuid) -> AgentRow {
    let label = |classes: &[&str]| {
        gtk::Label::builder()
            .xalign(0.0)
            .css_classes(classes)
            .build()
    };
    let dot = label(&["agent-dot"]);
    dot.set_label("●");
    dot.set_valign(gtk::Align::Start);
    let name = label(&["ws-name"]);
    name.set_hexpand(true);
    name.set_ellipsize(pango::EllipsizeMode::End);
    let time = label(&["dim-label", "caption"]);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&name);
    top.append(&time);

    let status = label(&["caption"]);
    // Room for "..." from the start, so what follows doesn't shift.
    let dots = label(&["caption"]);
    dots.set_width_chars(3);
    let place = label(&["caption"]);
    place.set_hexpand(true);
    place.set_ellipsize(pango::EllipsizeMode::End);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.append(&status);
    line.append(&dots);
    line.append(&place);

    // Questions can be long: a few lines of them.
    let message = label(&["dim-label", "caption"]);
    message.set_wrap(true);
    message.set_wrap_mode(pango::WrapMode::WordChar);
    message.set_lines(3);
    message.set_ellipsize(pango::EllipsizeMode::End);
    let totals = label(&["dim-label", "caption"]);
    let usage = label(&["dim-label", "caption"]);
    usage.set_ellipsize(pango::EllipsizeMode::End);
    let reply = gtk::Entry::builder()
        .placeholder_text("Reply (Enter sends)")
        .css_classes(["agent-reply"])
        .visible(false)
        .build();
    let weak = Rc::downgrade(shared);
    reply.connect_activate(move |entry| {
        let Some(shared) = weak.upgrade() else { return };
        let tab = shared.tabs.borrow().get(&id).and_then(Weak::upgrade);
        if let Some(tab) = tab {
            send_reply(&tab, &entry.text());
            entry.set_text("");
        }
    });
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.append(&top);
    content.append(&line);
    content.append(&message);
    content.append(&totals);
    content.append(&usage);
    content.append(&reply);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    body.set_margin_top(4);
    body.set_margin_bottom(4);
    body.append(&dot);
    body.append(&content);
    let row = gtk::ListBoxRow::builder()
        .child(&body)
        .name(id.to_string())
        .css_classes(["agent-row"])
        .tooltip_text("Go to this agent")
        .build();
    AgentRow {
        id,
        row,
        dot,
        name,
        time,
        status,
        dots,
        place,
        message,
        totals,
        usage,
        reply,
    }
}

/// Types a reply into the agent's tab and presses Enter. An empty one is
/// just Enter (e.g. to accept what the agent suggests).
fn send_reply(tab: &Tab, text: &str) {
    if !text.is_empty() {
        tab.terminal.type_input(&Input::Text(text.to_string()));
    }
    tab.terminal.type_input(&Input::Key {
        key: gdk::Key::Return,
        state: gdk::ModifierType::empty(),
        base: None,
    });
}

/// "worked 12m · waited 3m", leaving out what hasn't happened.
fn totals_text(agent: &Agent, now: Instant) -> String {
    let (worked, waited) = agent.totals(now);
    let mut parts = Vec::new();
    if !worked.is_zero() {
        parts.push(format!("worked {}", elapsed(worked)));
    }
    if !waited.is_zero() {
        parts.push(format!("waited for you {}", elapsed(waited)));
    }
    parts.join(" · ")
}

fn update_row(
    row: &AgentRow,
    state: &App,
    owner: &App,
    workspace: Uuid,
    tab: &Tab,
    agent: &Agent,
    now: Instant,
) {
    for label in [&row.dot, &row.status, &row.dots, &row.place] {
        set_state_class(label, Some(agent.state));
    }
    let workspace_name = owner
        .workspaces
        .borrow()
        .get(&workspace)
        .map(|ws| ws.name.borrow().clone())
        .unwrap_or_default();
    let task = tasks::task_for_tab(&state.shared, tab);
    row.name.set_label(
        &task
            .as_ref()
            .map(|t| format!("{} · {}", t.title, t.status.label()))
            .unwrap_or(workspace_name),
    );
    row.time
        .set_label(&elapsed(now.duration_since(agent.since)));
    row.status.set_label(&format!(
        "{} · {}",
        agent.program.as_deref().unwrap_or_default(),
        agent.state.label()
    ));
    let working = agent.state == AgentState::Working;
    if working != row.dots.is_visible() {
        row.dots.set_label("");
        row.dots.set_visible(working);
    }
    if Rc::ptr_eq(state, owner) {
        row.place.set_label("");
    } else {
        row.place.set_label(&format!(" · {}", owner.name.borrow()));
    }
    // What the agent said, or else what it shows as its title.
    let text = if agent.message.is_empty() {
        tab.title.text().to_string()
    } else {
        agent.message.clone()
    };
    row.message.set_label(&text);
    row.message.set_tooltip_text(Some(&text));
    row.message.set_visible(!text.is_empty());
    let totals = totals_text(agent, now);
    row.totals.set_visible(!totals.is_empty());
    row.totals.set_label(&totals);
    let usage = agent.usage.as_ref().map(agents::Usage::label);
    row.usage.set_visible(usage.is_some());
    row.usage.set_label(usage.as_deref().unwrap_or_default());
    row.usage.set_tooltip_text(
        agent
            .usage
            .as_ref()
            .map(|u| match u.window() {
                Some(window) => {
                    format!("{} · {} of {window} tokens in context", u.model, u.context)
                }
                None => format!("{} · {} tokens in context", u.model, u.context),
            })
            .as_deref(),
    );
    row.reply.set_visible(agent.state == AgentState::Input);
    if agent.unseen {
        row.row.add_css_class("unseen");
    } else {
        row.row.remove_css_class("unseen");
    }
}

/// "12s", "4m", "2h".
fn elapsed(duration: std::time::Duration) -> String {
    let s = duration.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// A dot for tabs and sidebar rows, hidden until something needs you.
pub(super) fn new_dot() -> gtk::Label {
    gtk::Label::builder()
        .label("●")
        .visible(false)
        .css_classes(["agent-dot"])
        .build()
}

fn set_dot(dot: &gtk::Label, state: Option<AgentState>) {
    set_state_class(dot, state);
    if let Some(state) = state {
        dot.set_tooltip_text(Some(state.label()));
    }
    dot.set_visible(state.is_some());
}

/// Colors a label by an agent's state.
fn set_state_class(label: &gtk::Label, state: Option<AgentState>) {
    for class in [
        AgentState::Working,
        AgentState::Input,
        AgentState::Done,
        AgentState::Idle,
        AgentState::Exited,
    ]
    .map(AgentState::class)
    {
        if Some(class) != state.map(AgentState::class) {
            label.remove_css_class(class);
        }
    }
    if let Some(state) = state {
        label.add_css_class(state.class());
    }
}

fn row_dot(row: &gtk::ListBoxRow) -> Option<gtk::Label> {
    find_descendant(row.upcast_ref(), &|widget| {
        widget.has_css_class("agent-dot")
    })
    .and_downcast::<gtk::Label>()
}

/// The first widget below `widget` (depth first) that matches.
pub(super) fn find_descendant(
    widget: &gtk::Widget,
    matches: &dyn Fn(&gtk::Widget) -> bool,
) -> Option<gtk::Widget> {
    let mut child = widget.first_child();
    while let Some(current) = child {
        if matches(&current) {
            return Some(current);
        }
        if let Some(found) = find_descendant(&current, matches) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_and_task_filters_are_independent() {
        use crate::tasks::TaskStatus;
        assert!(matches_filters(0, 0, AgentState::Idle, None));
        assert!(matches_filters(
            3,
            1,
            AgentState::Done,
            Some(TaskStatus::Active)
        ));
        assert!(!matches_filters(
            3,
            4,
            AgentState::Done,
            Some(TaskStatus::Active)
        ));
        assert!(matches_filters(
            1,
            3,
            AgentState::Working,
            Some(TaskStatus::Review)
        ));
        assert!(!matches_filters(
            2,
            3,
            AgentState::Working,
            Some(TaskStatus::Review)
        ));
        assert!(!matches_filters(0, 1, AgentState::Working, None));
    }

    #[test]
    fn desktop_notifications_tell_where_what_and_how_much() {
        let news = |heading, place, message, stats| AgentNews {
            program: "claude",
            what: "is done",
            heading,
            place,
            message,
            stats,
        };
        assert_eq!(
            desktop_text(&news(
                "Fix login",
                "soi · main · #42",
                "Fixed\n  it.",
                "worked 3m"
            )),
            (
                "Claude Code is done · Fix login".into(),
                "soi · main · #42\nFixed it.\nworked 3m".into()
            )
        );
        assert_eq!(
            desktop_text(&news("", "", "", "")),
            ("Claude Code is done".into(), String::new())
        );
        let (_, body) = desktop_text(&news("", "", &"x".repeat(500), ""));
        assert_eq!(body.chars().count(), DESKTOP_MESSAGE_CHARS + 1);
        assert_eq!(notices::ticket("https://github.com/a/b/issues/42"), "#42");
        assert_eq!(
            notices::ticket("https://jira.example.com/browse/PROJ-7/"),
            "PROJ-7"
        );
    }

    #[test]
    fn search_needs_every_word_in_any_field() {
        let fields = ["Fix login", "PROJ-7", "Users can't sign in with SSO"];
        assert!(matches_search("", &fields));
        assert!(matches_search("proj-7", &fields));
        assert!(matches_search("login sso", &fields));
        assert!(!matches_search("login oauth", &fields));
    }

    #[test]
    fn elapsed_times() {
        use std::time::Duration;
        assert_eq!(elapsed(Duration::from_secs(5)), "5s");
        assert_eq!(elapsed(Duration::from_secs(125)), "2m");
        assert_eq!(elapsed(Duration::from_secs(7300)), "2h");
    }
}
