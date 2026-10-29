//! Windows: opening and closing them, their names, switching between
//! them, and the saved windows of the window switcher.

use super::*;

/// Opens a window, restoring `session` if given. Without a session (and
/// with `fill`), it starts with one workspace.
pub(super) fn new_window(shared: &Rc<Shared>, session: Option<&WindowSession>, fill: bool) -> App {
    #[expect(
        clippy::expect_used,
        reason = "windows are only made by the running application"
    )]
    let app = gio::Application::default()
        .and_downcast::<gtk::Application>()
        .expect("running inside the GTK application");
    let size = |saved: i32, default: i32| if saved > 0 { saved } else { default };
    let window = gtk::ApplicationWindow::builder()
        .application(&app)
        .title("Soi")
        .default_width(size(session.map_or(0, |s| s.width), 1400))
        .default_height(size(session.map_or(0, |s| s.height), 900))
        .build();
    window.add_css_class("soi");
    keep_outside_size(&window);

    // No custom header bar: the window manager draws the title bar, so it
    // matches the rest of the desktop. Our buttons sit above the sidebar.
    let new_ws = toolbar_button(
        "list-add-symbolic",
        &shortcut_hint(shared, "New workspace", "new-workspace"),
    );
    let new_group_btn = toolbar_button("folder-new-symbolic", "New group");
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    toolbar.add_css_class("soi-toolbar");
    toolbar.append(&new_ws);
    toolbar.append(&new_group_btn);

    let list = gtk::ListBox::new();
    // Selection still happens on one click; group expansion uses its arrow.
    // Reserve double clicks on names for inline editing.
    list.set_activate_on_single_click(false);
    list.add_css_class("navigation-sidebar");
    let sidebar = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .width_request(160)
        .css_classes(["soi-sidebar"])
        .build();
    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    sidebar.set_vexpand(true);
    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.append(&toolbar);
    // A sidebar row rather than a button: the window's name on the left,
    // one arrow on the right.
    let window_label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    let window_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    window_row.append(&gtk::Image::from_icon_name("window-symbolic"));
    window_row.append(&window_label);
    window_row.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
    // Filled by `refresh_window_switchers`.
    let window_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    window_list.set_width_request(260);
    let window_button = gtk::MenuButton::builder()
        .child(&window_row)
        .css_classes(["soi-window-button"])
        .tooltip_text("Windows: switch, open a closed one, rename, delete")
        .popover(
            &gtk::Popover::builder()
                .child(&window_list)
                .has_arrow(false)
                .css_classes(["soi-windows"])
                .build(),
        )
        .build();
    sidebar_box.append(&window_button);
    sidebar_box.append(&sidebar);
    let agents = panel::build(
        session.is_some_and(|s| s.agents_panel),
        session.map_or(0, |s| s.agents_width),
    );
    let content = gtk::Paned::builder()
        .start_child(&stack)
        .end_child(&agents.panel)
        .resize_end_child(false)
        .shrink_end_child(false)
        .build();
    let main = gtk::Paned::builder()
        .start_child(&sidebar_box)
        .end_child(&content)
        .position(size(session.map_or(0, |s| s.sidebar_width), 240))
        .resize_start_child(false)
        .shrink_start_child(false)
        .build();
    window.set_child(Some(&main));
    toolbar.append(&agents.toggle);
    // Filled by `fill_task_menu`, again whenever shortcuts change.
    let task_menu = gio::Menu::new();
    let task_button = gtk::MenuButton::builder()
        .label("Tasks")
        .menu_model(&task_menu)
        .build();
    toolbar.append(&task_button);

    let state: App = Rc::new(State {
        id: Cell::new(session.map_or_else(Uuid::new_v4, |window| window.id)),
        window: window.clone(),
        sidebar,
        list: list.clone(),
        stack,
        tree: RefCell::default(),
        workspaces: RefCell::default(),
        panes: RefCell::default(),
        ratios: RefCell::default(),
        active: Cell::new(None),
        focused: glib::WeakRef::new(),
        rebuilding: Cell::new(false),
        closing: Cell::new(false),
        close_confirmed: Cell::new(false),
        last_used: Cell::new(session.map_or(0, |s| s.last_used)),
        shared: shared.clone(),
        name: RefCell::new(
            session
                .and_then(|s| s.name.clone())
                .unwrap_or_else(|| next_window_name(shared)),
        ),
        window_button,
        window_label,
        task_menu,
        window_list,
        shortcuts: RefCell::default(),
        profile_slot: gtk::Box::new(gtk::Orientation::Horizontal, 0),
        drop_hint: RefCell::default(),
        last_in_group: RefCell::default(),
        agents,
    });
    shared.windows.borrow_mut().push(Rc::downgrade(&state));

    restore(&state, session, fill);
    panel::place(&state);

    let weak = Rc::downgrade(&state);
    list.connect_row_selected(move |_, row| {
        let Some(state) = weak.upgrade() else { return };
        if state.rebuilding.get() {
            return;
        }
        if let Some(id) = row.and_then(|r| Uuid::parse_str(&r.widget_name()).ok()) {
            show_workspace(&state, id);
        }
    });
    let weak = Rc::downgrade(&state);
    list.connect_row_activated(move |_, row| {
        if let (Some(state), Some(id)) = (weak.upgrade(), group_id_of_row(row)) {
            toggle_group(&state, id);
        }
    });
    let weak = Rc::downgrade(&state);
    new_ws.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            new_terminal_workspace(&state, None);
        }
    });
    let weak = Rc::downgrade(&state);
    new_group_btn.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            new_group(&state, Vec::new(), None);
        }
    });

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let settings_btn = toolbar_button(
        "emblem-system-symbolic",
        &shortcut_hint(shared, "Settings", "settings"),
    );
    let weak = Rc::downgrade(&state);
    settings_btn.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            open_settings(&state);
        }
    });
    toolbar.append(&spacer);
    toolbar.append(&state.profile_slot);
    toolbar.append(&settings_btn);
    refresh_profile_ui(&state);
    install_drop_target(&state);
    install_sidebar_menu(&state);
    install_actions(&state);
    track_use(&state);
    install_shortcuts(&state);
    panel::connect(&state);
    tasks::connect(&state);

    // Keep state while open, but break window -> handler -> state -> window
    // on an accepted close, not only when the GObject is finally disposed.
    let owner_slot = RefCell::new(Some(state.clone()));
    window.connect_close_request(move |window| {
        let Some(owner) = owner_slot.borrow().clone() else {
            return glib::Propagation::Proceed;
        };
        finish_inline_renames(&owner, true);
        let shared = owner.shared.clone();
        if !owner.close_confirmed.get() {
            let running = running_in(&owner, &all_panes(&owner));
            if !running.is_empty() {
                let what = if shared.windows().len() <= 1 {
                    "soi"
                } else {
                    "this window"
                };
                let (weak, window) = (Rc::downgrade(&owner), window.downgrade());
                confirm_close(&owner, what, running, move || {
                    if let (Some(owner), Some(window)) = (weak.upgrade(), window.upgrade()) {
                        owner.close_confirmed.set(true);
                        window.close();
                    }
                });
                return glib::Propagation::Stop;
            }
        }
        if shared.windows().len() <= 1 {
            // The last window: soi quits, so remember everything.
            save_all(&shared);
            owner.closing.set(true);
        } else {
            // Closing the native window keeps its latest context available
            // in every other window's switcher.
            let mut context = snapshot(&owner);
            context.closed = true;
            shared.session.borrow_mut().update_window(context);
            shared.session_dirty.set(true);
            owner.closing.set(true);
            save_all(&shared);
            refresh_window_switchers(&shared);
        }
        release_terminals(&owner);
        owner_slot.take();
        glib::Propagation::Proceed
    });

    window.present();
    save_all(shared);
    refresh_window_switchers(shared);
    panel::refresh(shared);
    state
}

/// The Tasks menu's items: label, window action and keybinding id.
const TASK_ITEMS: [(&str, &str, &str); 6] = [
    ("New task worktree…", "win.task-new", "task-new"),
    ("Task details…", "win.task-context", "task-details"),
    ("Session history…", "win.task-history", "task-history"),
    ("Archived workspaces…", "win.task-archive", "task-archive"),
    ("Inspect changes…", "win.task-changes", "task-changes"),
    ("Toggle plan preview", "win.task-preview", "toggle-plan"),
];

/// Fills the Tasks menu, each item showing its current shortcut.
pub(super) fn fill_task_menu(state: &App) {
    let keys = state.shared.keybindings.borrow();
    state.task_menu.remove_all();
    for (label, action, id) in TASK_ITEMS {
        let item = gio::MenuItem::new(Some(label), Some(action));
        // The menu has room for one key; the first stands for the rest.
        if let Some(accel) = keybindings::accels(keys.get(id)).next() {
            item.set_attribute_value("accel", Some(&accel.to_variant()));
        }
        state.task_menu.append_item(&item);
    }
}

/// Ends the window's terminals once its context is saved, so a closed
/// window cannot leave hidden shells running; their output stays saved for
/// when the context comes back.
pub(super) fn release_terminals(state: &App) {
    save_output(&state.shared, std::slice::from_ref(state));
    state.panes.borrow_mut().clear();
    state.workspaces.borrow_mut().clear();
    state.ratios.borrow_mut().clear();
    // Only the workspace pages: a popover attached to the stack (e.g.
    // the quick switcher) isn't one, and removing it would do nothing.
    let mut child = state.stack.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if !widget.is::<gtk::Popover>() {
            state.stack.remove(&widget);
        }
    }
}

/// The window a widget is in.
pub(super) fn window_of(shared: &Shared, widget: &impl IsA<gtk::Widget>) -> Option<App> {
    let root = widget.root()?;
    shared
        .windows()
        .into_iter()
        .find(|state| state.window.upcast_ref::<gtk::Widget>() == root.upcast_ref::<gtk::Widget>())
}

pub(super) fn update_title(state: &App) {
    let workspace = active_workspace(state).map(|ws| ws.name.borrow().clone());
    let window = state.name.borrow();
    state.window.set_title(Some(&match workspace {
        Some(workspace) => format!("{workspace} — {window} — Soi"),
        None => format!("{window} — Soi"),
    }));
}

/// "Window N" with the lowest N not in use.
#[expect(
    clippy::expect_used,
    reason = "an endless range always finds a free name"
)]
pub(super) fn next_window_name(shared: &Shared) -> String {
    let taken: Vec<String> = shared
        .windows()
        .iter()
        .map(|w| w.name.borrow().clone())
        .chain(
            shared
                .session
                .borrow()
                .windows
                .iter()
                .filter_map(|w| w.name.clone()),
        )
        .collect();
    (1..)
        .map(|n| format!("Window {n}"))
        .find(|name| !taken.contains(name))
        .expect("unbounded")
}

/// Updates every window's switcher: its name and the list of windows.
pub(super) fn refresh_window_switchers(shared: &Shared) {
    let windows = shared.windows();
    for window in &windows {
        window.window_label.set_text(&window.name.borrow());
        fill_switcher(window, &windows, &shared.session.borrow().windows);
        update_title(window);
    }
}

/// The window switcher: the open windows, then the closed ones, each a row
/// to click, with what can be done to it on the row itself.
fn fill_switcher(state: &App, windows: &[App], contexts: &[WindowSession]) {
    let list_box = &state.window_list;
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    let popdown = {
        let button = state.window_button.downgrade();
        move || {
            if let Some(button) = button.upgrade() {
                button.popdown();
            }
        }
    };
    let section = |title: &str| {
        let label = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["soi-windows-title"])
            .build();
        list_box.append(&label);
    };
    let list = || {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.connect_row_activated({
            let (popdown, shared) = (popdown.clone(), Rc::downgrade(&state.shared));
            move |_, row| {
                popdown();
                if let (Some(shared), Ok(id)) =
                    (shared.upgrade(), Uuid::parse_str(&row.widget_name()))
                {
                    open_window_context(&shared, id);
                }
            }
        });
        list_box.append(&list);
        list
    };
    let flat = |icon: &str, tooltip: &str| {
        gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tooltip)
            .css_classes(["flat", "soi-windows-action"])
            .valign(gtk::Align::Center)
            .build()
    };

    section("Open windows");
    let open = list();
    for window in windows {
        let current = Rc::ptr_eq(window, state);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Every row has the mark's room, so the names line up.
        let mark = gtk::Image::from_icon_name("object-select-symbolic");
        mark.set_opacity(if current { 1.0 } else { 0.0 });
        body.append(&mark);
        body.append(
            &gtk::Label::builder()
                .label(window.name.borrow().as_str())
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build(),
        );
        if current {
            let rename = flat("document-edit-symbolic", "Rename this window");
            rename.set_action_name(Some("win.rename-window"));
            rename.connect_clicked({
                let popdown = popdown.clone();
                move |_| popdown()
            });
            body.append(&rename);
        }
        let close = flat(
            "window-close-symbolic",
            "Close (it stays under Closed windows)",
        );
        close.connect_clicked({
            let (popdown, target) = (popdown.clone(), window.window.downgrade());
            move |_| {
                popdown();
                if let Some(target) = target.upgrade() {
                    target.close();
                }
            }
        });
        body.append(&close);
        open.append(
            &gtk::ListBoxRow::builder()
                .child(&body)
                .name(window.id.get().to_string())
                .tooltip_text(if current {
                    "This window"
                } else {
                    "Go to this window"
                })
                .build(),
        );
    }

    let closed: Vec<&WindowSession> = contexts.iter().filter(|c| c.closed).collect();
    if !closed.is_empty() {
        section("Closed windows");
        let saved = list();
        for context in closed {
            let id = context.id.to_string();
            let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let mark = gtk::Image::from_icon_name("object-select-symbolic");
            mark.set_opacity(0.0);
            body.append(&mark);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            text.set_hexpand(true);
            text.append(
                &gtk::Label::builder()
                    .label(context.name.as_deref().unwrap_or("Saved window"))
                    .xalign(0.0)
                    .ellipsize(pango::EllipsizeMode::End)
                    .build(),
            );
            text.append(
                &gtk::Label::builder()
                    .label(workspace_count(context))
                    .xalign(0.0)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
            body.append(&text);
            // Deleting takes a second click, on the button it turns into.
            let delete = flat(
                "user-trash-symbolic",
                "Delete this window and its workspaces",
            );
            delete.connect_clicked(move |button| {
                if button.has_css_class("destructive-action") {
                    button
                        .activate_action("win.delete-window", Some(&id.to_variant()))
                        .ok();
                    return;
                }
                button.set_icon_name("");
                button.set_label("Delete");
                button.remove_css_class("flat");
                button.add_css_class("destructive-action");
                button.set_tooltip_text(Some("Click again to delete for good"));
            });
            body.append(&delete);
            saved.append(
                &gtk::ListBoxRow::builder()
                    .child(&body)
                    .name(context.id.to_string())
                    .tooltip_text("Open in a new window")
                    .build(),
            );
        }
    }

    list_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let new = gtk::Button::builder()
        .action_name("win.new-window")
        .css_classes(["flat"])
        .child(&{
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
            content.append(&gtk::Label::new(Some("New window")));
            content
        })
        .build();
    new.connect_clicked(move |_| popdown());
    list_box.append(&new);
}

/// How many workspaces a saved window has, e.g. "3 workspaces".
pub(super) fn workspace_count(context: &WindowSession) -> String {
    let count: usize = context
        .sidebar
        .iter()
        .map(|entry| match entry {
            Entry::Workspace(_) => 1,
            Entry::Group(group) => group.workspaces.len(),
        })
        .sum();
    match count {
        1 => "1 workspace".into(),
        n => format!("{n} workspaces"),
    }
}

/// The open window showing context `id`.
pub(super) fn window_with(shared: &Shared, id: Uuid) -> Option<App> {
    shared
        .windows()
        .into_iter()
        .find(|window| window.id.get() == id)
}

/// A stable target either focuses its live window or restores the same saved
/// context. Repeated activation must not create duplicate contexts/windows.
pub(super) fn open_window_context(shared: &Rc<Shared>, id: Uuid) {
    if let Some(window) = window_with(shared, id) {
        window.window.present();
        return;
    }
    let context = shared
        .session
        .borrow()
        .windows
        .iter()
        .find(|window| window.id == id)
        .cloned();
    if let Some(context) = context {
        let window = new_window(shared, Some(&context), true);
        mark_used(&window);
    }
}

/// Shows the saved context `id` in this window, saving the one it showed
/// like a closed window. Asks first when that stops programs. A context
/// open in another window gets focused there instead.
pub(super) fn swap_context(state: &App, id: Uuid) {
    if let Some(window) = window_with(&state.shared, id) {
        window.window.present();
        return;
    }
    let weak = Rc::downgrade(state);
    let running = running_in(state, &all_panes(state));
    confirm_close(state, "this window's terminals", running, move || {
        if let Some(state) = weak.upgrade() {
            replace_context(&state, id);
        }
    });
}

pub(super) fn replace_context(state: &App, id: Uuid) {
    let shared = &state.shared;
    let Some(context) = shared
        .session
        .borrow()
        .windows
        .iter()
        .find(|window| window.id == id && window.closed)
        .cloned()
    else {
        return;
    };
    finish_inline_renames(state, true);
    let mut old = snapshot(state);
    old.closed = true;
    shared.session.borrow_mut().update_window(old);
    shared.session_dirty.set(true);
    release_terminals(state);
    state.tree.borrow_mut().clear();
    state.last_in_group.borrow_mut().clear();
    state.active.set(None);

    state.id.set(id);
    state.name.replace(
        context
            .name
            .clone()
            .unwrap_or_else(|| next_window_name(shared)),
    );
    if let Some(open) = state
        .window
        .lookup_action("open-window")
        .and_downcast::<gio::SimpleAction>()
    {
        open.set_state(&id.to_string().to_variant());
    }
    panel::show_saved(state, context.agents_panel, context.agents_width);
    restore(state, Some(&context), true);
    mark_used(state);
    save_all(shared);
    refresh_window_switchers(shared);
    panel::refresh(shared);
}

/// Deletes a saved (closed) window context, or all of them with `None`,
/// together with its tabs' saved output.
pub(super) fn delete_saved_windows(shared: &Shared, id: Option<Uuid>) {
    if !shared.session.borrow_mut().remove_closed(id) {
        return;
    }
    shared.session_dirty.set(true);
    save_all(shared);
    save_output(shared, &[]);
    refresh_window_switchers(shared);
}

/// Asks for a new name for the window, next to its switcher.
pub(super) fn rename_window(state: &App) {
    let entry = gtk::Entry::builder()
        .text(state.name.borrow().as_str())
        .width_chars(24)
        .build();
    let popover = gtk::Popover::builder().child(&entry).build();
    popover.set_parent(&state.window_button);
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    let weak = Rc::downgrade(state);
    let popover_ref = popover.downgrade();
    entry.connect_activate(move |entry| {
        let name = entry.text().trim().to_string();
        if let (Some(state), false) = (weak.upgrade(), name.is_empty()) {
            state.name.replace(name);
            refresh_window_switchers(&state.shared);
            save_all(&state.shared);
        }
        if let Some(popover) = popover_ref.upgrade() {
            popover.popdown();
        }
    });
    popover.popup();
    entry.grab_focus();
}

/// Brings the next (`delta` 1) or previous (-1) window to the front.
/// Maximizes the window, the way its title bar button does: as big as the
/// screen allows, but still a window, unlike fullscreen.
pub(super) fn toggle_maximized(state: &App) {
    if state.window.is_maximized() {
        state.window.unmaximize();
    } else {
        state.window.maximize();
    }
}

/// Puts the window on half of its screen, out of maximized first.
pub(super) fn snap_window(state: &App, snap: Snap) {
    if state.window.is_maximized() {
        state.window.unmaximize();
    }
    crate::platform::current().snap_window(state.window.upcast_ref(), snap);
}

/// Keeps a size given from outside GTK, by a window tool such as Rectangle
/// or by `snap_window`. Asked what size the window wants, GTK answers with
/// the one it last chose and so puts the old size back; answer with the
/// size it has now instead. Only where the surface is just the window,
/// without client-side shadows around it.
fn keep_outside_size(window: &gtk::ApplicationWindow) {
    if !crate::platform::current().surface_is_window() {
        return;
    }
    // After GTK's own realize, so that this answers after GTK does.
    window.connect_realize(|window| {
        let Some(toplevel) = window.surface().and_downcast::<gdk::Toplevel>() else {
            return;
        };
        let window = window.downgrade();
        toplevel.connect_compute_size(move |toplevel, size| {
            // Before it's shown, the window gets the size GTK chooses.
            if !window.upgrade().is_some_and(|window| window.is_mapped()) {
                return;
            }
            let surface = toplevel.upcast_ref::<gdk::Surface>();
            let (width, height) = (surface.width(), surface.height());
            let placed = gdk::ToplevelState::MAXIMIZED | gdk::ToplevelState::FULLSCREEN;
            if width > 1 && height > 1 && !toplevel.state().intersects(placed) {
                size.set_size(width, height);
            }
        });
    });
}

pub(super) fn cycle_window(state: &App, delta: isize) {
    let windows = state.shared.windows();
    let Some(here) = windows.iter().position(|w| Rc::ptr_eq(w, state)) else {
        return;
    };
    let next = (here as isize + delta).rem_euclid(windows.len() as isize) as usize;
    windows[next].window.present();
}
