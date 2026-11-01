//! Saving windows, their layouts and their terminals' output, and
//! building them again from what was saved.

use super::*;

pub(super) fn restore(state: &App, session: Option<&WindowSession>, fill: bool) {
    let mut tree = Vec::new();
    for entry in session.map_or(&[][..], |s| &s.sidebar) {
        match entry {
            Entry::Workspace(ws) => {
                add_workspace_ui(state, ws);
                tree.push(Node::Workspace(ws.id));
            }
            Entry::Group(group) => {
                for ws in &group.workspaces {
                    add_workspace_ui(state, ws);
                }
                tree.push(Node::Group {
                    id: group.id,
                    name: group.name.clone(),
                    collapsed: group.collapsed,
                    collection: group.collection.clone(),
                    workspaces: group.workspaces.iter().map(|ws| ws.id).collect(),
                });
            }
        }
    }
    *state.tree.borrow_mut() = tree;
    if fill {
        ensure_workspace(state);
    }

    let active = session
        .and_then(|s| s.active)
        .filter(|id| state.workspaces.borrow().contains_key(id))
        .or_else(|| workspace_order(state).first().copied());
    rebuild_sidebar(state);
    if let Some(id) = active {
        select_workspace(state, id);
    }
}

/// Synchronizes live contexts while retaining closed ones. Skip disk writes
/// unless a layout, name, active tab, cwd, or window size actually changed.
pub(super) fn save_all(shared: &Shared) {
    if shared.recovery_error.is_some() {
        return;
    }
    let mut session = shared.session.borrow_mut();
    let mut changed = shared.session_dirty.get();
    for window in shared.windows() {
        changed |= session.update_window(snapshot(&window));
    }
    if let Some((saving, receipt)) = shared.session_save.borrow().as_ref() {
        if receipt.status() == crate::dirs::SaveStatus::Saved {
            shared.save_failure.take();
            shared.persisted_session.replace(Some(saving.clone()));
            if *saving == *session {
                shared.session_dirty.set(false);
                return;
            }
        } else if *saving == *session {
            // The writer owns this revision until it succeeds, including
            // retries after failure. Do not reset its backoff every tick.
            return;
        }
    }
    if changed {
        match session.save() {
            Ok(receipt) => {
                shared
                    .session_save
                    .replace(Some((session.clone(), receipt)));
                shared.session_dirty.set(true);
            }
            Err(err) => {
                shared.session_dirty.set(true);
                shared
                    .save_failure
                    .replace(Some(format!("Cannot save session: {err}")));
                crate::log::write(&format!("failed to save session: {err}"));
            }
        }
    }
}

/// Observe the final write without taking another UI snapshot. Focusing the
/// saving dialog can change window metadata; that must not restart shutdown.
pub(super) fn acknowledge_session(shared: &Shared) {
    if let Some((saving, receipt)) = shared.session_save.borrow().as_ref()
        && receipt.status() == crate::dirs::SaveStatus::Saved
    {
        shared.persisted_session.replace(Some(saving.clone()));
        if *saving == *shared.session.borrow() {
            shared.session_dirty.set(false);
            shared.save_failure.take();
        }
    }
}

/// Saves the output of the windows' terminals that printed something since
/// the last time, and deletes saved output of tabs that are gone.
pub(super) fn save_output(shared: &Shared, windows: &[App]) {
    if shared.recovery_error.is_some() {
        return;
    }
    for state in windows {
        for pane in state.panes.borrow().values() {
            for tab in pane.tabs.borrow().iter() {
                save_tab_output(tab);
            }
        }
    }
    prune_output(shared);
}

/// `save_output` for every window, a terminal per turn of the main loop:
/// each takes milliseconds, and all at once would hold up typing.
pub(super) fn save_output_gradually(shared: &Rc<Shared>) {
    if shared.recovery_error.is_some() {
        return;
    }
    let mut tabs = shared
        .windows()
        .iter()
        .flat_map(|state| {
            let panes = state.panes.borrow();
            panes
                .values()
                .flat_map(|pane| {
                    pane.tabs
                        .borrow()
                        .iter()
                        .map(Rc::downgrade)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .into_iter();
    let weak = Rc::downgrade(shared);
    glib::idle_add_local(move || {
        let Some(shared) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if shared.shutting_down.get() {
            return glib::ControlFlow::Break;
        }
        match tabs.next() {
            Some(tab) => {
                if let Some(tab) = tab.upgrade() {
                    let started = std::time::Instant::now();
                    save_tab_output(&tab);
                    crate::log::if_slow("output save", started);
                }
                glib::ControlFlow::Continue
            }
            None => {
                prune_output(&shared);
                glib::ControlFlow::Break
            }
        }
    });
}

/// Saves a terminal's output if it printed something since the last time.
fn save_tab_output(tab: &Tab) {
    let serial = tab.terminal.output_serial();
    if let Some((saving, receipt)) = tab.output_save.borrow().as_ref() {
        if receipt.status() == crate::dirs::SaveStatus::Saved {
            tab.saved_output.set(*saving);
        } else if *saving == serial {
            return;
        }
    }
    if serial == tab.saved_output.get() || tab.untouched.get() {
        return;
    }
    match session::scrollback::save(tab.id, &tab.terminal.export(SAVED_LINES)) {
        Ok(receipt) => {
            tab.output_save.replace(Some((serial, receipt)));
        }
        Err(err) => crate::log::write(&format!("failed to save output: {err}")),
    }
}

/// Deletes saved output of tabs that are gone.
pub(super) fn prune_output(shared: &Shared) {
    if shared.recovery_error.is_some()
        || shared.session_dirty.get()
        || shared
            .session_save
            .borrow()
            .as_ref()
            .is_some_and(|(_, receipt)| receipt.status() != crate::dirs::SaveStatus::Saved)
    {
        return;
    }
    // What's in a window now or in a saved context (closed windows).
    let mut keep = shared.session.borrow().tab_ids();
    // A failed or in-flight layout save must not delete output still referred
    // to by the last successfully persisted layout.
    if let Some(saved) = shared.persisted_session.borrow().as_ref() {
        keep.extend(saved.tab_ids());
    }
    for state in shared.windows() {
        for pane in state.panes.borrow().values() {
            keep.extend(pane.tabs.borrow().iter().map(|tab| tab.id));
        }
    }
    session::scrollback::prune(&keep);
}

/// Starts the terminals of every window that aren't shown, at the size of
/// one that is (they're resized when shown).
pub(super) fn start_unshown_terminals(shared: &Shared) {
    if shared.shutting_down.get() {
        return;
    }
    for state in shared.windows() {
        let panes = state.panes.borrow();
        let tabs: Vec<_> = panes
            .values()
            .flat_map(|pane| pane.tabs.borrow().clone())
            .collect();
        let shown = tabs
            .iter()
            .map(|tab| tab.terminal.widget())
            .find(|widget| widget.width() > 0 && widget.height() > 0);
        let (width, height) = match shown {
            Some(widget) => (widget.width(), widget.height()),
            None => (state.window.width() * 3 / 4, state.window.height() * 4 / 5),
        };
        for tab in &tabs {
            tab.terminal.start_unshown(width, height);
        }
    }
}

/// A workspace as it's saved: its layout, tabs and folders.
pub(super) fn workspace_snapshot(state: &App, id: Uuid) -> Option<Workspace> {
    let ui = state.workspaces.borrow().get(&id)?.clone();
    let tabs = ui
        .tabs
        .borrow()
        .iter()
        .map(|tab| main_tab_snapshot(state, tab))
        .collect();
    Some(Workspace {
        id,
        name: ui.name.borrow().clone(),
        root_dir: ui.root_dir.clone(),
        tabs,
        active_tab: ui.active_tab.get(),
        task: ui.task.borrow().clone().map(Box::new),
    })
}

pub(super) fn snapshot(state: &App) -> WindowSession {
    let workspace = |id: &Uuid| workspace_snapshot(state, *id);
    let sidebar = state
        .tree
        .borrow()
        .iter()
        .filter_map(|node| match node {
            Node::Workspace(id) => workspace(id).map(Entry::Workspace),
            Node::Group {
                id,
                name,
                collapsed,
                collection,
                workspaces,
            } => Some(Entry::Group(Group {
                id: *id,
                name: name.clone(),
                collapsed: *collapsed,
                collection: collection.clone(),
                workspaces: workspaces.iter().filter_map(&workspace).collect(),
            })),
        })
        .collect();
    WindowSession {
        id: state.id.get(),
        closed: false,
        name: Some(state.name.borrow().clone()),
        agents_panel: state.agents.panel.get_visible(),
        sidebar,
        active: state.active.get(),
        last_used: state.last_used.get(),
        width: if state.window.width() > 0 {
            state.window.width()
        } else {
            state.window.default_width()
        },
        height: if state.window.height() > 0 {
            state.window.height()
        } else {
            state.window.default_height()
        },
        sidebar_width: state
            .window
            .child()
            .and_downcast::<gtk::Paned>()
            .map_or(0, |main| main.position()),
        agents_width: panel::width(state),
    }
}

pub(super) fn snapshot_layout(state: &App, widget: &gtk::Widget) -> Option<Layout> {
    if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
        let pending = state.ratios.borrow().get(paned).and_then(|r| r.get());
        let ratio = pending.unwrap_or_else(|| match paned.max_position() {
            max if max > 0 => paned.position() as f64 / max as f64,
            _ => 0.5,
        });
        return Some(Layout::Split {
            orientation: match paned.orientation() {
                gtk::Orientation::Vertical => session::Orientation::Vertical,
                _ => session::Orientation::Horizontal,
            },
            ratio: ratio.clamp(0.05, 0.95),
            start: Box::new(snapshot_layout(state, &paned.start_child()?)?),
            end: Box::new(snapshot_layout(state, &paned.end_child()?)?),
        });
    }
    let pane = widget.downcast_ref::<gtk::Box>()?;
    let panes = state.panes.borrow();
    let pane = panes.get(pane)?;
    let tabs: Vec<PathBuf> = pane.tabs.borrow().iter().map(|tab| tab_cwd(tab)).collect();
    let ids = pane.tabs.borrow().iter().map(|tab| tab.id).collect();
    let mut agents: Vec<_> = pane
        .tabs
        .borrow()
        .iter()
        .map(|tab| tab.resume.borrow().clone())
        .collect();
    // As loaded when none was saved, so unchanged layouts compare equal.
    if agents.iter().all(Option::is_none) {
        agents.clear();
    }
    let active = pane.active.get().min(tabs.len().saturating_sub(1));
    Some(Layout::Pane {
        cwd: tabs.get(active)?.clone(),
        active,
        tabs: if tabs.len() > 1 { tabs } else { Vec::new() },
        ids,
        agents,
        zoomed: widget.has_css_class("zoomed"),
    })
}

pub(super) fn build_layout(state: &App, layout: &Layout) -> gtk::Widget {
    match layout {
        Layout::Pane {
            cwd,
            tabs,
            active,
            ids,
            agents,
            zoomed,
        } => {
            let tabs = if tabs.is_empty() {
                std::slice::from_ref(cwd)
            } else {
                tabs.as_slice()
            };
            let pane = new_pane(state, tabs, ids, agents, *active);
            if *zoomed {
                // Zoomed once it's in its workspace (see `add_workspace_ui`).
                pane.add_css_class("zoom-when-shown");
            }
            pane.upcast()
        }
        Layout::Split {
            orientation,
            ratio,
            start,
            end,
        } => {
            let orientation = match orientation {
                session::Orientation::Horizontal => gtk::Orientation::Horizontal,
                session::Orientation::Vertical => gtk::Orientation::Vertical,
            };
            let start = build_layout(state, start);
            let end = build_layout(state, end);
            make_paned(state, orientation, *ratio, &start, &end).upcast()
        }
    }
}
