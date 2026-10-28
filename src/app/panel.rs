//! The agents panel: every coding agent running in any window, with its
//! state, on the right side of each window. Agents that stop working while
//! you look elsewhere are marked (panel row, tab, sidebar, a counter and a
//! notification under the agents) until you look at them.

use std::time::Instant;

use super::*;
use crate::agents::{self, Agent, State as AgentState};

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
    panel.append(&activity_filter);
    panel.append(&task_filter);
    panel.append(&split);
    // Which soi this is, faint in the bottom right corner.
    panel.append(
        &gtk::Label::builder()
            .label(concat!("soi ", env!("CARGO_PKG_VERSION")))
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
    let title = format!("{workspace}: {command} {how}");
    let body = format!("after {}", elapsed(finished.took));
    notify_desktop(shared, tab.id, &title, &body);
    add_notice(shared, tab.id, &format!("{title} {body}"));
}

/// Starts watching the terminals of all windows.
pub(super) fn start(shared: &Rc<Shared>) {
    let weak = Rc::downgrade(shared);
    glib::timeout_add_local(TICK, move || match weak.upgrade() {
        Some(shared) => {
            tick(&shared);
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

/// Looks at every terminal: which agent runs there, and is it busy?
fn tick(shared: &Rc<Shared>) {
    let names = agent_names(shared);
    let now = Instant::now();
    for tab in live_tabs(shared) {
        observe(&tab, &names, now);
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

fn observe(tab: &Tab, names: &[String], now: Instant) -> bool {
    // A wrapper (e.g. mate, under node) may lead; the agent runs below it.
    let programs = tab.terminal.running_programs();
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
// Reports from agents

/// Listens for reports from agents (see `crate::agents`).
pub(super) fn listen(shared: &Rc<Shared>) {
    let Some(path) = agents::socket_path() else {
        crate::log::write("no runtime directory: agents can't report their state");
        return;
    };
    // Only one soi runs per user (GTK makes sure), so an old socket file
    // is left over from a crash.
    let _ = fs::remove_file(&path);
    let service = gio::SocketService::new();
    let address = gio::UnixSocketAddress::new(&path);
    if let Err(err) = service.add_address(
        &address,
        gio::SocketType::Stream,
        gio::SocketProtocol::Default,
        None::<&glib::Object>,
    ) {
        crate::log::write(&format!("cannot listen on {}: {err}", path.display()));
        return;
    }
    // Only this user may report.
    let _ = fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    let weak = Rc::downgrade(shared);
    service.connect_incoming(move |_, connection, _| {
        let weak = weak.clone();
        let connection = connection.clone();
        glib::spawn_future_local(async move {
            let input = connection.input_stream();
            let Ok((data, n, _)) = input
                .read_all_future(vec![0u8; 64 * 1024], glib::Priority::DEFAULT)
                .await
            else {
                return;
            };
            if let (Some(shared), Some(report)) = (weak.upgrade(), agents::read_report(&data[..n]))
            {
                apply_report(&shared, report);
                if let Ok(report) = serde_json::from_slice::<agents::Report>(
                    data[..n].split(|b| *b == b'\n').next().unwrap_or_default(),
                ) && let Some(metadata) = report.session
                    && let Some(tab) = shared
                        .tabs
                        .borrow()
                        .get(&report.tab)
                        .and_then(Weak::upgrade)
                {
                    if !metadata.id.is_empty() && metadata.resume_command().is_ok() {
                        tab.resume.replace(Some(metadata.clone()));
                    }
                    tasks::record_report(&shared, &tab, metadata);
                }
            }
        });
        true
    });
    service.start();
    // Keep it for the life of the app.
    std::mem::forget(service);
}

fn apply_report(shared: &Rc<Shared>, (id, state, message): (Uuid, AgentState, String)) {
    let Some(tab) = shared.tabs.borrow().get(&id).and_then(Weak::upgrade) else {
        return;
    };
    let now = Instant::now();
    let visible = is_visible(&tab);
    let mut changed = tab.agent.borrow_mut().report(state, &message, visible, now);
    // Learn the agent's program right away.
    changed |= observe(&tab, &agent_names(shared), now);
    if changed {
        changed_agent(shared, &tab);
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
        for (tab, agent) in &agents {
            if !matches_filters(
                ui.activity_filter.selected(),
                ui.task_filter.selected(),
                agent.state,
                tasks::task_for_tab(shared, tab).map(|task| task.status),
            ) {
                continue;
            }
            let Some((owner, workspace, _)) = locate(shared, tab) else {
                continue;
            };
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

/// Tells about an agent that needs you, in every window.
fn announce(shared: &Rc<Shared>, tab: &Rc<Tab>, agent: &Agent) {
    let Some((owner, workspace, _)) = locate(shared, tab) else {
        return;
    };
    let workspace = owner
        .workspaces
        .borrow()
        .get(&workspace)
        .map(|ws| ws.name.borrow().clone())
        .unwrap_or_default();
    let what = match agent.state {
        AgentState::Input => "needs your input",
        AgentState::Done => "is done",
        AgentState::Idle => "stopped",
        AgentState::Exited => "exited",
        AgentState::Working => return,
    };
    let title = format!(
        "{workspace}: {} {what}",
        agent.program.as_deref().unwrap_or("agent")
    );
    let mut text = title.clone();
    if !agent.message.is_empty() {
        text.push_str(" — ");
        text.push_str(&agent.message);
    }
    notify_desktop(shared, tab.id, &title, &agent.message);
    add_notice(shared, tab.id, &text);
}

/// Adds a notification under the agents in every window, in place of the
/// tab's earlier one; it stays until dismissed or the tab is looked at.
fn add_notice(shared: &Rc<Shared>, tab: Uuid, text: &str) {
    remove_notice(shared, tab);
    for state in shared.windows() {
        let ui = &state.agents;
        ui.notices.prepend(&notice_row(shared, tab, text));
        while let Some(old) = ui.notices.row_at_index(MAX_NOTICES) {
            ui.notices.remove(&old);
        }
        if !ui.notices_box.is_visible() {
            ui.notices_box.set_visible(true);
            size_notices(&ui.split);
        }
    }
}

fn notice_row(shared: &Rc<Shared>, tab: Uuid, text: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(text)
        .tooltip_text(text)
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .lines(3)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    let show = gtk::Button::with_label("Show");
    show.add_css_class("flat");
    show.set_valign(gtk::Align::Center);
    let dismiss = gtk::Button::from_icon_name("window-close-symbolic");
    dismiss.add_css_class("flat");
    dismiss.set_valign(gtk::Align::Center);
    dismiss.set_tooltip_text(Some("Dismiss"));
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    content.add_css_class("agents-notice");
    content.append(&label);
    content.append(&show);
    content.append(&dismiss);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(false)
        .name(tab.to_string())
        .build();
    let weak = Rc::downgrade(shared);
    show.connect_clicked(move |_| {
        if let Some(shared) = weak.upgrade() {
            remove_notice(&shared, tab);
            focus_tab(&shared, tab);
        }
    });
    let weak = Rc::downgrade(shared);
    dismiss.connect_clicked(move |_| {
        if let Some(shared) = weak.upgrade() {
            remove_notice(&shared, tab);
        }
    });
    row
}

/// Removes the tab's notification from every window.
fn remove_notice(shared: &Shared, tab: Uuid) {
    let name = tab.to_string();
    for state in shared.windows() {
        let ui = &state.agents;
        let rows: Vec<_> = (0..).map_while(|i| ui.notices.row_at_index(i)).collect();
        for row in rows.iter().filter(|row| row.widget_name() == name.as_str()) {
            ui.notices.remove(row);
        }
        if ui.notices.row_at_index(0).is_none() {
            ui.notices_box.set_visible(false);
        }
    }
}

/// Gives the notifications their share of the panel's height.
fn size_notices(split: &gtk::Paned) {
    let height = split.height();
    if height > 0 {
        split.set_position((height as f64 * (1.0 - NOTICES_SHARE)) as i32);
    }
}

/// Tells the desktop about an agent that needs you, when no soi window is
/// in front (otherwise the notifications under the agents do). Clicking it goes to the agent.
fn notify_desktop(shared: &Shared, tab: Uuid, title: &str, body: &str) {
    let windows = shared.windows();
    if !shared.settings.borrow().desktop_notifications()
        || windows.iter().any(|state| state.window.is_active())
    {
        return;
    }
    let Some(app) = gio::Application::default() else {
        return;
    };
    let notification = gio::Notification::new(title);
    if !body.is_empty() {
        notification.set_body(Some(body));
    }
    notification.set_default_action_and_target_value(
        "app.focus-agent",
        Some(&tab.to_string().to_variant()),
    );
    app.send_notification(Some(&notification_id(tab)), &notification);
}

/// Removes an agent's desktop notification: you've seen it.
fn withdraw_notification(tab: Uuid) {
    if let Some(app) = gio::Application::default() {
        app.withdraw_notification(&notification_id(tab));
    }
}

fn notification_id(tab: Uuid) -> String {
    format!("agent-{tab}")
}

/// `app.focus-agent`, which desktop notifications run when clicked.
pub(super) fn install_app_action(app: &gtk::Application, shared: &Rc<Shared>) {
    let action = gio::SimpleAction::new("focus-agent", Some(glib::VariantTy::STRING));
    let weak = Rc::downgrade(shared);
    action.connect_activate(move |_, target| {
        let id = target
            .and_then(|v| v.get::<String>())
            .and_then(|id| Uuid::parse_str(&id).ok());
        if let (Some(shared), Some(id)) = (weak.upgrade(), id) {
            focus_tab(&shared, id);
        }
    });
    app.add_action(&action);
}

// ---------------------------------------------------------------------------
// Settings ▸ Agents

pub(super) fn settings_page(state: &App) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    page.set_margin_start(12);
    page.set_margin_end(12);
    let text = |text: &str, classes: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .css_classes(classes.to_vec())
            .build()
    };

    page.append(&text("Agents", &["soi-section"]));
    page.append(&text(
        "Tabs running one of these programs appear in the agents panel (◨ in the sidebar). \
         Separate names with commas; leave empty for the defaults.",
        &["dim-label", "caption"],
    ));
    let list = gtk::Entry::builder()
        .text(
            state
                .shared
                .settings
                .borrow()
                .agents
                .clone()
                .unwrap_or_default(),
        )
        .placeholder_text(agents::DEFAULT_AGENTS.join(", "))
        .build();
    let weak = Rc::downgrade(state);
    list.connect_changed(move |entry| {
        if let Some(state) = weak.upgrade() {
            let value = entry.text().trim().to_string();
            update_settings(&state, |s| s.agents = (!value.is_empty()).then_some(value));
        }
    });
    page.append(&list);

    let notify = gtk::CheckButton::builder()
        .label("Desktop notifications when soi isn't in front")
        .active(state.shared.settings.borrow().desktop_notifications())
        .build();
    let weak = Rc::downgrade(state);
    notify.connect_toggled(move |check| {
        if let Some(state) = weak.upgrade() {
            let on = check.is_active();
            update_settings(&state, |s| s.desktop_notifications = Some(on));
        }
    });
    page.append(&notify);
    let long = gtk::SpinButton::with_range(0.0, 86_400.0, 5.0);
    long.set_value(
        state
            .shared
            .settings
            .borrow()
            .long_command
            .unwrap_or(settings::DEFAULT_LONG_COMMAND) as f64,
    );
    let weak = Rc::downgrade(state);
    long.connect_value_changed(move |spin| {
        if let Some(state) = weak.upgrade() {
            let seconds = spin.value() as u32;
            update_settings(&state, |s| s.long_command = Some(seconds));
        }
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&gtk::Label::new(Some(
        "Say when a command ends that ran at least (seconds; 0: never)",
    )));
    row.append(&long);
    page.append(&row);
    page.append(&text(
        "When an agent finishes or asks something while you're in another app. \
         Click the notification to go to the agent.",
        &["dim-label", "caption"],
    ));

    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&text("Agents to start", &["soi-section"]));
    page.append(&text(
        "What Start with brief and New task worktree offer. Add a tool more than once \
         to start it with different models, say OpenCode with Qwen and with GPT. The model \
         is passed as you write it: `opencode models` lists OpenCode's (provider/model), \
         Claude Code takes an alias like opus or a full name like claude-opus-5-5, \
         and `pi --list-models` lists Pi's. \
         To start one through a wrapper, like mate (mate claude …), put it under Start \
         through; its own options go in the arguments after --, e.g. -- --no-git. \
         Under each agent is the command it starts.",
        &["dim-label", "caption"],
    ));
    page.append(&presets_section());

    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&text("Exact states", &["soi-section"]));
    page.append(&text(
        "Without help, soi sees whether an agent is busy or quiet. With these, \
         Claude Code, OpenCode and Pi tell soi when they need your input and when they're done.",
        &["dim-label", "caption"],
    ));
    page.append(&integration_row(
        state,
        Integration {
            name: "Claude Code",
            description: format!(
                "Adds hooks to {} that run soi when Claude works, asks or finishes. \
                 Your other settings and hooks stay as they are.",
                tilde(&agents::claude_settings_path())
            ),
            preview: format!(
                "For SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Notification and \
                 Stop, a hook running:\n\n{}",
                agents::CLAUDE_COMMAND
            ),
            installed: agents::claude_hooks_installed,
            install: agents::install_claude_hooks,
        },
    ));
    page.append(&integration_row(
        state,
        Integration {
            name: "OpenCode",
            description: format!(
                "Writes a small plugin to {} that tells soi what OpenCode is doing.",
                tilde(&agents::opencode_plugin_path())
            ),
            preview: format!(
                "Writes {}: a plugin that sends OpenCode's busy, permission, question, idle \
                 and error events to soi.",
                tilde(&agents::opencode_plugin_path())
            ),
            installed: agents::opencode_plugin_installed,
            install: agents::install_opencode_plugin,
        },
    ));
    page.append(&integration_row(
        state,
        Integration {
            name: "Pi",
            description: format!(
                "Writes a small extension to {} that tells soi what Pi is doing.",
                tilde(&agents::pi_extension_path())
            ),
            preview: format!(
                "Writes {}: an extension that sends Pi's run, prompt and error events to soi.",
                tilde(&agents::pi_extension_path())
            ),
            installed: agents::pi_extension_installed,
            install: agents::install_pi_extension,
        },
    ));
    page.append(&text(
        "Restart running agents afterwards. Outside soi, the hooks, the plugin and the extension do nothing.",
        &["dim-label", "caption"],
    ));
    page.upcast()
}

struct Integration {
    name: &'static str,
    description: String,
    /// What installing changes, shown before doing it.
    preview: String,
    installed: fn() -> bool,
    install: fn() -> Result<(), String>,
}

fn integration_row(state: &App, integration: Integration) -> gtk::Widget {
    let Integration {
        name,
        description,
        preview,
        installed,
        install,
    } = integration;
    let title = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let status = gtk::Label::new(None);
    status.add_css_class("caption");
    let button = gtk::Button::new();
    let update = {
        let (status, button) = (status.clone(), button.clone());
        move || {
            let done = installed();
            status.set_label(if done { "Installed" } else { "Not installed" });
            if done {
                status.add_css_class("soi-note");
            } else {
                status.remove_css_class("soi-note");
            }
            button.set_label(if done { "Reinstall" } else { "Install" });
        }
    };
    update();
    let update = Rc::new(update);
    let weak = Rc::downgrade(state);
    button.connect_clicked(move |button| {
        let Some(state) = weak.upgrade() else { return };
        let parent = button
            .root()
            .and_downcast::<gtk::Window>()
            .unwrap_or_else(|| state.window.clone().upcast());
        let confirm = gtk::AlertDialog::builder()
            .message(format!("Set up {name} for soi?"))
            .detail(&preview)
            .buttons(["Cancel", "Set up"])
            .cancel_button(0)
            .default_button(1)
            .modal(true)
            .build();
        let update = update.clone();
        let window = parent.clone();
        confirm.choose(Some(&parent), gio::Cancellable::NONE, move |answer| {
            if answer != Ok(1) {
                return;
            }
            let dialog = match install() {
                Ok(()) => gtk::AlertDialog::builder()
                    .message(format!("{name} now reports to soi"))
                    .detail("Agents started from now on show exact states. Restart running ones.")
                    .build(),
                Err(err) => gtk::AlertDialog::builder()
                    .message(format!("Couldn't set up {name}"))
                    .detail(err)
                    .build(),
            };
            dialog.show(Some(&window));
            update();
        });
    });
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    top.append(&title);
    top.append(&status);
    top.append(&button);
    let description = gtk::Label::builder()
        .label(&description)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.append(&top);
    row.append(&description);
    row.upcast()
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
    fn elapsed_times() {
        use std::time::Duration;
        assert_eq!(elapsed(Duration::from_secs(5)), "5s");
        assert_eq!(elapsed(Duration::from_secs(125)), "2m");
        assert_eq!(elapsed(Duration::from_secs(7300)), "2h");
    }
}
