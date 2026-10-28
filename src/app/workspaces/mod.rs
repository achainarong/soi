//! Workspaces: creating, showing, cycling, moving, closing and archiving them.

use super::*;

mod archive;
mod groups;

pub(super) use archive::*;
pub(super) use groups::*;

pub(super) fn add_workspace_ui(state: &App, ws: &Workspace) {
    let layout = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    layout.set_hexpand(true);
    layout.append(&build_layout(state, &ws.layout));
    // A window resize goes to the terminals; the plan keeps its width.
    let plan_split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&layout)
        .resize_start_child(true)
        .resize_end_child(false)
        .shrink_end_child(false)
        .hexpand(true)
        .build();
    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    holder.append(&plan_split);
    state.stack.add_named(&holder, Some(&ws.id.to_string()));
    state.workspaces.borrow_mut().insert(
        ws.id,
        Rc::new(WorkspaceUi {
            name: RefCell::new(ws.name.clone()),
            root_dir: ws.root_dir.clone(),
            task: RefCell::new(ws.task.as_deref().cloned()),
            holder,
            plan_split,
            layout,
            plan_width: Cell::new(preview::PLAN_WIDTH),
            last_focus: glib::WeakRef::new(),
            zoomed: glib::WeakRef::new(),
            broadcast: Cell::new(false),
            plan_panel: RefCell::new(None),
        }),
    );
    let zoomed = workspace_panes(state, ws.id)
        .into_iter()
        .find(|pane| pane.has_css_class("zoom-when-shown"));
    if let Some(pane) = zoomed {
        pane.remove_css_class("zoom-when-shown");
        zoom_pane(state, &pane);
    }
}

/// Makes sure there is at least one workspace to show.
pub(super) fn ensure_workspace(state: &App) {
    if state.workspaces.borrow().is_empty() {
        let ws = Workspace::new(state.shared.settings.borrow().start_dir());
        add_workspace_ui(state, &ws);
        state.tree.borrow_mut().push(Node::Workspace(ws.id));
    }
}

pub(super) fn active_workspace(state: &App) -> Option<Rc<WorkspaceUi>> {
    state.workspaces.borrow().get(&state.active.get()?).cloned()
}

pub(super) fn workspace_of(
    state: &App,
    widget: &impl IsA<gtk::Widget>,
) -> Option<(Uuid, Rc<WorkspaceUi>)> {
    state
        .workspaces
        .borrow()
        .iter()
        .find(|(_, ws)| widget.is_ancestor(&ws.holder))
        .map(|(id, ws)| (*id, ws.clone()))
}

/// All workspaces in sidebar order, including those in collapsed groups.
pub(super) fn workspace_order(state: &App) -> Vec<Uuid> {
    let mut order = Vec::new();
    for node in state.tree.borrow().iter() {
        match node {
            Node::Workspace(id) => order.push(*id),
            Node::Group { workspaces, .. } => order.extend(workspaces),
        }
    }
    order
}

/// Shows a workspace and selects it in the sidebar, expanding its group.
pub(super) fn select_workspace(state: &App, id: Uuid) {
    let mut expanded = false;
    for node in state.tree.borrow_mut().iter_mut() {
        if let Node::Group {
            collapsed,
            workspaces,
            ..
        } = node
            && *collapsed
            && workspaces.contains(&id)
        {
            *collapsed = false;
            expanded = true;
        }
    }
    state.active.set(Some(id));
    if expanded {
        rebuild_sidebar(state);
    } else if let Some(row) = find_row(state, &id.to_string()) {
        state.list.select_row(Some(&row));
    }
    show_workspace(state, id);
}

pub(super) fn show_workspace(state: &App, id: Uuid) {
    state.active.set(Some(id));
    let group = group_of(&state.tree.borrow(), id);
    state.last_in_group.borrow_mut().insert(group, id);
    let Some(ws) = active_workspace(state) else {
        return;
    };
    state.stack.set_visible_child(&ws.holder);
    update_title(state);
    panel::schedule_seen(&state.shared);
    if let Some(pane) = current_pane(state) {
        focus_pane(state, &pane);
    }
}

pub(super) fn cycle_workspace(state: &App, delta: isize) {
    let order = workspace_order(state);
    if order.is_empty() {
        return;
    }
    let current = state
        .active
        .get()
        .and_then(|id| order.iter().position(|w| *w == id))
        .unwrap_or(0);
    let next = (current as isize + delta).rem_euclid(order.len() as isize) as usize;
    select_workspace(state, order[next]);
}

pub(super) fn pick_project(state: &App, group: Option<Uuid>) {
    let dialog = gtk::FileDialog::builder()
        .title("Open project folder")
        .modal(true)
        .build();
    let weak = Rc::downgrade(state);
    dialog.select_folder(Some(&state.window), gio::Cancellable::NONE, move |result| {
        let (Some(state), Ok(folder)) = (weak.upgrade(), result) else {
            return;
        };
        let Some(path) = folder.path() else {
            return;
        };
        insert_workspace(&state, Workspace::new(path), group);
    });
}

/// A new workspace with a shell in the profile's start directory.
pub(super) fn new_terminal_workspace(state: &App, group: Option<Uuid>) {
    let mut ws = Workspace::new(state.shared.settings.borrow().start_dir());
    let taken: Vec<String> = state
        .workspaces
        .borrow()
        .values()
        .map(|ws| ws.name.borrow().clone())
        .collect();
    ws.name = (1..)
        .map(|n| match n {
            1 => "Terminal".to_string(),
            n => format!("Terminal {n}"),
        })
        .find(|name| !taken.contains(name))
        .expect("unbounded");
    insert_workspace(state, ws, group);
}

/// Adds a workspace to `group`, or else right after the active workspace
/// (inside its group, if it has one), and shows it.
pub(super) fn insert_workspace(state: &App, ws: Workspace, group: Option<Uuid>) {
    add_workspace_ui(state, &ws);
    {
        let mut tree = state.tree.borrow_mut();
        let dest = match group {
            Some(group) => match locate(&tree, Item::Group(group)) {
                Some(Loc::Top(i)) => match &tree[i] {
                    Node::Group { workspaces, .. } => Loc::InGroup(i, workspaces.len()),
                    Node::Workspace(_) => Loc::Top(tree.len()),
                },
                _ => Loc::Top(tree.len()),
            },
            None => match state
                .active
                .get()
                .and_then(|active| locate(&tree, Item::Workspace(active)))
            {
                Some(Loc::Top(i)) => Loc::Top(i + 1),
                Some(Loc::InGroup(i, j)) => Loc::InGroup(i, j + 1),
                None => Loc::Top(tree.len()),
            },
        };
        match dest {
            Loc::InGroup(i, j) => {
                if let Node::Group {
                    workspaces,
                    collapsed,
                    ..
                } = &mut tree[i]
                {
                    workspaces.insert(j.min(workspaces.len()), ws.id);
                    *collapsed = false;
                }
            }
            Loc::Top(i) => {
                let i = i.min(tree.len());
                tree.insert(i, Node::Workspace(ws.id));
            }
        }
    }
    state.active.set(Some(ws.id));
    rebuild_sidebar(state);
    select_workspace(state, ws.id);
}

/// Moves a workspace, with its running terminals, to another window
/// (`target`: index into the open windows, `None`: a new window).
pub(super) fn move_to_window(from: &App, id: Uuid, target: Option<usize>) {
    let to = match target.and_then(|i| from.shared.windows().get(i).cloned()) {
        Some(to) if Rc::ptr_eq(&to, from) => return,
        Some(to) => to,
        None => new_window(&from.shared, None, false),
    };
    detach(&mut from.tree.borrow_mut(), id);
    let Some(ws) = from.workspaces.borrow_mut().remove(&id) else {
        return;
    };
    from.stack.remove(&ws.holder);
    if let Some(root) = ws.layout.first_child() {
        hand_over(from, &to, &root);
    }
    to.stack.add_named(&ws.holder, Some(&id.to_string()));
    to.workspaces.borrow_mut().insert(id, ws);
    to.tree.borrow_mut().push(Node::Workspace(id));

    // The window it came from keeps at least one workspace.
    ensure_workspace(from);
    if let Some(next) = workspace_order(from).first().copied() {
        from.active.set(Some(next));
        rebuild_sidebar(from);
        select_workspace(from, next);
    }
    to.active.set(Some(id));
    rebuild_sidebar(&to);
    select_workspace(&to, id);
    to.window.present();
    save_all(&from.shared);
}

/// Moves the bookkeeping of the panes and splits below `widget` to another
/// window (their widgets move along with the workspace).
pub(super) fn hand_over(from: &App, to: &App, widget: &gtk::Widget) {
    if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
        if let Some(ratio) = from.ratios.borrow_mut().remove(paned) {
            to.ratios.borrow_mut().insert(paned.clone(), ratio);
        }
        for child in [paned.start_child(), paned.end_child()]
            .into_iter()
            .flatten()
        {
            hand_over(from, to, &child);
        }
    } else if let Some(pane) = widget.downcast_ref::<gtk::Box>()
        && let Some(info) = from.panes.borrow_mut().remove(pane)
    {
        to.panes.borrow_mut().insert(pane.clone(), info);
    }
}

pub(super) fn close_workspace(state: &App, id: Uuid) {
    if let Some(ws) = keep_workspace(state, id) {
        remember(&state.shared, Closed::Workspace(ws));
    }
    discard_workspace(state, id);
}

/// What's needed to bring a workspace back: its layout, and its tabs'
/// output (saved now).
fn keep_workspace(state: &App, id: Uuid) -> Option<Workspace> {
    let ws = workspace_snapshot(state, id)?;
    for pane in workspace_panes(state, id) {
        if let Some(info) = state.panes.borrow().get(&pane) {
            for tab in info.tabs.borrow().iter() {
                let output = tab.terminal.export(SAVED_LINES);
                if let Err(err) = session::scrollback::save(tab.id, &output) {
                    crate::log::write(&format!("failed to save output: {err}"));
                }
            }
        }
    }
    Some(ws)
}

/// Puts a workspace away for good, unlike closing it: it stays in the
/// archive however much is closed after it. Its folders are left alone.
pub(super) fn archive_workspace(state: &App, id: Uuid) {
    if let Some(ws) = keep_workspace(state, id) {
        record_task(&ws, "Archived");
        state.shared.session.borrow_mut().archived.push(ws);
        state.shared.session_dirty.set(true);
    }
    discard_workspace(state, id);
}

/// Brings an archived workspace back into this window's sidebar.
pub(super) fn unarchive_workspace(state: &App, id: Uuid) {
    let ws = {
        let mut session = state.shared.session.borrow_mut();
        let Some(index) = session.archived.iter().position(|ws| ws.id == id) else {
            return;
        };
        session.archived.remove(index)
    };
    state.shared.session_dirty.set(true);
    record_task(&ws, "Restored from the archive");
    reopen_workspace(state, ws);
}

/// Notes `event` in the history of a task workspace's task, so it shows
/// outside soi too.
fn record_task(ws: &Workspace, event: &str) {
    if let Some(task) = &ws.task
        && let Err(e) = task.record(&[event.into()])
    {
        crate::log::write(&format!("failed to record task history: {e}"));
    }
}

/// Closes a workspace for good: it can't be reopened (see `close_workspace`).
pub(super) fn discard_workspace(state: &App, id: Uuid) {
    let order = workspace_order(state);
    let index = order.iter().position(|w| *w == id).unwrap_or(0);
    detach(&mut state.tree.borrow_mut(), id);
    let Some(ws) = state.workspaces.borrow_mut().remove(&id) else {
        return;
    };
    if let Some(root) = ws.layout.first_child() {
        forget_tree(state, &root);
    }
    state.stack.remove(&ws.holder);
    ensure_workspace(state);

    let order = workspace_order(state);
    let next = order[index.min(order.len() - 1)];
    state.active.set(Some(next));
    rebuild_sidebar(state);
    select_workspace(state, next);
}

/// Removes a workspace from the sidebar tree. Returns the top-level index
/// where a node taking its place should be inserted.
pub(super) fn detach(tree: &mut Vec<Node>, id: Uuid) -> Option<usize> {
    if let Some(i) = tree
        .iter()
        .position(|n| matches!(n, Node::Workspace(w) if *w == id))
    {
        tree.remove(i);
        return Some(i);
    }
    for (i, node) in tree.iter_mut().enumerate() {
        if let Node::Group { workspaces, .. } = node
            && let Some(j) = workspaces.iter().position(|w| *w == id)
        {
            workspaces.remove(j);
            return Some(i + 1);
        }
    }
    None
}

pub(super) fn rename_workspace(state: &App, id: Uuid) {
    start_inline_rename(state, &id.to_string());
}

/// Brings back a closed workspace (see `close_workspace`) at the end of the
/// sidebar, and shows it.
pub(super) fn reopen_workspace(state: &App, mut ws: Workspace) {
    if state.workspaces.borrow().contains_key(&ws.id) {
        ws.id = Uuid::new_v4();
    }
    add_workspace_ui(state, &ws);
    state.tree.borrow_mut().push(Node::Workspace(ws.id));
    rebuild_sidebar(state);
    select_workspace(state, ws.id);
}
