//! Panes: splitting, focusing and closing them.

use super::*;

mod tab_drop;
mod tabs;

pub(super) use tab_drop::*;
pub(super) use tabs::*;

/// A pane with a tab for each directory, showing tab `active`. Tabs with
/// an id from `ids` come back with their saved output.
pub(super) fn new_pane(state: &App, tabs: &[PathBuf], ids: &[Uuid], active: usize) -> gtk::Box {
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    strip.add_css_class("pane-tabs");
    strip.set_hexpand(true);
    let hint = |label, id| shortcut_hint(&state.shared, label, id);
    let new_tab_btn = header_button(&["list-add-symbolic"], &hint("New tab", "new-tab"));
    let search = header_button(&["edit-find-symbolic"], &hint("Search", "search"));
    let split_right = header_button(
        &["view-split-left-right-symbolic", "view-dual-symbolic"],
        &hint("Split right", "split-right"),
    );
    let split_down = header_button(
        &["view-split-top-bottom-symbolic", "view-paged-symbolic"],
        &hint("Split down", "split-down"),
    );
    let zoom = header_button(
        &["view-fullscreen-symbolic"],
        &hint("Zoom pane", "zoom-pane"),
    );
    zoom.add_css_class("zoom-button");
    let close = header_button(&["window-close-symbolic"], "Close pane and its tabs");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    header.add_css_class("pane-header");
    header.append(&strip);
    header.append(&new_tab_btn);
    header.append(&search);
    header.append(&split_right);
    header.append(&split_down);
    header.append(&zoom);
    header.append(&close);

    let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
    let drop_hint = gtk::Box::builder()
        .css_classes(["drop-hint"])
        .can_target(false)
        .visible(false)
        .build();
    let overlay = gtk::Overlay::builder().child(&stack).build();
    overlay.add_overlay(&drop_hint);
    let pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    pane.add_css_class("pane");
    pane.append(&header);
    pane.append(&overlay);
    state.panes.borrow_mut().insert(
        pane.clone(),
        Rc::new(Pane {
            tabs: RefCell::default(),
            active: Cell::new(0),
            stack,
            strip,
            drop_hint,
        }),
    );
    install_tab_drop(state, &pane);

    let on_click = |button: &gtk::Button, f: fn(&App, &gtk::Box)| {
        let (shared, pane_ref) = (Rc::downgrade(&state.shared), pane.downgrade());
        button.connect_clicked(move |_| {
            if let (Some(shared), Some(pane)) = (shared.upgrade(), pane_ref.upgrade())
                && let Some(state) = window_of(&shared, &pane)
            {
                f(&state, &pane);
            }
        });
    };
    on_click(&new_tab_btn, |s, p| new_tab(s, p, None));
    on_click(&search, |s, p| {
        if let Some(terminal) = s.panes.borrow().get(p).and_then(|p| p.terminal()) {
            terminal.browse(true);
        }
    });
    on_click(&split_right, |s, p| {
        split_pane(s, p, gtk::Orientation::Horizontal)
    });
    on_click(&split_down, |s, p| {
        split_pane(s, p, gtk::Orientation::Vertical)
    });
    on_click(&zoom, zoom_pane);
    on_click(&close, |s, p| {
        let running = running_in(s, std::slice::from_ref(p));
        let (weak, pane) = (Rc::downgrade(s), p.downgrade());
        confirm_close(s, "this pane", running, move || {
            if let (Some(s), Some(p)) = (weak.upgrade(), pane.upgrade()) {
                close_pane(&s, &p);
            }
        });
    });

    for (i, cwd) in tabs.iter().enumerate() {
        add_tab(state, &pane, cwd, ids.get(i).copied());
    }
    activate_tab(state, &pane, active, false);
    pane
}

/// The pane a widget (e.g. a tab's terminal) is in.
pub(super) fn pane_of(state: &App, widget: &gtk::Widget) -> Option<gtk::Box> {
    let panes = state.panes.borrow();
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if let Ok(pane) = widget.clone().downcast::<gtk::Box>()
            && panes.contains_key(&pane)
        {
            return Some(pane);
        }
        parent = widget.parent();
    }
    None
}

/// The programs still running (not just a shell at its prompt) in the
/// tabs of these panes.
pub(super) fn running_in(state: &App, panes: &[gtk::Box]) -> Vec<String> {
    let all = state.panes.borrow();
    let mut running: Vec<String> = panes
        .iter()
        .filter_map(|pane| all.get(pane))
        .flat_map(|pane| {
            pane.tabs
                .borrow()
                .iter()
                .filter_map(|tab| tab.terminal.running_program())
                .collect::<Vec<_>>()
        })
        .collect();
    running.sort();
    running.dedup();
    running
}

pub(super) fn workspace_panes(state: &App, id: Uuid) -> Vec<gtk::Box> {
    let mut panes = Vec::new();
    if let Some(root) = state
        .workspaces
        .borrow()
        .get(&id)
        .and_then(|ws| ws.layout.first_child())
    {
        collect_panes(&root, &mut panes);
    }
    panes
}

pub(super) fn all_panes(state: &App) -> Vec<gtk::Box> {
    state.panes.borrow().keys().cloned().collect()
}

/// Runs `close` right away, or after asking when programs would be stopped.
pub(super) fn confirm_close(
    state: &App,
    what: &str,
    running: Vec<String>,
    close: impl FnOnce() + 'static,
) {
    if running.is_empty() {
        close();
        return;
    }
    let (them, verb) = if running.len() == 1 {
        ("it", "is")
    } else {
        ("them", "are")
    };
    let dialog = gtk::AlertDialog::builder()
        .message(format!("Close {what}?"))
        .detail(format!(
            "{} {verb} still running. Closing stops {them}.",
            running.join(", ")
        ))
        .buttons(["Cancel", "Close"])
        .cancel_button(0)
        .default_button(0)
        .modal(true)
        .build();
    dialog.choose(Some(&state.window), gio::Cancellable::NONE, move |answer| {
        if matches!(answer, Ok(1)) {
            close();
        }
    });
}

pub(super) fn toolbar_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    button
}

/// A small icon button; `icons` are tried in order, for icons only some
/// themes have (Breeze's split icons).
pub(super) fn header_button(icons: &[&str], tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_child(Some(&gtk::Image::from_gicon(&gio::ThemedIcon::from_names(
        icons,
    ))));
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    button.set_focus_on_click(false);
    button
}

/// The current directory of the workspace's focused terminal, or its first
/// one; the project directory while it has none.
pub(super) fn workspace_cwd(state: &App, ws: &WorkspaceUi) -> PathBuf {
    let pane = ws
        .last_focus
        .upgrade()
        .filter(|pane| pane.is_ancestor(&ws.holder))
        .or_else(|| first_pane(&ws.layout.first_child()?));
    let panes = state.panes.borrow();
    pane.and_then(|pane| panes.get(&pane).cloned())
        .map(|pane| pane_cwd(&pane))
        .unwrap_or_else(|| ws.root_dir.clone())
}

/// Where the workspace's git status and changes come from: a task's
/// worktree, else the folder of its selected terminal (see `workspace_cwd`).
pub(super) fn git_directory(state: &App, ws: &WorkspaceUi) -> PathBuf {
    let worktree = ws
        .task
        .borrow()
        .as_ref()
        .is_some_and(|task| task.branch.is_some());
    if worktree {
        ws.root_dir.clone()
    } else {
        workspace_cwd(state, ws)
    }
}

/// The current directory of the pane's shown tab.
pub(super) fn pane_cwd(pane: &Pane) -> PathBuf {
    pane.current()
        .map(|tab| tab_cwd(&tab))
        .unwrap_or_else(home_dir)
}

/// The shell's current directory, falling back to where it was started.
/// While a program runs (e.g. tmux, whose shells aren't ours), the folder
/// the program reported (OSC 7) comes first.
pub(super) fn tab_cwd(tab: &Tab) -> PathBuf {
    let reported = || {
        tab.terminal.running_program()?;
        tab.terminal.reported_cwd().filter(|dir| dir.is_dir())
    };
    reported()
        .or_else(|| {
            let pid = tab.terminal.pid()?;
            crate::platform::current().process_cwd(pid)
        })
        .unwrap_or_else(|| tab.cwd.clone())
}

pub(super) fn make_paned(
    state: &App,
    orientation: gtk::Orientation,
    ratio: f64,
    start: &impl IsA<gtk::Widget>,
    end: &impl IsA<gtk::Widget>,
) -> gtk::Paned {
    let paned = gtk::Paned::builder()
        .orientation(orientation)
        .start_child(start)
        .end_child(end)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .build();
    // The divider can only be positioned once the split has been given a size.
    let pending = Rc::new(Cell::new(Some(ratio.clamp(0.05, 0.95))));
    let pending_ref = pending.clone();
    paned.connect_max_position_notify(move |paned| {
        let max = paned.max_position();
        if let (Some(ratio), true) = (pending_ref.get(), max > 0) {
            paned.set_position((ratio * max as f64).round() as i32);
            pending_ref.set(None);
        }
    });
    state.ratios.borrow_mut().insert(paned.clone(), pending);
    paned
}

/// Where a widget sits in the split tree, so another widget can take its place.
pub(super) enum Slot {
    Start(gtk::Paned),
    End(gtk::Paned),
    Root(gtk::Box),
}

impl Slot {
    /// Detaches `widget` from its parent and returns the slot it occupied.
    fn take(widget: &gtk::Widget) -> Option<Slot> {
        let parent = widget.parent()?;
        if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
            if paned.start_child().as_ref() == Some(widget) {
                paned.set_start_child(gtk::Widget::NONE);
                Some(Slot::Start(paned.clone()))
            } else {
                paned.set_end_child(gtk::Widget::NONE);
                Some(Slot::End(paned.clone()))
            }
        } else {
            let holder = parent.downcast::<gtk::Box>().ok()?;
            holder.remove(widget);
            Some(Slot::Root(holder))
        }
    }

    fn fill(self, widget: &impl IsA<gtk::Widget>) {
        match self {
            Slot::Start(paned) => paned.set_start_child(Some(widget)),
            Slot::End(paned) => paned.set_end_child(Some(widget)),
            Slot::Root(holder) => holder.append(widget),
        }
    }
}

pub(super) fn split_pane(state: &App, pane: &gtk::Box, orientation: gtk::Orientation) {
    unzoom_around(state, pane);
    let Some(info) = state.panes.borrow().get(pane).cloned() else {
        return;
    };
    let new = new_pane(state, &[pane_cwd(&info)], &[], 0);
    let Some(slot) = Slot::take(pane.upcast_ref()) else {
        return;
    };
    slot.fill(&make_paned(state, orientation, 0.5, pane, &new));
    focus_pane(state, &new);
}

pub(super) fn close_pane(state: &App, pane: &gtk::Box) {
    if state.closing.get() || !state.panes.borrow().contains_key(pane) {
        return;
    }
    unzoom_around(state, pane);
    if let Some(info) = state.panes.borrow_mut().remove(pane) {
        for tab in info.tabs.borrow().iter() {
            remember_closed(state, tab);
        }
    }
    let Some(parent) = pane.parent() else {
        return;
    };
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        // Replace the split with the remaining sibling.
        let sibling = if paned.start_child().as_ref() == Some(pane.upcast_ref()) {
            paned.end_child()
        } else {
            paned.start_child()
        };
        paned.set_start_child(gtk::Widget::NONE);
        paned.set_end_child(gtk::Widget::NONE);
        state.ratios.borrow_mut().remove(paned);
        if let (Some(sibling), Some(slot)) = (sibling, Slot::take(paned.upcast_ref())) {
            slot.fill(&sibling);
            if let Some(next) = first_pane(&sibling) {
                focus_pane(state, &next);
            }
        }
    } else if let Some(holder) = parent.downcast_ref::<gtk::Box>() {
        // Last pane of the workspace: keep the workspace, start a fresh shell.
        let root_dir = workspace_of(state, pane).map(|(_, ws)| ws.root_dir.clone());
        holder.remove(pane);
        let new = new_pane(state, &[root_dir.unwrap_or_else(home_dir)], &[], 0);
        holder.append(&new);
        focus_pane(state, &new);
    }
}

pub(super) fn first_pane(widget: &gtk::Widget) -> Option<gtk::Box> {
    match widget.downcast_ref::<gtk::Paned>() {
        Some(paned) => first_pane(&paned.start_child()?),
        None => widget.downcast_ref::<gtk::Box>().cloned(),
    }
}

pub(super) fn collect_panes(widget: &gtk::Widget, out: &mut Vec<gtk::Box>) {
    match widget.downcast_ref::<gtk::Paned>() {
        Some(paned) => {
            for child in [paned.start_child(), paned.end_child()]
                .into_iter()
                .flatten()
            {
                collect_panes(&child, out);
            }
        }
        None => out.extend(widget.downcast_ref::<gtk::Box>().cloned()),
    }
}

/// Drops bookkeeping for every pane and split below `widget`.
pub(super) fn forget_tree(state: &App, widget: &gtk::Widget) {
    if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
        state.ratios.borrow_mut().remove(paned);
        for child in [paned.start_child(), paned.end_child()]
            .into_iter()
            .flatten()
        {
            forget_tree(state, &child);
        }
    } else if let Some(pane) = widget.downcast_ref::<gtk::Box>() {
        state.panes.borrow_mut().remove(pane);
    }
}

pub(super) fn set_focused(state: &App, pane: &gtk::Box) {
    if let Some(prev) = state.focused.upgrade() {
        prev.remove_css_class("focused");
    }
    pane.add_css_class("focused");
    state.focused.set(Some(pane));
    if let Some((_, ws)) = workspace_of(state, pane) {
        ws.last_focus.set(Some(pane));
    }
}

pub(super) fn focus_pane(state: &App, pane: &gtk::Box) {
    let terminal = state.panes.borrow().get(pane).and_then(|p| p.terminal());
    if let Some(terminal) = terminal {
        terminal.widget().grab_focus();
    }
}

/// The focused pane of the active workspace.
pub(super) fn current_pane(state: &App) -> Option<gtk::Box> {
    let ws = active_workspace(state)?;
    ws.last_focus
        .upgrade()
        .filter(|pane| pane.is_ancestor(&ws.holder))
        .or_else(|| first_pane(&ws.layout.first_child()?))
}

pub(super) fn current_terminal(state: &App) -> Option<TermView> {
    let pane = current_pane(state)?;
    let panes = state.panes.borrow();
    panes.get(&pane)?.terminal()
}

/// Focuses the nearest pane in the given direction.
pub(super) fn move_focus(state: &App, dir: Dir) {
    let (Some(ws), Some(current)) = (active_workspace(state), current_pane(state)) else {
        return;
    };
    let Some(root) = ws.layout.first_child() else {
        return;
    };
    if ws.zoomed.upgrade().is_some() {
        // The hidden panes have no place yet: look once they're laid out.
        unzoom(&ws);
        let weak = Rc::downgrade(state);
        let frames = Cell::new(0);
        ws.holder.add_tick_callback(move |_, _| {
            frames.set(frames.get() + 1);
            if frames.get() < 2 {
                return glib::ControlFlow::Continue;
            }
            if let Some(state) = weak.upgrade() {
                move_focus(&state, dir);
            }
            glib::ControlFlow::Break
        });
        return;
    }
    let mut panes = Vec::new();
    collect_panes(&root, &mut panes);
    let Some(cur) = current.compute_bounds(&ws.holder) else {
        return;
    };
    let center = |r: &graphene::Rect| (r.x() + r.width() / 2.0, r.y() + r.height() / 2.0);
    let (cx, cy) = center(&cur);
    let best = panes
        .into_iter()
        .filter(|pane| pane != &current)
        .filter_map(|pane| {
            let b = pane.compute_bounds(&ws.holder)?;
            let in_direction = match dir {
                Dir::Left => b.x() + b.width() <= cur.x() + 1.0,
                Dir::Right => b.x() >= cur.x() + cur.width() - 1.0,
                Dir::Up => b.y() + b.height() <= cur.y() + 1.0,
                Dir::Down => b.y() >= cur.y() + cur.height() - 1.0,
            };
            let (x, y) = center(&b);
            in_direction.then(|| ((x - cx).powi(2) + (y - cy).powi(2), pane))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, pane)) = best {
        focus_pane(state, &pane);
    }
}

/// Lets a pane fill its workspace, hiding the others, or brings them back.
pub(super) fn zoom_pane(state: &App, pane: &gtk::Box) {
    let Some((_, ws)) = workspace_of(state, pane) else {
        return;
    };
    let was = ws.zoomed.upgrade();
    unzoom(&ws);
    if was.as_ref() == Some(pane) {
        focus_pane(state, pane);
        return;
    }
    // Hide the other side of every split on the way up. The dividers keep
    // their positions, so the layout (and what's saved of it) stays as it was.
    let mut child: gtk::Widget = pane.clone().upcast();
    let mut hid = false;
    while let Some(paned) = child.parent().and_then(|p| p.downcast::<gtk::Paned>().ok()) {
        let other = if paned.start_child().as_ref() == Some(&child) {
            paned.end_child()
        } else {
            paned.start_child()
        };
        if let Some(other) = other {
            other.set_visible(false);
            hid = true;
        }
        child = paned.upcast();
    }
    if hid {
        pane.add_css_class("zoomed");
        ws.zoomed.set(Some(pane));
    }
    focus_pane(state, pane);
}

/// Shows every pane of the workspace again.
pub(super) fn unzoom(ws: &WorkspaceUi) {
    let Some(pane) = ws.zoomed.upgrade() else {
        return;
    };
    ws.zoomed.set(None);
    pane.remove_css_class("zoomed");
    if let Some(root) = ws.layout.first_child() {
        show_all(&root);
    }
}

/// Unzooms the workspace of `pane`, before its layout changes.
pub(super) fn unzoom_around(state: &App, pane: &gtk::Box) {
    if let Some((_, ws)) = workspace_of(state, pane) {
        unzoom(&ws);
    }
}

fn show_all(widget: &gtk::Widget) {
    widget.set_visible(true);
    if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
        for child in [paned.start_child(), paned.end_child()]
            .into_iter()
            .flatten()
        {
            show_all(&child);
        }
    }
}

/// Moves the nearest divider beside the focused pane towards `dir`.
pub(super) fn move_divider(state: &App, dir: Dir) {
    const STEP: f64 = 0.05;
    let Some(pane) = current_pane(state) else {
        return;
    };
    let orientation = match dir {
        Dir::Left | Dir::Right => gtk::Orientation::Horizontal,
        Dir::Up | Dir::Down => gtk::Orientation::Vertical,
    };
    let mut parent = pane.parent();
    while let Some(widget) = parent {
        if let Some(paned) = widget.downcast_ref::<gtk::Paned>()
            && paned.orientation() == orientation
            && paned.max_position() > 0
        {
            let max = paned.max_position() as f64;
            let delta = match dir {
                Dir::Left | Dir::Up => -STEP,
                Dir::Right | Dir::Down => STEP,
            };
            let ratio = (paned.position() as f64 / max + delta).clamp(0.05, 0.95);
            paned.set_position((ratio * max).round() as i32);
            return;
        }
        parent = widget.parent();
    }
}

/// Turns typing into every pane of the active workspace at once on or off.
pub(super) fn toggle_broadcast(state: &App) {
    let Some(ws) = active_workspace(state) else {
        return;
    };
    let on = !ws.broadcast.get();
    ws.broadcast.set(on);
    if on {
        ws.holder.add_css_class("broadcast");
    } else {
        ws.holder.remove_css_class("broadcast");
    }
}

/// Types what was typed into `from` into the shown tab of the workspace's
/// other panes, when that's on.
pub(super) fn mirror_input(state: &App, from: &Tab, input: &Input) {
    let Some((id, ws)) = workspace_of(state, from.terminal.widget()) else {
        return;
    };
    if !ws.broadcast.get() {
        return;
    }
    let terminals: Vec<TermView> = {
        let panes = state.panes.borrow();
        workspace_panes(state, id)
            .iter()
            .filter_map(|pane| panes.get(pane)?.terminal())
            .filter(|terminal| terminal.widget() != from.terminal.widget())
            .collect()
    };
    for terminal in terminals {
        terminal.type_input(input);
    }
}
