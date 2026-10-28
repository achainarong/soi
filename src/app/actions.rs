//! The window's actions and the keyboard shortcuts that run them.

use super::*;

pub(super) fn install_actions(state: &App) {
    let add = |name: &str, f: fn(&App, &str)| {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(state);
        action.connect_activate(move |_, param| {
            if let (Some(state), Some(param)) =
                (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            {
                f(&state, &param);
            }
        });
        state.window.add_action(&action);
    };

    add("ws-rename", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            rename_workspace(s, id)
        }
    });
    add("ws-close", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            let running = running_in(s, &workspace_panes(s, id));
            let weak = Rc::downgrade(s);
            let close = move || {
                if let Some(s) = weak.upgrade() {
                    close_workspace(&s, id);
                }
            };
            if !running.is_empty() {
                confirm_close(s, "this workspace", running, close);
                return;
            }
            // Asks even when nothing runs: the sidebar's close button is easy to hit.
            let name = s
                .workspaces
                .borrow()
                .get(&id)
                .map(|ws| ws.name.borrow().clone())
                .unwrap_or_default();
            let dialog = gtk::AlertDialog::builder()
                .message(format!("Close workspace “{name}”?"))
                .detail("You can reopen it afterwards.")
                .buttons(["Cancel", "Close"])
                .cancel_button(0)
                .default_button(1)
                .modal(true)
                .build();
            dialog.choose(Some(&s.window), gio::Cancellable::NONE, move |answer| {
                if matches!(answer, Ok(1)) {
                    close();
                }
            });
        }
    });
    add("ws-archive", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            let running = running_in(s, &workspace_panes(s, id));
            let weak = Rc::downgrade(s);
            let archive = move || {
                if let Some(s) = weak.upgrade() {
                    archive_workspace(&s, id);
                }
            };
            if running.is_empty() {
                archive();
            } else {
                confirm_close(s, "this workspace", running, archive);
            }
        }
    });
    add("ws-remove-worktree", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            tasks::remove_worktree(s, id);
        }
    });
    add("ws-move", |s, p| {
        let (ws, group) = p.split_once('|').unwrap_or((p, ""));
        if let Ok(ws) = Uuid::parse_str(ws) {
            move_to_group(s, ws, Uuid::parse_str(group).ok());
        }
    });
    add("ws-new-group", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            let at = detach(&mut s.tree.borrow_mut(), id);
            new_group(s, vec![id], at);
        }
    });
    add("group-rename", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            rename_group(s, id)
        }
    });
    add("group-add", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            new_terminal_workspace(s, Some(id))
        }
    });
    add("group-new-task", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            tasks::create(s, Some(id))
        }
    });
    add("group-collection", |s, p| {
        if let Some((id, name)) = p.split_once('|')
            && let Ok(id) = Uuid::parse_str(id)
        {
            set_group_collection(s, id, Some(name.to_string()).filter(|n| !n.is_empty()))
        }
    });
    add("group-open-folder", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            pick_project(s, Some(id))
        }
    });
    add("group-ungroup", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            ungroup(s, id)
        }
    });
    add("group-delete", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            let members = s.tree.borrow().iter().find_map(|node| match node {
                Node::Group {
                    id: gid,
                    workspaces,
                    ..
                } if *gid == id => Some(workspaces.clone()),
                _ => None,
            });
            let panes: Vec<gtk::Box> = members
                .unwrap_or_default()
                .into_iter()
                .flat_map(|ws| workspace_panes(s, ws))
                .collect();
            let running = running_in(s, &panes);
            let weak = Rc::downgrade(s);
            confirm_close(s, "this group and its workspaces", running, move || {
                if let Some(s) = weak.upgrade() {
                    delete_group(&s, id);
                }
            });
        }
    });

    let add_plain = |name: &str, f: fn(&App)| {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(state);
        action.connect_activate(move |_, _| {
            if let Some(state) = weak.upgrade() {
                f(&state);
            }
        });
        state.window.add_action(&action);
    };
    add_plain("new-workspace", |s| new_terminal_workspace(s, None));
    add_plain("new-window", |s| {
        new_window(&s.shared, None, true);
    });
    add_plain("rename-window", rename_window);
    let open = gio::SimpleAction::new_stateful(
        "open-window",
        Some(glib::VariantTy::STRING),
        &state.id.get().to_string().to_variant(),
    );
    let weak = Rc::downgrade(state);
    open.connect_activate(move |_, target| {
        if let (Some(state), Some(id)) = (
            weak.upgrade(),
            target
                .and_then(|p| p.get::<String>())
                .and_then(|id| Uuid::parse_str(&id).ok()),
        ) {
            open_window_context(&state.shared, id);
        }
    });
    state.window.add_action(&open);
    add("swap-window", |s, p| {
        if let Ok(id) = Uuid::parse_str(p) {
            swap_context(s, id);
        }
    });
    add("ws-to-window", |s, p| {
        let (ws, target) = p.split_once('|').unwrap_or((p, ""));
        if let Ok(ws) = Uuid::parse_str(ws) {
            move_to_window(s, ws, target.parse().ok());
        }
    });
    add("delete-window", |s, p| match p {
        "all" => delete_saved_windows(&s.shared, None),
        id => {
            if let Ok(id) = Uuid::parse_str(id) {
                delete_saved_windows(&s.shared, Some(id));
            }
        }
    });
    add_plain("open-folder", |s| pick_project(s, None));
    add_plain("new-group", |s| new_group(s, Vec::new(), None));
}

/// Notes when the window is typed in, to open it again on start (see
/// `Session::windows_to_open`). Only typing counts: a window that just got
/// the focus because another one closed wasn't used.
pub(super) fn track_use(state: &App) {
    let keys = gtk::EventControllerKey::new();
    // Before the shortcuts, which may stop the key.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(state);
    keys.connect_key_pressed(move |_, _, _, _| {
        // Coarse, so typing doesn't change the session all the time.
        if let Some(state) = weak.upgrade()
            && now() >= state.last_used.get() + 10
        {
            mark_used(&state);
        }
        glib::Propagation::Proceed
    });
    state.window.add_controller(keys);
}

/// Notes that the window is used now (see `track_use`).
pub(super) fn mark_used(state: &App) {
    state.last_used.set(now());
}

/// Seconds since 1970.
pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Builds the window's keyboard shortcuts from the user's keybindings.
pub(super) fn install_shortcuts(state: &App) {
    if let Some(old) = state.shortcuts.take() {
        state.window.remove_controller(&old);
    }
    let controller = gtk::ShortcutController::new();
    // Capture phase: handle our keys before the terminal sees them.
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let keys = state.shared.keybindings.borrow();
    for binding in BINDINGS {
        let Some((key, mods)) = parse_accel(keys.get(binding.id)) else {
            continue;
        };
        let mut keyvals = vec![key];
        // The same key on the number pad, and "=" for "+" (no Shift needed
        // on US keyboards).
        keyvals.extend(match key {
            gdk::Key::plus => &[gdk::Key::equal, gdk::Key::KP_Add][..],
            gdk::Key::minus => &[gdk::Key::KP_Subtract][..],
            gdk::Key::_0 => &[gdk::Key::KP_0][..],
            _ => &[][..],
        });
        // Shift+Tab arrives as ISO_Left_Tab.
        if key == gdk::Key::Tab && mods.contains(gdk::ModifierType::SHIFT_MASK) {
            keyvals.push(gdk::Key::ISO_Left_Tab);
        }
        for keyval in keyvals {
            let weak = Rc::downgrade(state);
            let id = binding.id;
            let edits = edits_text(keyval, mods);
            let action = gtk::CallbackAction::new(move |window, _| {
                if edits && editing_text(window) {
                    return glib::Propagation::Proceed;
                }
                if let Some(state) = weak.upgrade() {
                    run_action(&state, id);
                }
                glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(
                Some(gtk::KeyvalTrigger::new(keyval, mods)),
                Some(action),
            ));
        }
    }
    state.window.add_controller(controller.clone());
    state.shortcuts.replace(Some(controller));
}

/// Applies changed keybindings to every window.
pub(super) fn reload_shortcuts(shared: &Shared) {
    for window in shared.windows() {
        install_shortcuts(&window);
    }
}

/// Runs a bindable action (see `keybindings::BINDINGS`).
pub(super) fn run_action(state: &App, id: &str) {
    let pane = current_pane(state);
    let terminal = current_terminal(state);
    match id {
        "new-tab" => {
            if let Some(pane) = pane {
                new_tab(state, &pane, None);
            }
        }
        "close-tab" => {
            if let (Some(workspace), Some(ws)) = (state.active.get(), active_workspace(state)) {
                request_close_main_tab(state, workspace, ws.current_tab().id);
            }
        }
        "close-pane" => {
            if let Some(pane) = pane {
                let running = running_in(state, std::slice::from_ref(&pane));
                let weak = Rc::downgrade(state);
                confirm_close(state, "this pane", running, move || {
                    if let Some(state) = weak.upgrade() {
                        close_pane(&state, &pane);
                    }
                });
            }
        }
        "reopen-tab" => reopen_tab(state),
        "next-tab" => cycle_main_tab(state, 1),
        "previous-tab" => cycle_main_tab(state, -1),
        "next-pane" => cycle_pane(state, 1),
        "previous-pane" => cycle_pane(state, -1),
        "split-right" | "split-down" => {
            if let Some(pane) = pane {
                let orientation = if id == "split-right" {
                    gtk::Orientation::Horizontal
                } else {
                    gtk::Orientation::Vertical
                };
                split_pane(state, &pane, orientation);
            }
        }
        "focus-left" => move_focus(state, Dir::Left),
        "focus-right" => move_focus(state, Dir::Right),
        "focus-up" => move_focus(state, Dir::Up),
        "focus-down" => move_focus(state, Dir::Down),
        "zoom-pane" => {
            if let Some(pane) = pane {
                zoom_pane(state, &pane);
            }
        }
        "broadcast-input" => toggle_broadcast(state),
        "grow-left" => move_divider(state, Dir::Left),
        "grow-right" => move_divider(state, Dir::Right),
        "grow-up" => move_divider(state, Dir::Up),
        "grow-down" => move_divider(state, Dir::Down),
        "new-workspace" => new_terminal_workspace(state, None),
        "rename-workspace" => {
            if let Some(ws) = state.active.get() {
                rename_workspace(state, ws);
            }
        }
        "next-workspace" => cycle_workspace(state, 1),
        "previous-workspace" => cycle_workspace(state, -1),
        "next-group" => cycle_group(state, 1),
        "previous-group" => cycle_group(state, -1),
        "new-window" => {
            new_window(&state.shared, None, true);
        }
        "next-window" => cycle_window(state, 1),
        "previous-window" => cycle_window(state, -1),
        "maximize-window" => toggle_maximized(state),
        "snap-left" => snap_window(state, Snap::Left),
        "snap-right" => snap_window(state, Snap::Right),
        "snap-top" => snap_window(state, Snap::Top),
        "snap-bottom" => snap_window(state, Snap::Bottom),
        "copy" => terminal.iter().for_each(TermView::copy_clipboard),
        "cut" => terminal.iter().for_each(TermView::cut_clipboard),
        "paste" => terminal.iter().for_each(TermView::paste_clipboard),
        "copy-last-output" => {
            terminal.iter().for_each(|t| {
                t.copy_last_output();
            });
        }
        "search" => terminal.iter().for_each(|t| t.browse(true)),
        "browse" => terminal.iter().for_each(|t| t.browse(false)),
        "settings" => open_settings(state),
        "quick-switch" => switcher::open(state),
        "toggle-agents" => panel::toggle(state),
        "next-agent" => panel::next_agent(state),
        "task-details" => task_view::open(state),
        "toggle-plan" => preview::toggle(state),
        "zoom-in" => zoom(state, Some(1.1)),
        "zoom-out" => zoom(state, Some(1.0 / 1.1)),
        "zoom-reset" => zoom(state, None),
        "quit" => quit(&state.shared),
        _ => {
            if let Some(n) = id
                .strip_prefix("tab-")
                .and_then(|n| n.parse::<usize>().ok())
            {
                select_main_tab(state, n);
                return;
            }
            let n = id
                .strip_prefix("workspace-")
                .and_then(|n| n.parse::<usize>().ok());
            if let Some(n) = n {
                select_in_group(state, n);
            }
        }
    }
}

/// Parses an accelerator; `None` for "" (turned off) or nonsense. (GTK
/// itself accepts "" as a key with no name.)
/// Whether text fields use the key to move or select, or to redo: Shift+Ctrl+
/// Left selects a word, Shift+Cmd+Left to the start of the line. Shortcuts on
/// these keys leave them to the text while it's being edited.
fn edits_text(key: gdk::Key, mods: gdk::ModifierType) -> bool {
    use gdk::ModifierType as M;
    let without_shift = mods - M::SHIFT_MASK;
    match key {
        gdk::Key::Left
        | gdk::Key::Right
        | gdk::Key::Up
        | gdk::Key::Down
        | gdk::Key::Home
        | gdk::Key::End => {
            [M::empty(), M::CONTROL_MASK, M::ALT_MASK, M::META_MASK].contains(&without_shift)
        }
        gdk::Key::z | gdk::Key::Z => {
            mods == M::SHIFT_MASK | M::CONTROL_MASK || mods == M::SHIFT_MASK | M::META_MASK
        }
        _ => false,
    }
}

/// Whether the window's focus is in text that can be edited.
fn editing_text(window: &gtk::Widget) -> bool {
    let focus = window.root().and_then(|root| root.focus());
    match focus {
        Some(widget) => {
            widget
                .downcast_ref::<gtk::TextView>()
                .is_some_and(|view| view.is_editable())
                || widget
                    .downcast_ref::<gtk::Text>()
                    .is_some_and(|text| text.is_editable())
        }
        None => false,
    }
}

pub(super) fn parse_accel(accel: &str) -> Option<(gdk::Key, gdk::ModifierType)> {
    gtk::accelerator_parse(accel).filter(|(key, _)| key.name().is_some())
}

/// How an accelerator is shown: "Shift+Ctrl+T", or "Off".
pub(super) fn accel_label(accel: &str) -> String {
    match parse_accel(accel) {
        Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
        None => "Off".into(),
    }
}

/// A tooltip with the action's current shortcut, e.g. "New tab (Shift+Ctrl+T)".
pub(super) fn shortcut_hint(shared: &Shared, label: &str, id: &str) -> String {
    let keys = shared.keybindings.borrow();
    match keys.get(id) {
        "" => label.to_string(),
        accel => format!("{label} ({})", accel_label(accel)),
    }
}

/// Whether two accelerators mean the same keys.
pub(super) fn same_accel(a: &str, b: &str) -> bool {
    let normalize = |accel: &str| parse_accel(accel).map(|(key, mods)| (key.to_lower(), mods));
    normalize(a).is_some() && normalize(a) == normalize(b)
}

/// Binds `accel` ("" = off) to an action, taking it away from any other.
pub(super) fn rebind(state: &App, id: &str, accel: &str) {
    let shared = &state.shared;
    let note = {
        let mut keys = shared.keybindings.borrow_mut();
        let note = if accel.is_empty() {
            None
        } else {
            keys.conflict(id, accel, same_accel).map(|other| {
                keys.set(other.id, "");
                format!(
                    "{} was used by \"{}\"; that shortcut is now off.",
                    accel_label(accel),
                    other.label
                )
            })
        };
        keys.set(id, accel);
        if let Err(err) = keys.save() {
            crate::log::write(&format!("failed to save shortcuts: {err}"));
        }
        note
    };
    shared.shortcut_note.replace(note);
    reload_shortcuts(shared);
    refresh_profile_ui(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_that_move_or_select_in_text_are_left_to_it() {
        use gdk::Key as K;
        use gdk::ModifierType as M;
        let (shift, control, alt, meta) =
            (M::SHIFT_MASK, M::CONTROL_MASK, M::ALT_MASK, M::META_MASK);
        for (key, mods) in [
            (K::Left, shift | control),
            (K::Down, shift | control),
            (K::Right, shift | meta),
            (K::Up, meta),
            (K::Left, shift | alt),
            (K::z, shift | control),
            (K::z, shift | meta),
        ] {
            assert!(edits_text(key, mods), "{key:?} {mods:?}");
        }
        for (key, mods) in [
            (K::Left, alt | meta),
            (K::Down, control | meta),
            (K::Up, shift | control | alt),
            (K::t, shift | control),
            (K::Page_Down, control),
        ] {
            assert!(!edits_text(key, mods), "{key:?} {mods:?}");
        }
    }
}
