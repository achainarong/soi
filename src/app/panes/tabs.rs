//! The tabs in a pane and its tab strip.

use super::*;

/// Opens a tab in `cwd` (default: the current tab's directory) and shows it.
pub(in crate::app) fn new_tab(state: &App, pane: &gtk::Box, cwd: Option<PathBuf>) {
    let cwd = cwd.or_else(|| state.panes.borrow().get(pane).map(|pane| pane_cwd(pane)));
    new_main_tab(state, cwd);
}

/// Adds a tab with a shell in `cwd` at the end; returns its index. A tab
/// that had `id` before shows its saved output.
pub(in crate::app) fn add_tab(
    state: &App,
    pane: &gtk::Box,
    cwd: &Path,
    id: Option<Uuid>,
) -> Option<usize> {
    add_command_tab(state, pane, cwd, id, None, &[])
}

/// Like `add_tab`, running `command` instead of the shell when given, with
/// `extra_env` added to its environment.
pub(in crate::app) fn add_command_tab(
    state: &App,
    pane: &gtk::Box,
    cwd: &Path,
    id: Option<Uuid>,
    command: Option<Vec<String>>,
    extra_env: &[(String, String)],
) -> Option<usize> {
    let info = state.panes.borrow().get(pane).cloned()?;
    if !info.tabs.borrow().is_empty() {
        return None; // One terminal per pane; new terminals belong in main tabs or splits.
    }
    let cwd = if cwd.is_dir() {
        cwd.to_path_buf()
    } else if command.is_some() {
        // Explicit agent/resume launches must never silently fall back to home.
        return None;
    } else {
        home_dir()
    };
    let shell = command.is_none();
    let (command, scrollback, blink, integration) = {
        let settings = state.shared.settings.borrow();
        (
            command.unwrap_or_else(|| settings.shell_command()),
            settings.scrollback_lines(),
            settings.cursor_blink(),
            settings.shell_integration(),
        )
    };
    let id = id.unwrap_or_else(Uuid::new_v4);
    // Lets shell configs detect soi, and agents report to it.
    let mut env = panel::tab_env(id);
    env.push(("TERM_PROGRAM".into(), "soi".into()));
    if !shell {
        // Agents and the tools they run see the PATH of the user's shell.
        env.push(("PATH".into(), tasks::agent_path(state)));
    }
    env.extend(extra_env.iter().cloned());
    let command = if shell && integration {
        crate::shell_integration::wrap(command, &mut env)
    } else {
        command
    };
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let program = (!shell).then(|| command.first().cloned()).flatten();
    let terminal = TermView::new(
        command,
        &cwd,
        &env,
        &state.shared.theme.borrow(),
        scrollback as usize,
    );
    terminal.set_cursor_blink(blink);
    if let Some(program) = &program {
        terminal.set_program(program);
    }
    if let Some(saved) = session::scrollback::load(id) {
        terminal.restore(&saved);
        terminal.restore("\x1b[2m── restored ──\x1b[0m\r\n");
    }

    let title = gtk::Label::builder()
        .label(tilde(&cwd))
        .xalign(0.0)
        .width_chars(TITLE_MIN_CHARS)
        .max_width_chars(28)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    let close = header_button(
        &["window-close-symbolic"],
        &shortcut_hint(&state.shared, "Close tab", "close-tab"),
    );
    let dot = panel::new_dot();
    let progress = gtk::ProgressBar::builder()
        .css_classes(["tab-progress"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let button = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    button.add_css_class("pane-tab");
    button.append(&dot);
    button.append(&title);
    button.append(&progress);
    button.append(&close);
    info.strip.append(&button);
    info.stack.add_child(terminal.widget());

    let tab = Rc::new(Tab {
        id,
        terminal: terminal.clone(),
        agent: RefCell::default(),
        dot,
        button: button.clone(),
        title,
        reported_progress: Cell::new(None),
        close: close.clone(),
        cwd,
        saved_output: Cell::new(0),
    });
    info.tabs.borrow_mut().push(tab.clone());
    let index = info.tabs.borrow().len() - 1;
    panel::register(&state.shared, &tab);

    // Runs `f` for this tab, in whichever window and pane it is now (tabs
    // can be dragged to other panes).
    let for_tab = |f: fn(&App, &gtk::Box, &Rc<Tab>)| {
        let shared = Rc::downgrade(&state.shared);
        let tab_ref = Rc::downgrade(&tab);
        move || {
            if let (Some(shared), Some(tab)) = (shared.upgrade(), tab_ref.upgrade())
                && let Some(state) = window_of(&shared, tab.terminal.widget())
                && let Some(pane) = pane_of(&state, tab.terminal.widget())
            {
                f(&state, &pane, &tab);
            }
        }
    };
    let close_this = for_tab(request_close_tab);
    close.connect_clicked(move |_| close_this());
    let exited = for_tab(close_tab);
    terminal.connect_exited(exited);
    let focused = for_tab(|state, pane, _| set_focused(state, pane));
    let focus = gtk::EventControllerFocus::new();
    focus.connect_enter(move |_| focused());
    terminal.widget().add_controller(focus);
    let (shared, tab_ref) = (Rc::downgrade(&state.shared), Rc::downgrade(&tab));
    terminal.connect_input(move |input| {
        if let (Some(shared), Some(tab)) = (shared.upgrade(), tab_ref.upgrade())
            && let Some(state) = window_of(&shared, tab.terminal.widget())
        {
            mirror_input(&state, &tab, input);
        }
    });
    let (shared, tab_ref) = (Rc::downgrade(&state.shared), Rc::downgrade(&tab));
    terminal.connect_progress(move |reported| {
        show_progress(&progress, reported);
        if let (Some(shared), Some(tab)) = (shared.upgrade(), tab_ref.upgrade()) {
            tab.reported_progress.set(reported);
            if let Some(state) = window_of(&shared, tab.terminal.widget())
                && let Some((id, _)) = workspace_of(&state, tab.terminal.widget())
            {
                update_workspace_progress(&state, id);
            }
        }
    });
    // Shells set several titles in a row while starting up (or restoring);
    // showing only the last keeps the tab from flickering.
    let tab_ref = Rc::downgrade(&tab);
    let shared = Rc::downgrade(&state.shared);
    let pending: Rc<RefCell<Option<String>>> = Rc::default();
    terminal.connect_title_changed(move |title| {
        if title.is_empty() {
            return;
        }
        let scheduled = pending.replace(Some(title.to_string())).is_some();
        if scheduled {
            return;
        }
        let (tab_ref, pending, shared) = (tab_ref.clone(), pending.clone(), shared.clone());
        glib::timeout_add_local_once(TITLE_SETTLE, move || {
            if let Some(title) = pending.take()
                && let Some(tab) = tab_ref.upgrade()
                && tab.title.text() != title
            {
                tab.title.set_text(&title);
                if let Some(shared) = shared.upgrade()
                    && let Some(state) = window_of(&shared, tab.terminal.widget())
                    && let Some((_, ws)) = workspace_of(&state, tab.terminal.widget())
                    && let Some(main) = main_tab_of(&ws, tab.terminal.widget())
                    && main.last_focus.upgrade().as_ref()
                        == pane_of(&state, tab.terminal.widget()).as_ref()
                {
                    main.name.replace(title);
                    refresh_main_tabs(&ws);
                }
            }
        });
    });

    update_tab_strip(&info);
    Some(index)
}

/// How long a tab waits for a shell's titles to settle before showing one.
const TITLE_SETTLE: std::time::Duration = std::time::Duration::from_millis(150);
/// The narrowest a tab's title gets, so tabs don't resize with short titles.
const TITLE_MIN_CHARS: i32 = 16;

pub(in crate::app) fn tab_index(state: &App, pane: &gtk::Box, tab: &Rc<Tab>) -> Option<usize> {
    let panes = state.panes.borrow();
    let tabs = panes.get(pane)?.tabs.borrow();
    tabs.iter().position(|t| Rc::ptr_eq(t, tab))
}

/// Shows tab `index` of a pane (clamped), focusing it if asked.
pub(in crate::app) fn activate_tab(state: &App, pane: &gtk::Box, index: usize, focus: bool) {
    let Some(info) = state.panes.borrow().get(pane).cloned() else {
        return;
    };
    let tab = {
        let tabs = info.tabs.borrow();
        let index = index.min(tabs.len().saturating_sub(1));
        for (i, tab) in tabs.iter().enumerate() {
            if i == index {
                tab.button.add_css_class("active");
            } else {
                tab.button.remove_css_class("active");
            }
        }
        info.active.set(index);
        tabs.get(index).cloned()
    };
    if let Some(tab) = tab {
        info.stack.set_visible_child(tab.terminal.widget());
        if focus {
            if let Some((workspace, ws)) = workspace_of(state, pane)
                && let Some(main) = main_tab_of(&ws, pane)
            {
                select_main_tab_id(state, workspace, main.id);
            }
            tab.terminal.widget().grab_focus();
        }
    }
    panel::schedule_seen(&state.shared);
}

/// With a single tab, the header shows just its title, like a plain pane.
pub(in crate::app) fn update_tab_strip(info: &Pane) {
    let tabs = info.tabs.borrow();
    let single = tabs.len() == 1;
    if single {
        info.strip.add_css_class("single");
    } else {
        info.strip.remove_css_class("single");
    }
    for tab in tabs.iter() {
        tab.close.set_visible(!single);
    }
}

/// Closes a tab; closing the last one closes the pane.
pub(in crate::app) fn close_tab(state: &App, pane: &gtk::Box, tab: &Rc<Tab>) {
    if state.closing.get() {
        return;
    }
    let Some(info) = state.panes.borrow().get(pane).cloned() else {
        return;
    };
    let Some(index) = tab_index(state, pane, tab) else {
        return;
    };
    if info.tabs.borrow().len() == 1 {
        close_pane(state, pane);
        return;
    }
    let removed = info.tabs.borrow_mut().remove(index);
    remember_closed(state, &removed);
    info.strip.remove(&removed.button);
    info.stack.remove(removed.terminal.widget());
    let active = info.active.get();
    let next = if index < active { active - 1 } else { active };
    update_tab_strip(&info);
    activate_tab(state, pane, next, true);
}

/// Closes a tab, asking first if a program is still running in it.
pub(in crate::app) fn request_close_tab(state: &App, pane: &gtk::Box, tab: &Rc<Tab>) {
    let running: Vec<String> = tab.terminal.running_program().into_iter().collect();
    let (weak, pane_ref, tab_ref) = (Rc::downgrade(state), pane.downgrade(), Rc::downgrade(tab));
    confirm_close(state, "this tab", running, move || {
        if let (Some(state), Some(pane), Some(tab)) =
            (weak.upgrade(), pane_ref.upgrade(), tab_ref.upgrade())
        {
            close_tab(&state, &pane, &tab);
        }
    });
}

/// Keeps what's needed to reopen a tab being closed: its folder, and its
/// output, saved like that of open tabs so it survives a restart.
pub(in crate::app) fn remember_closed(state: &App, tab: &Tab) {
    if let Err(err) = session::scrollback::save(tab.id, &tab.terminal.export(SAVED_LINES)) {
        crate::log::write(&format!("failed to save output: {err}"));
    }
    remember(
        &state.shared,
        Closed::Tab {
            id: tab.id,
            cwd: tab_cwd(tab),
        },
    );
}

/// Adds something closed to what can be reopened.
pub(in crate::app) fn remember(shared: &Shared, closed: Closed) {
    let mut session = shared.session.borrow_mut();
    session.closed.push(closed);
    if session.closed.len() > CLOSED {
        session.closed.remove(0);
    }
    shared.session_dirty.set(true);
}

/// Opens what was closed last again: a tab in the focused pane (a fresh
/// shell in its folder, below its output), or a workspace as it was.
pub(in crate::app) fn reopen_tab(state: &App) {
    let Some(closed) = state.shared.session.borrow_mut().closed.pop() else {
        return;
    };
    state.shared.session_dirty.set(true);
    match closed {
        Closed::Tab { id, cwd } => {
            if let Some(workspace) = state.active.get() {
                let saved = crate::session::WorkspaceTab {
                    id: Uuid::new_v4(),
                    name: "Terminal".into(),
                    focused_pane: 0,
                    layout: Layout::Pane {
                        cwd,
                        tabs: Vec::new(),
                        active: 0,
                        ids: vec![id],
                        zoomed: false,
                    },
                };
                add_main_tab(state, workspace, &saved);
                select_main_tab_id(state, workspace, saved.id);
            }
        }
        Closed::MainTab(tab) => {
            if let Some(workspace) = state.active.get() {
                add_main_tab(state, workspace, &tab);
                select_main_tab_id(state, workspace, tab.id);
            }
        }
        Closed::Workspace(ws) => reopen_workspace(state, ws),
    }
}

/// Shows progress in the workspace's sidebar row too: of all its tabs
/// together, so it's seen from other workspaces.
pub(in crate::app) fn update_workspace_progress(state: &App, id: Uuid) {
    let Some(row) = find_row(state, &id.to_string()) else {
        return;
    };
    let Some(bar) = panel::find_descendant(row.upcast_ref(), &|w| w.has_css_class("ws-progress"))
        .and_downcast::<gtk::ProgressBar>()
    else {
        return;
    };
    let reported: Vec<Progress> = {
        let panes = state.panes.borrow();
        workspace_panes(state, id)
            .iter()
            .filter_map(|pane| panes.get(pane))
            .flat_map(|pane| {
                let tabs = pane.tabs.borrow();
                tabs.iter()
                    .filter_map(|tab| tab.reported_progress.get())
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    show_progress(&bar, combined(&reported));
}

/// Several programs' progress as one: errors first, else how far they
/// are on average, else busy.
fn combined(reported: &[Progress]) -> Option<Progress> {
    let percents: Vec<u32> = reported
        .iter()
        .filter_map(|p| match p {
            Progress::Normal(n) | Progress::Error(n) | Progress::Paused(n) => Some(*n as u32),
            Progress::Indeterminate => None,
        })
        .collect();
    let average = match percents.len() {
        0 => 0,
        n => (percents.iter().sum::<u32>() / n as u32) as u8,
    };
    if reported.iter().any(|p| matches!(p, Progress::Error(_))) {
        Some(Progress::Error(average))
    } else if !percents.is_empty() {
        let paused = reported.iter().all(|p| matches!(p, Progress::Paused(_)));
        Some(if paused {
            Progress::Paused(average)
        } else {
            Progress::Normal(average)
        })
    } else {
        reported.first().copied()
    }
}

/// How often a busy bar (no percentage) moves.
const PULSE: std::time::Duration = std::time::Duration::from_millis(80);

/// Shows a program's progress on its tab: a bar, red for errors, dimmed
/// while paused, moving back and forth when it can't tell how far it is.
fn show_progress(bar: &gtk::ProgressBar, progress: Option<Progress>) {
    for class in ["error", "paused", "indeterminate"] {
        bar.remove_css_class(class);
    }
    let (fraction, class) = match progress {
        None => {
            bar.set_visible(false);
            return;
        }
        Some(Progress::Normal(percent)) => (percent, None),
        Some(Progress::Error(percent)) => (percent, Some("error")),
        Some(Progress::Paused(percent)) => (percent, Some("paused")),
        Some(Progress::Indeterminate) => (0, Some("indeterminate")),
    };
    if let Some(class) = class {
        bar.add_css_class(class);
    }
    if class == Some("indeterminate") && !bar.has_css_class("pulsing") {
        bar.add_css_class("pulsing");
        bar.set_pulse_step(0.05);
        let weak = bar.downgrade();
        glib::timeout_add_local(PULSE, move || match weak.upgrade() {
            Some(bar) if bar.has_css_class("indeterminate") => {
                bar.pulse();
                glib::ControlFlow::Continue
            }
            Some(bar) => {
                bar.remove_css_class("pulsing");
                glib::ControlFlow::Break
            }
            None => glib::ControlFlow::Break,
        });
    }
    bar.set_fraction(f64::from(fraction) / 100.0);
    bar.set_tooltip_text(Some(&match progress {
        Some(Progress::Indeterminate) => "Busy".to_string(),
        Some(Progress::Error(_)) => format!("{fraction}%, with errors"),
        Some(Progress::Paused(_)) => format!("{fraction}%, paused"),
        _ => format!("{fraction}%"),
    }));
    bar.set_visible(true);
}
