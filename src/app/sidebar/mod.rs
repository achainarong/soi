//! The sidebar: its rows, context menu and inline renaming.

use super::*;

mod drag;

pub(super) use drag::*;

pub(super) fn rebuild_sidebar(state: &App) {
    finish_inline_renames(state, true);
    state.rebuilding.set(true);
    state.list.remove_all();
    {
        let tree = state.tree.borrow();
        let workspaces = state.workspaces.borrow();
        let ws_row = |id: &Uuid, group: Option<Uuid>| {
            workspaces
                .get(id)
                .map(|ws| workspace_row(state, *id, ws, group.is_some()))
        };
        for node in tree.iter() {
            match node {
                Node::Workspace(id) => {
                    if let Some(row) = ws_row(id, None) {
                        state.list.append(&row);
                    }
                }
                Node::Group {
                    id,
                    name,
                    collapsed,
                    workspaces,
                    ..
                } => {
                    state.list.append(&group_row(state, *id, name, *collapsed));
                    if !collapsed {
                        for ws in workspaces {
                            if let Some(row) = ws_row(ws, Some(*id)) {
                                state.list.append(&row);
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(row) = state
        .active
        .get()
        .and_then(|id| find_row(state, &id.to_string()))
    {
        state.list.select_row(Some(&row));
    }
    let ids: Vec<Uuid> = state.workspaces.borrow().keys().copied().collect();
    for id in ids {
        update_workspace_progress(state, id);
    }
    state.rebuilding.set(false);
}

pub(super) fn workspace_row(
    state: &App,
    id: Uuid,
    ws: &WorkspaceUi,
    in_group: bool,
) -> gtk::ListBoxRow {
    let name = sidebar_name(
        state,
        Item::Workspace(id),
        ws.name.borrow().as_str(),
        "ws-name",
    );
    let directory = git_directory(state, ws);
    let subtitle = match git_branch(&directory) {
        Some(branch) => format!("{branch} • {}", tilde(&directory)),
        None => tilde(&directory),
    };
    let subtitle = gtk::Label::builder()
        .label(subtitle)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::Middle)
        .build();
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    let weak = Rc::downgrade(state);
    let root = ws.root_dir.clone();
    tasks::watch_git(&subtitle, move || {
        weak.upgrade()
            .and_then(|state| {
                let workspaces = state.workspaces.borrow();
                Some(git_directory(&state, workspaces.get(&id)?))
            })
            .unwrap_or_else(|| root.clone())
    });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_hexpand(true);
    content.append(&name);
    content.append(&subtitle);
    // What its programs report of their progress (see `update_workspace_progress`).
    let progress = gtk::ProgressBar::builder()
        .css_classes(["tab-progress", "ws-progress"])
        .visible(false)
        .build();
    content.append(&progress);
    // Marks a workspace with an agent that needs you.
    let dot = panel::workspace_dot(state, id);
    dot.set_valign(gtk::Align::Start);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    body.set_margin_top(4);
    body.set_margin_bottom(4);
    body.set_margin_start(if in_group { 12 } else { 0 });
    body.append(&content);
    // Marks a task worktree, which "Remove worktree…" can delete. At the end,
    // so a long name shortens instead of hiding it.
    if let Some(branch) = ws
        .task
        .borrow()
        .as_ref()
        .and_then(|task| task.branch.as_ref())
    {
        body.append(
            &gtk::Label::builder()
                .label("⎇")
                .css_classes(["ws-worktree"])
                .tooltip_text(format!("Task worktree on {branch}"))
                .valign(gtk::Align::Start)
                .build(),
        );
    }
    body.append(&dot);
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["ws-close"])
        .tooltip_text("Close workspace")
        .valign(gtk::Align::Center)
        .focus_on_click(false)
        .action_name("win.ws-close")
        .action_target(&id.to_string().to_variant())
        .build();
    body.append(&close);
    let row = gtk::ListBoxRow::builder()
        .child(&body)
        .name(id.to_string())
        .build();

    attach_drag(&row, Item::Workspace(id));
    attach_inline_rename(&row, state, Item::Workspace(id));
    layout_drag::install_workspace_drop(state, &row, id);
    row
}

/// The branch of a workspace soi created as a task worktree.
fn worktree_branch(state: &App, id: Uuid) -> Option<String> {
    let workspaces = state.workspaces.borrow();
    let task = workspaces.get(&id)?.task.borrow();
    task.as_ref().and_then(|task| task.branch.clone())
}

pub(super) fn group_row(state: &App, id: Uuid, name: &str, collapsed: bool) -> gtk::ListBoxRow {
    let arrow = gtk::Button::builder()
        .icon_name(if collapsed {
            "pan-end-symbolic"
        } else {
            "pan-down-symbolic"
        })
        .css_classes(["group-toggle"])
        .tooltip_text(if collapsed {
            "Expand group"
        } else {
            "Collapse group"
        })
        .build();
    let weak = Rc::downgrade(state);
    arrow.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            toggle_group(&state, id);
        }
    });
    let label = sidebar_name(state, Item::Group(id), name, "group-name");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    content.set_margin_top(8);
    content.append(&arrow);
    content.append(&label);
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .css_classes(["group-add"])
        .tooltip_text("New terminal in group")
        .valign(gtk::Align::Center)
        .focus_on_click(false)
        .action_name("win.group-add")
        .action_target(&id.to_string().to_variant())
        .build();
    content.append(&add);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .name(format!("group:{id}"))
        .selectable(false)
        .build();

    attach_inline_rename(&row, state, Item::Group(id));
    attach_drag(&row, Item::Group(id));
    row
}

pub(super) fn group_id_of_row(row: &gtk::ListBoxRow) -> Option<Uuid> {
    Uuid::parse_str(row.widget_name().strip_prefix("group:")?).ok()
}

pub(super) fn find_row(state: &App, name: &str) -> Option<gtk::ListBoxRow> {
    (0..)
        .map_while(|i| state.list.row_at_index(i))
        .find(|row| row.widget_name() == name)
}

pub(super) fn row_name_editor(row: &gtk::ListBoxRow) -> Option<gtk::EditableLabel> {
    panel::find_descendant(row.upcast_ref(), &|widget| {
        widget.is::<gtk::EditableLabel>()
    })
    .and_downcast()
}

/// A detailed action name like `win.ws-rename('<target>')`.
pub(super) fn action(name: &str, target: &str) -> String {
    gio::Action::print_detailed_name(&format!("win.{name}"), Some(&target.to_variant())).into()
}

/// One right-click handler for the whole sidebar, so empty space below the
/// rows gets a menu too. Popovers hang off the sidebar, not a row, because
/// rows are recreated whenever the sidebar is rebuilt.
pub(super) fn install_sidebar_menu(state: &App) {
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let weak = Rc::downgrade(state);
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        open_sidebar_menu(&state, x, y);
    });
    state.sidebar.add_controller(click);
}

/// Opens the context menu at `(x, y)` in sidebar coordinates.
pub(super) fn open_sidebar_menu(state: &App, x: f64, y: f64) -> gtk::PopoverMenu {
    let item = state
        .sidebar
        .compute_point(&state.list, &graphene::Point::new(x as f32, y as f32))
        .and_then(|p| state.list.row_at_y(p.y() as i32))
        .and_then(|row| Item::of_row(&row));
    let popover = gtk::PopoverMenu::from_model(Some(&sidebar_menu(state, item)));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    show_popover(
        &state.sidebar,
        popover.upcast_ref(),
        gdk::Rectangle::new(x as i32, y as i32, 1, 1),
    );
    popover
}

pub(super) fn sidebar_menu(state: &App, item: Option<Item>) -> gio::Menu {
    let specific = gio::Menu::new();
    match item {
        Some(Item::Workspace(id)) => {
            let workspace = id;
            let tree = state.tree.borrow();
            let group = group_of(&tree, id);
            let target = |extra: &str| format!("{id}|{extra}");
            let id = id.to_string();
            specific.append(Some("Rename"), Some(&action("ws-rename", &id)));
            let move_menu = gio::Menu::new();
            for node in tree.iter() {
                if let Node::Group { id: gid, name, .. } = node
                    && Some(*gid) != group
                {
                    move_menu.append(
                        Some(name),
                        Some(&action("ws-move", &target(&gid.to_string()))),
                    );
                }
            }
            if group.is_some() {
                move_menu.append(
                    Some("Remove from group"),
                    Some(&action("ws-move", &target(""))),
                );
            }
            move_menu.append(Some("New group…"), Some(&action("ws-new-group", &id)));
            specific.append_submenu(Some("Move to group"), &move_menu);
            let window_menu = gio::Menu::new();
            for (i, window) in state.shared.windows().iter().enumerate() {
                if !Rc::ptr_eq(window, state) {
                    let label = format!("Window {}", i + 1);
                    window_menu.append(
                        Some(&label),
                        Some(&action("ws-to-window", &format!("{id}|{i}"))),
                    );
                }
            }
            window_menu.append(
                Some("New window"),
                Some(&action("ws-to-window", &format!("{id}|new"))),
            );
            specific.append_submenu(Some("Move to"), &window_menu);
            specific.append(Some("Close workspace"), Some(&action("ws-close", &id)));
            specific.append(Some("Archive"), Some(&action("ws-archive", &id)));
            if worktree_branch(state, workspace).is_some() {
                specific.append(
                    Some("Remove worktree…"),
                    Some(&action("ws-remove-worktree", &id)),
                );
            }
        }
        Some(Item::Group(id)) => {
            let id = id.to_string();
            specific.append(Some("Rename"), Some(&action("group-rename", &id)));
            specific.append(
                Some("New terminal in group"),
                Some(&action("group-add", &id)),
            );
            specific.append(
                Some("Open folder in group…"),
                Some(&action("group-open-folder", &id)),
            );
            specific.append(
                Some("New task worktree in group…"),
                Some(&action("group-new-task", &id)),
            );
            let chosen = group_collection(state, Uuid::parse_str(&id).unwrap_or_default());
            let collection_menu = gio::Menu::new();
            let mark = |name: Option<&str>| {
                if chosen.as_deref() == name {
                    "✓ "
                } else {
                    ""
                }
            };
            collection_menu.append(
                Some(&format!("{}The default collection", mark(None))),
                Some(&action("group-collection", &format!("{id}|"))),
            );
            for collection in crate::collections::load() {
                let name = &collection.name;
                collection_menu.append(
                    Some(&format!("{}{name}", mark(Some(name)))),
                    Some(&action("group-collection", &format!("{id}|{name}"))),
                );
            }
            specific.append_submenu(Some("Collection for new tasks"), &collection_menu);
            specific.append(Some("Ungroup"), Some(&action("group-ungroup", &id)));
            specific.append(
                Some("Close group and its workspaces"),
                Some(&action("group-delete", &id)),
            );
        }
        None => {}
    }
    let general = gio::Menu::new();
    general.append(Some("New terminal"), Some("win.new-workspace"));
    general.append(Some("New window"), Some("win.new-window"));
    general.append(Some("Open folder…"), Some("win.open-folder"));
    general.append(Some("New group"), Some("win.new-group"));

    let menu = gio::Menu::new();
    menu.append_section(None, &specific);
    menu.append_section(None, &general);
    menu
}

pub(super) fn show_popover(
    parent: &impl IsA<gtk::Widget>,
    popover: &gtk::Popover,
    at: gdk::Rectangle,
) {
    popover.set_parent(parent);
    popover.set_pointing_to(Some(&at));
    // GtkStack only removes its own pages during disposal. An animated
    // popover still attached when the window closes can make that loop spin
    // forever. Detach before parent disposal, not just after the close idle.
    let weak_popover = popover.downgrade();
    let unrealize = parent.connect_unrealize(move |_| {
        if let Some(popover) = weak_popover.upgrade() {
            popover.unparent();
        }
    });
    let unrealize = Rc::new(RefCell::new(Some(unrealize)));
    let weak_parent = parent.upcast_ref::<gtk::Widget>().downgrade();
    popover.connect_closed(move |popover| {
        let popover = popover.clone();
        let (parent, unrealize) = (weak_parent.clone(), unrealize.clone());
        glib::idle_add_local_once(move || {
            if let (Some(parent), Some(handler)) = (parent.upgrade(), unrealize.take()) {
                parent.disconnect(handler);
            }
            if popover.parent().is_some() {
                popover.unparent();
            }
        });
    });
    popover.popup();
}

/// Edits the name directly inside the sidebar row named `row_name`.
pub(super) fn start_inline_rename(state: &App, row_name: &str) {
    finish_inline_renames(state, true);
    if let Some(editor) = find_row(state, row_name).as_ref().and_then(row_name_editor) {
        editor.set_can_target(true);
        editor.set_focusable(true);
        editor.set_editable(true);
        editor.start_editing();
        editor.select_region(0, -1);
    }
}

pub(super) fn finish_inline_renames(state: &App, commit: bool) {
    let mut child = state.list.first_child();
    while let Some(widget) = child {
        if let Some(editor) = widget
            .downcast_ref::<gtk::ListBoxRow>()
            .and_then(row_name_editor)
        {
            editor.stop_editing(commit);
        }
        child = widget.next_sibling();
    }
}

pub(super) fn sidebar_name(state: &App, item: Item, text: &str, class: &str) -> gtk::EditableLabel {
    let editor = gtk::EditableLabel::builder()
        .text(text)
        .editable(false)
        .xalign(0.0)
        .hexpand(true)
        .width_chars(1)
        .max_width_chars(24)
        .css_classes([class])
        .tooltip_text("Double-click to rename")
        .build();
    // EditableLabel exposes its label/text stack in the documented widget
    // hierarchy. Keep the same long-name ellipsizing as the original labels.
    if let Some(label) = editor
        .first_child()
        .and_downcast::<gtk::Stack>()
        .and_then(|stack| stack.child_by_name("label"))
        .and_downcast::<gtk::Label>()
    {
        label.set_ellipsize(pango::EllipsizeMode::End);
    }
    // A name lets clicks through to its row (select the workspace, open the
    // sidebar menu on right-click) unless it is being renamed; double-clicks
    // on the row start renaming.
    editor.set_can_target(false);
    editor.set_focusable(false);
    let weak = Rc::downgrade(state);
    editor.connect_editing_notify(move |editor| {
        if editor.is_editing() {
            return;
        }
        editor.set_editable(false);
        editor.set_can_target(false);
        editor.set_focusable(false);
        let Some(state) = weak.upgrade() else { return };
        if state.rebuilding.get() || state.closing.get() {
            return;
        }
        let current = match item {
            Item::Workspace(id) => state
                .workspaces
                .borrow()
                .get(&id)
                .map(|ws| ws.name.borrow().clone()),
            Item::Group(id) => group_name(&state, id),
        };
        let Some(current) = current else { return };
        let proposed = editor.text().trim().to_string();
        let name = if proposed.is_empty() {
            current.clone()
        } else {
            proposed
        };
        editor.set_text(&name);
        if name == current {
            return;
        }
        match item {
            Item::Workspace(id) => {
                if let Some(ws) = state.workspaces.borrow().get(&id) {
                    ws.name.replace(name);
                }
            }
            Item::Group(id) => {
                for node in state.tree.borrow_mut().iter_mut() {
                    if let Node::Group {
                        id: gid,
                        name: saved,
                        ..
                    } = node
                        && *gid == id
                    {
                        *saved = name.clone();
                    }
                }
            }
        }
        update_title(&state);
        save_all(&state.shared);
    });
    // EditableLabel's built-in blur commit refocuses its label. Commit on
    // the next main-loop turn and restore the clicked target instead, so
    // returning to a terminal doesn't lose keyboard focus 100 ms later.
    let focus = gtk::EventControllerFocus::new();
    let weak = editor.downgrade();
    focus.connect_leave(move |controller| {
        let weak = weak.clone();
        let controller = controller.downgrade();
        glib::timeout_add_local_once(std::time::Duration::ZERO, move || {
            let (Some(editor), Some(controller)) = (weak.upgrade(), controller.upgrade()) else {
                return;
            };
            if !editor.is_editing() || controller.contains_focus() {
                return;
            }
            let target = editor.root().and_then(|root| root.focus());
            editor.stop_editing(true);
            if let Some(target) = target {
                target.grab_focus();
            }
        });
    });
    editor.add_controller(focus);
    editor
}

pub(super) fn attach_inline_rename(widget: &impl IsA<gtk::Widget>, state: &App, item: Item) {
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let weak = Rc::downgrade(state);
    click.connect_pressed(move |gesture, presses, x, y| {
        if presses != 2 {
            return;
        }
        // Double-clicking the row's buttons (collapse, add, close) is just
        // clicking them twice.
        if gesture
            .widget()
            .and_then(|w| w.pick(x, y, gtk::PickFlags::DEFAULT))
            .is_some_and(|w| {
                w.is::<gtk::Button>() || w.ancestor(gtk::Button::static_type()).is_some()
            })
        {
            return;
        }
        let Some(state) = weak.upgrade() else { return };
        let row_name = match item {
            Item::Workspace(id) => id.to_string(),
            Item::Group(id) => format!("group:{id}"),
        };
        if find_row(&state, &row_name)
            .as_ref()
            .and_then(row_name_editor)
            .is_some_and(|editor| editor.is_editing())
        {
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        match item {
            Item::Workspace(id) => rename_workspace(&state, id),
            Item::Group(id) => rename_group(&state, id),
        }
    });
    widget.add_controller(click);
}
