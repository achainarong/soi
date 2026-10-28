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
    let mut session = shared.session.borrow_mut();
    let mut changed = shared.session_dirty.get();
    for window in shared.windows() {
        changed |= session.update_window(snapshot(&window));
    }
    if changed {
        match session.save() {
            Ok(()) => shared.session_dirty.set(false),
            Err(err) => {
                shared.session_dirty.set(true);
                crate::log::write(&format!("failed to save session: {err}"));
            }
        }
    }
}

/// Saves the output of the windows' terminals that printed something since
/// the last time, and deletes saved output of tabs that are gone.
pub(super) fn save_output(shared: &Shared, windows: &[App]) {
    for state in windows {
        for pane in state.panes.borrow().values() {
            for tab in pane.tabs.borrow().iter() {
                let serial = tab.terminal.output_serial();
                if serial == tab.saved_output.get() {
                    continue;
                }
                match session::scrollback::save(tab.id, &tab.terminal.export(SAVED_LINES)) {
                    Ok(()) => tab.saved_output.set(serial),
                    Err(err) => crate::log::write(&format!("failed to save output: {err}")),
                }
            }
        }
    }
    // What's in a window now or in a saved context (closed windows).
    let mut keep = shared.session.borrow().tab_ids();
    for state in shared.windows() {
        for pane in state.panes.borrow().values() {
            keep.extend(pane.tabs.borrow().iter().map(|tab| tab.id));
        }
    }
    session::scrollback::prune(&keep);
}

/// A workspace as it's saved: its layout, tabs and folders.
pub(super) fn workspace_snapshot(state: &App, id: Uuid) -> Option<Workspace> {
    let ui = state.workspaces.borrow().get(&id)?.clone();
    let layout = ui
        .layout
        .first_child()
        .and_then(|root| snapshot_layout(state, &root))
        .unwrap_or_else(|| Layout::pane(ui.root_dir.clone()));
    Some(Workspace {
        id,
        name: ui.name.borrow().clone(),
        root_dir: ui.root_dir.clone(),
        layout,
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
                workspaces,
            } => Some(Entry::Group(Group {
                id: *id,
                name: name.clone(),
                collapsed: *collapsed,
                workspaces: workspaces.iter().filter_map(&workspace).collect(),
            })),
        })
        .collect();
    WindowSession {
        id: state.id.get(),
        closed: false,
        name: Some(state.name.borrow().clone()),
        agents_panel: state.agents.panel.is_visible(),
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
    let active = pane.active.get().min(tabs.len().saturating_sub(1));
    Some(Layout::Pane {
        cwd: tabs.get(active)?.clone(),
        active,
        tabs: if tabs.len() > 1 { tabs } else { Vec::new() },
        ids,
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
            zoomed,
        } => {
            let tabs = if tabs.is_empty() {
                std::slice::from_ref(cwd)
            } else {
                tabs.as_slice()
            };
            let pane = new_pane(state, tabs, ids, *active);
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
