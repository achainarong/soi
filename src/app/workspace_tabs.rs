//! Main workspace tabs: each owns a split layout and remembers its focus.

use super::*;
use crate::session::WorkspaceTab;

pub(super) fn tab_panes(tab: &MainTabUi) -> Vec<gtk::Box> {
    let mut panes = Vec::new();
    if let Some(root) = tab.layout.first_child() {
        collect_panes(&root, &mut panes);
    }
    panes
}

pub(super) fn main_tab_of(
    ws: &WorkspaceUi,
    widget: &impl IsA<gtk::Widget>,
) -> Option<Rc<MainTabUi>> {
    ws.tabs
        .borrow()
        .iter()
        .find(|tab| widget.is_ancestor(&tab.layout))
        .cloned()
}

pub(super) fn add_main_tab(state: &App, workspace: Uuid, saved: &WorkspaceTab) {
    let root = build_layout(state, &saved.layout);
    add_main_tab_layout(state, workspace, saved, &root);
}

pub(super) fn add_main_tab_layout(
    state: &App,
    workspace: Uuid,
    saved: &WorkspaceTab,
    root: &impl IsA<gtk::Widget>,
) {
    let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
        return;
    };
    let layout = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    layout.set_hexpand(true);
    layout.set_vexpand(true);
    layout.append(root);
    let label = gtk::Label::builder()
        .label(&saved.name)
        .ellipsize(pango::EllipsizeMode::End)
        .max_width_chars(28)
        .build();
    let close = header_button(
        &["window-close-symbolic"],
        &shortcut_hint(&state.shared, "Close main tab", "close-tab"),
    );
    let dot = panel::new_dot();
    let progress = gtk::ProgressBar::builder()
        .css_classes(["tab-progress"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let button = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    button.add_css_class("pane-tab");
    button.append(&dot);
    button.append(&label);
    button.append(&progress);
    button.append(&close);
    // Before the new-tab button, which stays last.
    let last = ws.tabs.borrow().last().map(|tab| tab.button.clone());
    ws.tab_strip.insert_child_after(&button, last.as_ref());
    ws.tab_stack.add_named(&layout, Some(&saved.id.to_string()));
    let tab = Rc::new(MainTabUi {
        id: saved.id,
        name: RefCell::new(saved.name.clone()),
        button: button.clone(),
        label,
        dot,
        progress,
        layout,
        last_focus: glib::WeakRef::new(),
        zoomed: glib::WeakRef::new(),
        broadcast: Cell::new(false),
    });
    let panes = tab_panes(&tab);
    tab.last_focus
        .set(panes.get(saved.focused_pane).or(panes.first()));
    ws.tabs.borrow_mut().push(tab.clone());
    layout_drag::install_main_tab_drag(state, &tab);
    let click = gtk::GestureClick::new();
    let (shared, holder, id) = (Rc::downgrade(&state.shared), ws.holder.downgrade(), tab.id);
    click.connect_released(move |_, _, _, _| {
        if let (Some(shared), Some(holder)) = (shared.upgrade(), holder.upgrade())
            && let Some(state) = window_of(&shared, &holder)
        {
            select_main_tab_id(&state, workspace, id);
        }
    });
    button.add_controller(click);
    let (shared, holder, id) = (Rc::downgrade(&state.shared), ws.holder.downgrade(), tab.id);
    close.connect_clicked(move |_| {
        if let (Some(shared), Some(holder)) = (shared.upgrade(), holder.upgrade())
            && let Some(state) = window_of(&shared, &holder)
        {
            request_close_main_tab(&state, workspace, id);
        }
    });
    refresh_main_tabs(&ws);
}

pub(super) fn refresh_main_tabs(ws: &WorkspaceUi) {
    for (i, tab) in ws.tabs.borrow().iter().enumerate() {
        tab.label
            .set_text(&format!("{}: {}", i + 1, tab.name.borrow()));
        if i == ws.active_tab.get() {
            tab.button.add_css_class("active");
        } else {
            tab.button.remove_css_class("active");
        }
    }
}

pub(super) fn select_main_tab_id(state: &App, workspace: Uuid, id: Uuid) {
    let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
        return;
    };
    let index = ws.tabs.borrow().iter().position(|tab| tab.id == id);
    let Some(index) = index else { return };
    ws.active_tab.set(index);
    let tab = ws.current_tab();
    ws.tab_stack.set_visible_child(&tab.layout);
    refresh_main_tabs(&ws);
    select_workspace(state, workspace);
    panel::schedule_seen(&state.shared);
}

pub(super) fn select_main_tab(state: &App, n: usize) {
    let Some(workspace) = state.active.get() else {
        return;
    };
    let Some(ws) = active_workspace(state) else {
        return;
    };
    let id = ws.tabs.borrow().get(n.wrapping_sub(1)).map(|tab| tab.id);
    if let Some(id) = id {
        select_main_tab_id(state, workspace, id);
    }
}

pub(super) fn cycle_main_tab(state: &App, delta: isize) {
    let Some(ws) = active_workspace(state) else {
        return;
    };
    let count = ws.tabs.borrow().len();
    if count == 0 {
        return;
    }
    select_main_tab(
        state,
        (ws.active_tab.get() as isize + delta).rem_euclid(count as isize) as usize + 1,
    );
}

pub(super) fn new_main_tab(state: &App, cwd: Option<PathBuf>) -> Option<gtk::Box> {
    let workspace = state.active.get()?;
    let ws = active_workspace(state)?;
    let cwd = cwd.unwrap_or_else(|| workspace_cwd(state, &ws));
    let saved = WorkspaceTab {
        id: Uuid::new_v4(),
        name: "Terminal".into(),
        layout: Layout::pane(cwd),
        focused_pane: 0,
    };
    add_main_tab(state, workspace, &saved);
    select_main_tab_id(state, workspace, saved.id);
    current_pane(state)
}

pub(super) fn new_command_main_tab(
    state: &App,
    directory: &Path,
    command: Vec<String>,
    env: &[(String, String)],
) -> Option<gtk::Box> {
    if !directory.is_dir() {
        return None;
    }
    let workspace = state.active.get()?;
    let pane = new_pane(state, &[], &[], &[], 0);
    if add_command_tab(state, &pane, directory, None, Some(command), env).is_none() {
        state.panes.borrow_mut().remove(&pane);
        return None;
    }
    let saved = WorkspaceTab {
        id: Uuid::new_v4(),
        name: "Terminal".into(),
        layout: Layout::pane(directory.to_path_buf()),
        focused_pane: 0,
    };
    add_main_tab_layout(state, workspace, &saved, &pane);
    select_main_tab_id(state, workspace, saved.id);
    Some(pane)
}

pub(super) fn main_tab_snapshot(state: &App, tab: &MainTabUi) -> WorkspaceTab {
    let panes = tab_panes(tab);
    WorkspaceTab {
        id: tab.id,
        name: tab.name.borrow().clone(),
        layout: tab
            .layout
            .first_child()
            .and_then(|root| snapshot_layout(state, &root))
            .unwrap_or_else(|| Layout::pane(home_dir())),
        focused_pane: tab
            .last_focus
            .upgrade()
            .and_then(|pane| panes.iter().position(|p| p == &pane))
            .unwrap_or(0),
    }
}

pub(super) fn request_close_main_tab(state: &App, workspace: Uuid, id: Uuid) {
    let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
        return;
    };
    let Some(tab) = ws.tabs.borrow().iter().find(|tab| tab.id == id).cloned() else {
        return;
    };
    let running = running_in(state, &tab_panes(&tab));
    let weak = Rc::downgrade(state);
    confirm_close(
        state,
        "this main tab and all its panes",
        running,
        move || {
            if let Some(state) = weak.upgrade() {
                close_main_tab(&state, workspace, id);
            }
        },
    );
}

pub(super) fn close_main_tab(state: &App, workspace: Uuid, id: Uuid) {
    let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
        return;
    };
    let Some(index) = ws.tabs.borrow().iter().position(|tab| tab.id == id) else {
        return;
    };
    let tab = ws.tabs.borrow()[index].clone();
    let saved = main_tab_snapshot(state, &tab);
    for pane in tab_panes(&tab) {
        if let Some(info) = state.panes.borrow().get(&pane) {
            for terminal in info.tabs.borrow().iter() {
                if let Err(error) =
                    session::scrollback::save(terminal.id, &terminal.terminal.export(SAVED_LINES))
                {
                    crate::log::write(&format!("failed to save output: {error}"));
                }
            }
        }
    }
    remember(&state.shared, Closed::MainTab(saved));
    if let Some(root) = tab.layout.first_child() {
        forget_tree(state, &root);
    }
    remove_main_tab_ui(state, workspace, &ws, &tab, index);
    if state.active.get() == Some(workspace) {
        select_main_tab_id(state, workspace, ws.current_tab().id);
    }
}

fn remove_main_tab_ui(
    state: &App,
    workspace: Uuid,
    ws: &WorkspaceUi,
    tab: &MainTabUi,
    index: usize,
) {
    ws.tab_strip.remove(&tab.button);
    ws.tab_stack.remove(&tab.layout);
    ws.tabs.borrow_mut().remove(index);
    let active = ws.active_tab.get();
    ws.active_tab.set(if index < active {
        active - 1
    } else {
        active.min(ws.tabs.borrow().len().saturating_sub(1))
    });
    if ws.tabs.borrow().is_empty() {
        let saved = WorkspaceTab {
            id: Uuid::new_v4(),
            name: "Terminal".into(),
            layout: Layout::pane(ws.root_dir.clone()),
            focused_pane: 0,
        };
        add_main_tab(state, workspace, &saved);
    }
    let next = ws.current_tab();
    ws.tab_stack.set_visible_child(&next.layout);
    refresh_main_tabs(ws);
}

/// Detaches a live layout without closing or recording any terminals.
pub(super) fn take_main_tab(
    state: &App,
    workspace: Uuid,
    id: Uuid,
) -> Option<(gtk::Widget, WorkspaceTab)> {
    let ws = state.workspaces.borrow().get(&workspace)?.clone();
    let index = ws.tabs.borrow().iter().position(|tab| tab.id == id)?;
    let tab = ws.tabs.borrow()[index].clone();
    let root = tab.layout.first_child()?;
    unzoom_around(state, &first_pane(&root)?);
    let saved = main_tab_snapshot(state, &tab);
    tab.layout.remove(&root);
    remove_main_tab_ui(state, workspace, &ws, &tab, index);
    Some((root, saved))
}

pub(super) fn reorder_main_tab(state: &App, workspace: Uuid, id: Uuid, before: Option<Uuid>) {
    let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
        return;
    };
    let mut tabs = ws.tabs.borrow_mut();
    let Some(from) = tabs.iter().position(|tab| tab.id == id) else {
        return;
    };
    let at = match before {
        Some(before) => match tabs.iter().position(|tab| tab.id == before) {
            Some(at) => at,
            None => return,
        },
        None => tabs.len(),
    };
    let at = if from < at { at - 1 } else { at };
    let tab = tabs.remove(from);
    tabs.insert(at, tab.clone());
    let previous = at.checked_sub(1).map(|index| tabs[index].button.clone());
    ws.tab_strip
        .reorder_child_after(&tab.button, previous.as_ref());
    drop(tabs);
    select_main_tab_id(state, workspace, id);
}

pub(super) fn cycle_pane(state: &App, delta: isize) {
    let Some(ws) = active_workspace(state) else {
        return;
    };
    let tab = ws.current_tab();
    let panes = tab_panes(&tab);
    if panes.len() < 2 {
        return;
    }
    unzoom(&ws);
    let current = current_pane(state)
        .and_then(|pane| panes.iter().position(|p| p == &pane))
        .unwrap_or(0);
    let next = (current as isize + delta).rem_euclid(panes.len() as isize) as usize;
    focus_pane(state, &panes[next]);
}
