//! Real window/session regressions: `make test-gui`.

use super::*;

macro_rules! checks {
    ($($name:ident),* $(,)?) => {
        pub(super) const CHECKS: &[(&str, fn())] = &[
            $((stringify!($name), $name)),*
        ];
    };
}

checks![
    switcher_lists_open_and_closed_windows_aligned,
    sidebar_names_rename_inline_and_support_commit_cancel_and_empty_input,
    inline_rename_commits_on_focus_loss_and_group_arrows_still_toggle,
    new_windows_are_saved_immediately,
    closing_a_window_retains_its_context,
    saved_windows_open_in_place,
    starting_again_brings_back_a_saved_window,
    saved_windows_can_be_deleted,
    context_live_updates_groups_splits_tabs_and_directories,
    restart_restores_open_windows_and_retains_closed_contexts,
    numbers_count_within_the_group_and_groups_switch,
    windows_maximize_and_zoom_is_kept,
    tabs_move_between_panes_by_drag_and_drop,
    dropped_tabs_move_only_once_the_drag_is_over,
    quick_switcher_goes_to_the_workspace_typed,
    terminal_output_comes_back_after_a_restart,
    context_tracks_shell_directory_changes,
    agents_report_states_and_alert_until_seen,
    zoomed_panes_fill_the_workspace_until_the_layout_changes,
    closed_tabs_reopen_with_their_folder_and_output,
    typing_goes_to_every_pane_while_broadcasting,
    tabs_show_the_progress_programs_report,
    screen_readers_get_the_lines_and_the_cursor,
    task_context_preview_and_history_work_in_real_windows,
    agent_navigation_keeps_sidebar_geometry_and_terminal_zoom,
    bash_marks_its_commands,
    closed_workspaces_reopen_and_closed_things_survive_a_restart,
    the_plan_beside_the_terminal_is_a_live_markdown_editor,
    agents_to_start_can_be_added_edited_and_removed,
    agents_started_directly_show_in_the_agents_panel,
    agents_started_through_a_wrapper_count_as_the_agent,
];

// Run actual GTK window/session lifecycles in a child with its own home
// and state directory, so tests cannot overwrite the user's session.
pub(super) fn isolated(name: &str, test: impl FnOnce()) {
    if std::env::var_os("SOI_WINDOW_TEST_CHILD").is_some() {
        test();
        return;
    }
    // Keep Unix socket paths short on macOS (sockaddr_un has 104 bytes).
    let root = std::env::temp_dir().join(format!(
        "soi-ui-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    fs::create_dir(&root).unwrap();
    // libproc returns physical paths: /var is /private/var on macOS.
    let root = root.canonicalize().unwrap();
    fs::create_dir(root.join("run")).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name])
        .env("SOI_WINDOW_TEST_CHILD", "1")
        .env("SOI_DEV", "1")
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        // macOS uses TMPDIR for the agent socket, not XDG_RUNTIME_DIR.
        .env("TMPDIR", root.join("run"))
        .env("SHELL", "/bin/sh")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes while waiting: verbose GTK logging can fill a pipe.
    let stdout = capture(child.stdout.take().unwrap());
    let stderr = capture(child.stderr.take().unwrap());
    // A broken main loop must not hang the entire suite indefinitely.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let status = child.wait().unwrap();
    let stdout = stdout.join().unwrap().unwrap();
    let stderr = stderr.join().unwrap().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        !timed_out && status.success(),
        "{name}: timed out: {timed_out}\n{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
}

fn capture(
    mut pipe: impl std::io::Read + Send + 'static,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        pipe.by_ref().take(1024 * 1024).read_to_end(&mut bytes)?;
        // Keep draining after the diagnostic limit without retaining more data.
        std::io::copy(&mut pipe, &mut std::io::sink())?;
        Ok(bytes)
    })
}

fn application() -> (gtk::Application, Rc<Shared>) {
    gtk::init().unwrap();
    let app = gtk::Application::builder()
        .application_id(format!("dev.soi.Test.Run{}", Uuid::new_v4().simple()))
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    app.set_default();
    let shared = init_shared(&app);
    {
        let mut settings = shared.settings.borrow_mut();
        settings.shell = Some("/bin/sh".into());
        // Not on the desktop of whoever runs the tests.
        settings.desktop_notifications = Some(false);
    }
    (app, shared)
}

fn wait_until(mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let context = glib::MainContext::default();
    while !check() {
        assert!(
            std::time::Instant::now() < deadline,
            "live context did not synchronize"
        );
        for _ in 0..100 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn saved_window(id: Uuid) -> WindowSession {
    Session::load()
        .into_windows()
        .into_iter()
        .find(|w| w.id == id)
        .unwrap()
}

fn reopen(from: &App, id: Uuid) {
    from.window
        .lookup_action("open-window")
        .unwrap()
        .activate(Some(&id.to_string().to_variant()));
}

fn mapped_label(widget: &gtk::Widget, text: &str) -> Option<gtk::Label> {
    if let Some(label) = widget.downcast_ref::<gtk::Label>()
        && label.text() == text
        && label.is_mapped()
    {
        return Some(label.clone());
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(label) = mapped_label(&widget, text) {
            return Some(label);
        }
        child = widget.next_sibling();
    }
    None
}

fn double_click(widget: &impl IsA<gtk::Widget>) {
    let controllers = widget.observe_controllers();
    let gesture = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i))
        .find_map(|controller| {
            controller
                .downcast::<gtk::GestureClick>()
                .ok()
                .filter(|click| {
                    click.propagation_phase() == gtk::PropagationPhase::Capture
                        && click.button() == gdk::BUTTON_PRIMARY
                })
        })
        .expect("double-click rename gesture");
    gesture.emit_by_name::<()>("pressed", &[&2i32, &0.0f64, &0.0f64]);
}

/// The ids of the window switcher's rows, open windows first.
fn switcher_rows(state: &App) -> Vec<String> {
    fn collect(widget: &gtk::Widget, rows: &mut Vec<String>) {
        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
            rows.push(row.widget_name().to_string());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            collect(&widget, rows);
            child = widget.next_sibling();
        }
    }
    let mut rows = Vec::new();
    collect(state.window_list.upcast_ref(), &mut rows);
    rows
}

fn switcher_lists_open_and_closed_windows_aligned() {
    isolated("switcher_lists_open_and_closed_windows_aligned", || {
        let (_app, shared) = application();
        let first = new_window(&shared, None, true);
        let second = new_window(&shared, None, true);
        let (first_id, second_id) = (first.id.get().to_string(), second.id.get().to_string());
        assert_eq!(switcher_rows(&first), [first_id.clone(), second_id.clone()]);
        second.name.replace("Closed one".into());
        second.window.close();
        assert_eq!(
            switcher_rows(&first),
            [first_id, second_id],
            "a closed window stays in the list"
        );
        first.window.present();
        wait_until(|| first.window_button.is_mapped());
        first.window_button.popup();
        let popover = first.window_button.popover().unwrap();
        let names = [first.name.borrow().clone(), "Closed one".into()];
        wait_until(|| {
            names.iter().all(|name| {
                mapped_label(popover.upcast_ref(), name)
                    .and_then(|label| label.compute_bounds(&popover))
                    .is_some_and(|b| b.width() > 0.0)
            })
        });
        let positions: Vec<f32> = names
            .iter()
            .map(|name| {
                mapped_label(popover.upcast_ref(), name)
                    .unwrap()
                    .compute_bounds(&popover)
                    .unwrap()
                    .x()
            })
            .collect();
        assert!(
            (positions[0] - positions[1]).abs() <= 1.0,
            "open and closed names must align: {positions:?}"
        );
        first.window_button.popdown();
        first.window.close();
    });
}

fn sidebar_names_rename_inline_and_support_commit_cancel_and_empty_input() {
    isolated(
        "sidebar_names_rename_inline_and_support_commit_cancel_and_empty_input",
        || {
            let (_app, shared) = application();
            let window = new_window(&shared, None, true);
            let workspace = window.active.get().unwrap();
            let row = find_row(&window, &workspace.to_string()).unwrap();
            double_click(&row);
            let editor = row_name_editor(&row).expect("workspace name must edit inside its row");
            assert!(editor.is_editing());
            editor.set_text("  Development  ");
            editor
                .activate_action("editing.stop", Some(&true.to_variant()))
                .unwrap();
            assert_eq!(
                window.workspaces.borrow()[&workspace]
                    .name
                    .borrow()
                    .as_str(),
                "Development"
            );
            assert_eq!(editor.text(), "Development");
            assert_eq!(find_row(&window, &workspace.to_string()).unwrap(), row);

            rename_workspace(&window, workspace);
            editor.set_text("Discard this");
            editor
                .activate_action("editing.stop", Some(&false.to_variant()))
                .unwrap();
            assert_eq!(editor.text(), "Development");
            assert_eq!(
                window.workspaces.borrow()[&workspace]
                    .name
                    .borrow()
                    .as_str(),
                "Development"
            );
            rename_workspace(&window, workspace);
            editor.set_text("   ");
            editor.stop_editing(true);
            assert_eq!(
                editor.text(),
                "Development",
                "empty names must restore the original name"
            );

            new_group(&window, Vec::new(), None);
            let group = match window.tree.borrow().last().unwrap() {
                Node::Group { id, .. } => *id,
                _ => panic!("group missing"),
            };
            let group_row = find_row(&window, &format!("group:{group}")).unwrap();
            let group_editor =
                row_name_editor(&group_row).expect("group must rename inside its row");
            assert!(group_editor.is_editing());
            group_editor.stop_editing(false);
            double_click(&group_editor);
            assert!(
                group_editor.is_editing(),
                "double-click must start editing group names"
            );
            group_editor.set_text("  Services  ");
            group_editor.stop_editing(true);
            assert_eq!(group_name(&window, group).as_deref(), Some("Services"));
            let saved = saved_window(window.id.get());
            assert!(
                saved
                    .sidebar
                    .iter()
                    .any(|entry| matches!(entry, Entry::Workspace(w) if w.name == "Development"))
            );
            assert!(
                saved
                    .sidebar
                    .iter()
                    .any(|entry| matches!(entry, Entry::Group(g) if g.name == "Services"))
            );
            window.window.close();
        },
    );
}

fn inline_rename_commits_on_focus_loss_and_group_arrows_still_toggle() {
    isolated(
        "inline_rename_commits_on_focus_loss_and_group_arrows_still_toggle",
        || {
            let (_app, shared) = application();
            let window = new_window(&shared, None, true);
            wait_until(|| window.window.is_mapped());
            let workspace = window.active.get().unwrap();
            rename_workspace(&window, workspace);
            let row = find_row(&window, &workspace.to_string()).unwrap();
            let editor = row_name_editor(&row).unwrap();
            editor.set_text("Saved on blur");
            let terminal = current_terminal(&window).unwrap();
            terminal.widget().grab_focus();
            wait_until(|| {
                window.workspaces.borrow()[&workspace]
                    .name
                    .borrow()
                    .as_str()
                    == "Saved on blur"
            });
            assert!(
                terminal.widget().is_focus(),
                "committing on blur must not steal focus back"
            );
            new_group(&window, Vec::new(), None);
            finish_inline_renames(&window, false);
            let group = match window.tree.borrow().last().unwrap() {
                Node::Group { id, .. } => *id,
                _ => panic!("group missing"),
            };
            move_to_group(&window, workspace, Some(group));
            let row = find_row(&window, &format!("group:{group}")).unwrap();
            let arrow = row
                .child()
                .unwrap()
                .first_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap();
            arrow.emit_clicked();
            assert!(
                find_row(&window, &workspace.to_string()).is_none(),
                "arrow must collapse the group"
            );
            let row = find_row(&window, &format!("group:{group}")).unwrap();
            row.child()
                .unwrap()
                .first_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .emit_clicked();
            assert!(
                find_row(&window, &workspace.to_string()).is_some(),
                "arrow must expand the group"
            );
            window.window.close();
        },
    );
}

fn new_windows_are_saved_immediately() {
    isolated("new_windows_are_saved_immediately", || {
        let (_app, shared) = application();
        let first = new_window(&shared, None, true);
        let second = new_window(&shared, None, true);
        assert_eq!(
            Session::load().into_windows().len(),
            2,
            "creating a window must immediately create its saved context"
        );
        second.window.close();
        first.window.close();
    });
}

fn closing_a_window_retains_its_context() {
    isolated("closing_a_window_retains_its_context", || {
        let (_app, shared) = application();
        let first = new_window(&shared, None, true);
        let second = new_window(&shared, None, true);
        second.name.replace("Retained window".into());
        save_all(&shared);
        second.window.close();
        let saved = Session::load().into_windows();
        assert!(
            saved
                .iter()
                .any(|w| w.name.as_deref() == Some("Retained window")),
            "closing an extra window must retain its saved context"
        );
        let context = saved_window(second.id.get());
        assert!(context.closed);
        assert!(
            second.panes.borrow().is_empty(),
            "closed contexts must release live terminals"
        );
        assert!(
            switcher_rows(&first).contains(&second.id.get().to_string()),
            "switcher must list the saved context"
        );
        reopen(&first, second.id.get());
        let reopened = shared
            .windows()
            .into_iter()
            .find(|w| w.id.get() == second.id.get())
            .unwrap();
        assert_eq!(snapshot(&reopened).sidebar, context.sidebar);
        assert!(!saved_window(second.id.get()).closed);
        reopen(&first, second.id.get());
        assert_eq!(
            shared.windows().len(),
            2,
            "reopening an open context must only focus it"
        );
        assert_eq!(Session::load().into_windows().len(), 2);
        reopened.window.close();
        first.window.close();
    });
}

fn saved_windows_open_in_place() {
    isolated("saved_windows_open_in_place", || {
        let (_app, shared) = application();
        let first = new_window(&shared, None, true);
        let second = new_window(&shared, None, true);
        second.name.replace("Work".into());
        new_terminal_workspace(&second, None);
        save_all(&shared);
        let (first_id, second_id) = (first.id.get(), second.id.get());
        let work = snapshot(&second).sidebar;
        second.window.close();
        first
            .window
            .lookup_action("swap-window")
            .unwrap()
            .activate(Some(&second_id.to_string().to_variant()));
        assert_eq!(shared.windows().len(), 1, "no new window");
        assert_eq!(first.id.get(), second_id);
        assert_eq!(*first.name.borrow(), "Work");
        assert_eq!(snapshot(&first).sidebar, work);
        assert!(saved_window(first_id).closed, "the old one is saved");
        assert!(!saved_window(second_id).closed);
        let state = first.window.action_state("open-window").unwrap();
        assert_eq!(state.get::<String>().unwrap(), second_id.to_string());
        // And back again.
        first
            .window
            .lookup_action("swap-window")
            .unwrap()
            .activate(Some(&first_id.to_string().to_variant()));
        assert_eq!(first.id.get(), first_id);
        assert!(saved_window(second_id).closed);
        first.window.close();
    });
}

fn starting_again_brings_back_a_saved_window() {
    isolated("starting_again_brings_back_a_saved_window", || {
        let (_app, shared) = application();
        let first = new_window(&shared, None, true);
        let second = new_window(&shared, None, true);
        let second_id = second.id.get();
        second.window.close();
        reopen_last(&shared);
        assert_eq!(shared.windows().len(), 2);
        assert!(window_with(&shared, second_id).is_some());
        reopen_last(&shared);
        assert_eq!(shared.windows().len(), 2, "nothing saved: no new window");
        for window in shared.windows().into_iter().rev() {
            window.window.close();
        }
        drop(first);
    });
}

fn saved_windows_can_be_deleted() {
    isolated("saved_windows_can_be_deleted", || {
        let (_app, shared) = application();
        let first = new_window(&shared, None, true);
        let second = new_window(&shared, None, true);
        let third = new_window(&shared, None, true);
        let tab = second.panes.borrow().values().next().unwrap().tabs.borrow()[0].id;
        session::scrollback::save(tab, "output").unwrap();
        second.window.close();
        third.window.close();
        let delete = |target: &str| {
            first
                .window
                .lookup_action("delete-window")
                .unwrap()
                .activate(Some(&target.to_variant()));
        };
        delete(&second.id.get().to_string());
        let saved: Vec<Uuid> = Session::load()
            .into_windows()
            .iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(saved, [first.id.get(), third.id.get()]);
        assert!(
            session::scrollback::load(tab).is_none(),
            "a deleted context's output must go too"
        );
        delete("all");
        assert_eq!(Session::load().into_windows().len(), 1);
        assert_eq!(
            switcher_rows(&first),
            [first.id.get().to_string()],
            "no closed windows left"
        );
        first.window.close();
    });
}

fn context_live_updates_groups_splits_tabs_and_directories() {
    isolated(
        "context_live_updates_groups_splits_tabs_and_directories",
        || {
            let (_app, shared) = application();
            let window = new_window(&shared, None, true);
            let project = home_dir().join("project");
            fs::create_dir(&project).unwrap();
            let pane = current_pane(&window).unwrap();
            new_tab(&window, &pane, Some(project));
            split_pane(&window, &pane, gtk::Orientation::Horizontal);
            let workspace = window.active.get().unwrap();
            let at = detach(&mut window.tree.borrow_mut(), workspace);
            new_group(&window, vec![workspace], at);
            // Exercise the regular autosave source: no explicit save here.
            wait_until(|| saved_window(window.id.get()) == snapshot(&window));
            let saved = saved_window(window.id.get());
            let Entry::Group(group) = &saved.sidebar[0] else {
                panic!("group not saved")
            };
            let Layout::Split { start, end, .. } = &group.workspaces[0].layout else {
                panic!("split not saved")
            };
            let Layout::Pane { tabs, active, .. } = &**start else {
                panic!("pane not saved")
            };
            assert_eq!(tabs.len(), 2);
            assert_eq!(*active, 1);
            assert_eq!(tabs[1], home_dir().join("project"));
            assert!(matches!(**end, Layout::Pane { .. }));
            window.window.close();
        },
    );
}

fn restart_restores_open_windows_and_retains_closed_contexts() {
    isolated(
        "restart_restores_open_windows_and_retains_closed_contexts",
        || {
            let (app, shared) = application();
            let first = new_window(&shared, None, true);
            let second = new_window(&shared, None, true);
            let first_id = first.id.get();
            let second_id = second.id.get();
            second.window.close();
            first.window.close();

            let restored = init_shared(&app);
            restore_windows(&restored);
            let windows = restored.windows();
            assert_eq!(
                windows.len(),
                1,
                "closed contexts must not reopen automatically"
            );
            assert_eq!(windows[0].id.get(), first_id);
            assert!(
                restored
                    .session
                    .borrow()
                    .windows
                    .iter()
                    .any(|w| w.id == second_id && w.closed)
            );
            assert_eq!(
                next_window_name(&restored),
                "Window 3",
                "closed names stay reserved"
            );
            reopen(&windows[0], second_id);
            let reopened = restored
                .windows()
                .into_iter()
                .find(|w| w.id.get() == second_id)
                .unwrap();
            assert_eq!(restored.session.borrow().windows.len(), 2);
            reopened.window.close();
            windows[0].window.close();
        },
    );
}

fn numbers_count_within_the_group_and_groups_switch() {
    isolated("numbers_count_within_the_group_and_groups_switch", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let a = window.active.get().unwrap();
        new_terminal_workspace(&window, None);
        let b = window.active.get().unwrap();
        new_terminal_workspace(&window, None);
        let c = window.active.get().unwrap();
        new_terminal_workspace(&window, None);
        let d = window.active.get().unwrap();
        // Sidebar: a, group [b, c], d.
        new_group(&window, Vec::new(), None);
        let group = match window.tree.borrow().last().unwrap() {
            Node::Group { id, .. } => *id,
            Node::Workspace(_) => panic!("group missing"),
        };
        move_to_group(&window, b, Some(group));
        move_to_group(&window, c, Some(group));
        let go = |action: &str| {
            run_action(&window, action);
            window.active.get().unwrap()
        };

        select_workspace(&window, c);
        assert_eq!(go("workspace-1"), b, "first of the group");
        assert_eq!(go("workspace-2"), c);
        assert_eq!(go("workspace-3"), c, "no third in the group");
        select_workspace(&window, a);
        assert_eq!(go("workspace-2"), d, "second ungrouped");

        // Groups come back to where you were in them.
        assert_eq!(go("next-group"), c);
        assert_eq!(go("next-group"), d, "around to the ungrouped");
        assert_eq!(go("previous-group"), c);
        assert_eq!(go("workspace-1"), b);
        assert_eq!(go("previous-group"), d);
        assert_eq!(go("next-group"), b);
        window.window.close();
    });
}

fn windows_maximize_and_zoom_is_kept() {
    isolated("windows_maximize_and_zoom_is_kept", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        wait_until(|| window.window.is_mapped());
        run_action(&window, "maximize-window");
        wait_until(|| window.window.is_maximized());
        run_action(&window, "maximize-window");
        wait_until(|| !window.window.is_maximized());

        if cfg!(target_os = "macos") {
            let surface = window.window.surface().unwrap();
            let screen = gdk::prelude::DisplayExt::monitor_at_surface(&surface.display(), &surface)
                .unwrap()
                .geometry();
            // Each move sticks, also after another one.
            for (action, wide) in [
                ("snap-top", false),
                ("snap-left", true),
                ("snap-bottom", false),
                ("snap-right", true),
            ] {
                let snapped = || {
                    let (w, h) = (window.window.width(), window.window.height());
                    if wide {
                        (w - screen.width() / 2).abs() <= 2 && h > screen.height() / 2
                    } else {
                        (w - screen.width()).abs() <= 2 && h < screen.height() * 2 / 3
                    }
                };
                run_action(&window, action);
                wait_until(snapped);
                // GTK used to put the old size back a moment later.
                let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
                wait_until(|| std::time::Instant::now() > until);
                assert!(snapped(), "{action} didn't stay");
            }
        }

        run_action(&window, "zoom-in");
        persist::save_all(&shared);
        let zoom = Session::load().zoom.expect("zoom saved");
        assert!((zoom - 1.1).abs() < 1e-9, "{zoom}");
        // Starting again: zoomed from the first terminal on.
        let (_again, restarted) = application();
        assert!((restarted.zoom.get() - 1.1).abs() < 1e-9);
        let font = |shared: &Shared| shared.theme.borrow().font.as_ref().map(|f| f.size());
        assert_eq!(font(&restarted), font(&shared), "the zoomed font");
        run_action(&window, "zoom-reset");
        persist::save_all(&shared);
        assert_eq!(Session::load().zoom, None, "normal size isn't saved");
        window.window.close();
    });
}

fn tabs_move_between_panes_by_drag_and_drop() {
    isolated("tabs_move_between_panes_by_drag_and_drop", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let first = current_pane(&window).unwrap();
        new_tab(&window, &first, None);
        let tabs = |pane: &gtk::Box| window.panes.borrow()[pane].tabs.borrow().clone();
        let ws = window.active.get().unwrap();
        let root = || {
            window.workspaces.borrow()[&ws]
                .layout
                .first_child()
                .unwrap()
        };
        let moved = tabs(&first)[1].clone();
        let terminal = moved.terminal.widget().clone();

        // To the right edge: a split, with the tab as the new pane.
        move_tab(&window, &moved, &first, DropZone::Right);
        let paned = root().downcast::<gtk::Paned>().expect("a split");
        assert_eq!(paned.orientation(), gtk::Orientation::Horizontal);
        assert_eq!(paned.start_child().as_ref(), Some(first.upcast_ref()));
        let second = paned.end_child().unwrap().downcast::<gtk::Box>().unwrap();
        assert!(Rc::ptr_eq(&tabs(&second)[0], &moved));
        assert_eq!(tabs(&first).len(), 1);
        assert_eq!(moved.terminal.widget(), &terminal, "the same terminal");

        // Into the middle of the first pane: the empty one closes.
        move_tab(&window, &moved, &first, DropZone::Center);
        assert!(root().is::<gtk::Box>(), "no split left");
        assert_eq!(tabs(&first).len(), 2);
        assert!(!window.panes.borrow().contains_key(&second));

        // A pane's only tab moves the whole pane: here above the other.
        split_pane(&window, &first, gtk::Orientation::Horizontal);
        let paned = root().downcast::<gtk::Paned>().unwrap();
        let other = paned.end_child().unwrap().downcast::<gtk::Box>().unwrap();
        let only = tabs(&other)[0].clone();
        move_tab(&window, &only, &first, DropZone::Top);
        let paned = root().downcast::<gtk::Paned>().unwrap();
        assert_eq!(paned.orientation(), gtk::Orientation::Vertical);
        assert_eq!(paned.end_child().as_ref(), Some(first.upcast_ref()));
        let top = paned.start_child().unwrap().downcast::<gtk::Box>().unwrap();
        assert!(Rc::ptr_eq(&tabs(&top)[0], &only));
        // Dropped onto its own pane, a lone tab stays.
        move_tab(&window, &only, &top, DropZone::Left);
        assert_eq!(root(), paned.upcast::<gtk::Widget>());
        // The new layout is what gets saved.
        let saved = snapshot(&window);
        let Entry::Workspace(saved) = &saved.sidebar[0] else {
            panic!("workspace not saved")
        };
        assert!(matches!(
            saved.layout,
            Layout::Split {
                orientation: session::Orientation::Vertical,
                ..
            }
        ));

        // A popped-out pane goes back among the tabs where it's dropped
        // on the tab strip: here first.
        move_tab(&window, &only, &first, DropZone::Tabs(0));
        assert!(root().is::<gtk::Box>(), "its pane closed");
        assert!(Rc::ptr_eq(&tabs(&first)[0], &only));
        assert_eq!(tabs(&first).len(), 3);
        let strip_order: Vec<gtk::Widget> = {
            let strip = window.panes.borrow()[&first].strip.clone();
            let mut out = Vec::new();
            let mut child = strip.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                out.push(widget);
            }
            out
        };
        let buttons: Vec<gtk::Widget> = tabs(&first)
            .iter()
            .map(|t| t.button.clone().upcast())
            .collect();
        assert_eq!(strip_order, buttons, "the header shows the same order");
        // On its own strip, a tab changes place: the first goes last.
        move_tab(&window, &only, &first, DropZone::Tabs(3));
        assert!(Rc::ptr_eq(&tabs(&first)[2], &only));
        move_tab(&window, &only, &first, DropZone::Tabs(0));
        assert!(Rc::ptr_eq(&tabs(&first)[0], &only));
        // A point on the header means the strip, below it the terminal.
        wait_until(|| first.height() > 100);
        let header = first.first_child().unwrap().height() as f64;
        let width = first.width() as f64;
        assert!(matches!(
            drop_zone(&window, &first, width - 5.0, header / 2.0),
            DropZone::Tabs(3)
        ));
        assert_eq!(
            drop_zone(&window, &first, width / 2.0, header + 5.0),
            DropZone::Top
        );
        assert_eq!(
            drop_zone(&window, &first, width / 2.0, first.height() as f64 / 2.0),
            DropZone::Center
        );

        // A moved tab's buttons act on the pane it is in now.
        moved.close.emit_clicked();
        assert!(!tabs(&first).iter().any(|tab| Rc::ptr_eq(tab, &moved)));
        window.window.close();
    });
}

fn dropped_tabs_move_only_once_the_drag_is_over() {
    isolated("dropped_tabs_move_only_once_the_drag_is_over", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let pane = current_pane(&window).unwrap();
        new_tab(&window, &pane, None);
        let tabs = |pane: &gtk::Box| window.panes.borrow()[pane].tabs.borrow().clone();
        let ws = window.active.get().unwrap();
        let split = || {
            window.workspaces.borrow()[&ws]
                .layout
                .first_child()
                .unwrap()
                .is::<gtk::Paned>()
        };
        let settle = || {
            let mut n = 0;
            wait_until(|| {
                n += 1;
                n > 20
            });
        };
        let tab = tabs(&pane)[1].clone();
        let drop = |zone| {
            shared.tab_drop.borrow_mut().target = Some((tab.id, pane.downgrade(), zone));
            finish_tab_drop(&shared);
        };
        let end = || {
            shared.tab_drop.borrow_mut().ended = true;
            finish_tab_drop(&shared);
        };

        // Dropped, but GTK still drags the tab: it stays for now.
        shared.tab_drop.replace(TabDrop::default());
        drop(DropZone::Right);
        settle();
        assert!(!split(), "not while the drag is still going");
        end();
        wait_until(split);

        // The other order works too, and a drag without a drop does nothing.
        let paned = window.workspaces.borrow()[&ws]
            .layout
            .first_child()
            .unwrap();
        let right = paned
            .downcast::<gtk::Paned>()
            .unwrap()
            .end_child()
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap();
        let moved = tabs(&right)[0].clone();
        assert!(Rc::ptr_eq(&moved, &tab));
        shared.tab_drop.replace(TabDrop::default());
        end();
        settle();
        assert!(split(), "a cancelled drag moves nothing");
        shared.tab_drop.replace(TabDrop::default());
        end();
        drop(DropZone::Center);
        wait_until(|| !split());
        assert_eq!(tabs(&pane).len(), 2);
        window.window.close();
    });
}

fn quick_switcher_goes_to_the_workspace_typed() {
    isolated("quick_switcher_goes_to_the_workspace_typed", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let renderer = window.active.get().unwrap();
        window.workspaces.borrow()[&renderer]
            .name
            .replace("Renderer".into());
        new_terminal_workspace(&window, None);
        assert_ne!(window.active.get(), Some(renderer));

        switcher::open(&window);
        let find = |test: &dyn Fn(&gtk::Widget) -> bool| {
            panel::find_descendant(window.stack.upcast_ref(), test).unwrap()
        };
        let entry = find(&|w| w.is::<gtk::SearchEntry>())
            .downcast::<gtk::SearchEntry>()
            .unwrap();
        let list = find(&|w| w.is::<gtk::ListBox>())
            .downcast::<gtk::ListBox>()
            .unwrap();
        let rows = || (0..).map_while(|i| list.row_at_index(i)).count();
        assert_eq!(rows(), 2, "every workspace to start with");
        entry.set_text("rend");
        wait_until(|| rows() == 1);
        entry.emit_activate();
        assert_eq!(window.active.get(), Some(renderer));
        window.window.close();
    });
}

fn terminal_output_comes_back_after_a_restart() {
    isolated("terminal_output_comes_back_after_a_restart", || {
        use std::os::unix::fs::PermissionsExt;
        let shell = |name: &str, text: &str| {
            let path = home_dir().join(name);
            fs::write(
                &path,
                format!("#!/bin/sh\nprintf '{text}\\r\\n'\nexec sleep 60\n"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path.to_string_lossy().into_owned()
        };
        let (app, shared) = application();
        shared.settings.borrow_mut().shell = Some(shell("before", "from before the restart"));
        let window = new_window(&shared, None, true);
        let pane = current_pane(&window).unwrap();
        let tab = |index: usize| window.panes.borrow()[&pane].tabs.borrow()[index].clone();
        let kept = tab(0);
        wait_until(|| kept.terminal.export(10).contains("from before"));
        // A tab closed before quitting doesn't come back by itself; its
        // output is kept to reopen it.
        new_tab(&window, &pane, None);
        let closed = tab(1);
        wait_until(|| closed.terminal.export(10).contains("from before"));
        save_output(&shared, &shared.windows());
        assert!(session::scrollback::load(closed.id).is_some());
        close_tab(&window, &pane, &closed);
        window.window.close();
        assert!(session::scrollback::load(kept.id).is_some());
        assert!(session::scrollback::load(closed.id).is_some());

        let restored = init_shared(&app);
        restored.settings.borrow_mut().shell = Some(shell("after", "after the restart"));
        restore_windows(&restored);
        let window = &restored.windows()[0];
        let pane = current_pane(window).unwrap();
        let tab = window.panes.borrow()[&pane].tabs.borrow()[0].clone();
        assert_eq!(tab.id, kept.id, "tabs keep their identity");
        wait_until(|| tab.terminal.export(10).contains("after the restart"));
        let text = tab.terminal.export(10);
        let before = text
            .find("from before the restart")
            .expect("old output is back");
        assert!(before < text.find("restored").unwrap());
        assert!(text.find("restored").unwrap() < text.find("after the restart").unwrap());
        window.window.close();
    });
}

fn context_tracks_shell_directory_changes() {
    isolated("context_tracks_shell_directory_changes", || {
        use std::os::unix::fs::PermissionsExt;
        let (_app, shared) = application();
        let project = home_dir().join("project");
        fs::create_dir(&project).unwrap();
        let shell = home_dir().join("test-shell");
        fs::write(
            &shell,
            "#!/bin/sh\ncd \"$HOME/project\" || exit\nexec /bin/sh\n",
        )
        .unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        shared.settings.borrow_mut().shell = Some(shell.to_string_lossy().into_owned());
        let window = new_window(&shared, None, true);
        wait_until(|| {
            let context = saved_window(window.id.get());
            let Entry::Workspace(workspace) = &context.sidebar[0] else {
                return false;
            };
            matches!(&workspace.layout, Layout::Pane { cwd, .. } if *cwd == project)
        });
        window.window.close();
    });
}

fn agents_report_states_and_alert_until_seen() {
    isolated("agents_report_states_and_alert_until_seen", || {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let (_app, shared) = application();
        // An "agent" in the foreground of each tab: sleep, with job
        // control so it gets the terminal like a program started by hand.
        let shell = home_dir().join("test-shell");
        fs::write(&shell, "#!/bin/sh\nset -m\nsleep 60\nexec /bin/sh\n").unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        {
            let mut settings = shared.settings.borrow_mut();
            settings.shell = Some(shell.to_string_lossy().into_owned());
            settings.agents = Some("sleep".into());
        }
        panel::listen(&shared);
        let window = new_window(&shared, None, true);
        let first = workspace_order(&window)[0];
        let tab = {
            let panes = window.panes.borrow();
            let pane = &workspace_panes(&window, first)[0];
            panes[pane].tabs.borrow()[0].clone()
        };
        // Terminals start once they have a size.
        wait_until(|| tab.terminal.pid().is_some());
        // Look elsewhere: a second workspace.
        new_terminal_workspace(&window, None);
        assert_ne!(window.active.get(), Some(first));
        wait_until(|| tab.agent.borrow().program.as_deref() == Some("sleep"));
        assert!(
            !tab.agent.borrow().unseen,
            "no alert for an agent just found"
        );

        let mut socket =
            std::os::unix::net::UnixStream::connect(crate::agents::socket_path().unwrap()).unwrap();
        writeln!(
            socket,
            r#"{{"tab":"{}","state":"input","message":"Allow edit?"}}"#,
            tab.id
        )
        .unwrap();
        drop(socket);
        wait_until(|| tab.agent.borrow().state == crate::agents::State::Input);
        let agent = tab.agent.borrow().clone();
        assert!(agent.unseen && agent.reported);
        assert_eq!(agent.message, "Allow edit?");

        let ui = &window.agents;
        assert_eq!(ui.count.label(), "1");
        assert!(ui.count.is_visible());
        assert!(tab.dot.is_visible());
        let notice = ui.notices.row_at_index(0).expect("a notification");
        assert_eq!(notice.widget_name(), tab.id.to_string().as_str());
        assert!(
            panel::find_descendant(notice.upcast_ref(), &|w| w
                .downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.label().contains("Allow edit?")))
            .is_some()
        );
        let row = find_row(&window, &first.to_string()).unwrap();
        let dot =
            panel::find_descendant(row.upcast_ref(), &|w| w.has_css_class("agent-dot")).unwrap();
        assert!(dot.is_visible(), "the workspace is marked");
        let agent_row = (0..)
            .map_while(|i| ui.list.row_at_index(i))
            .find(|row| row.widget_name() == tab.id.to_string().as_str())
            .expect("a row in the agents panel");
        assert!(agent_row.has_css_class("unseen"));
        // It asks: a reply box, which types into the tab.
        let reply =
            panel::find_descendant(agent_row.upcast_ref(), &|w| w.has_css_class("agent-reply"))
                .unwrap()
                .downcast::<gtk::Entry>()
                .unwrap();
        assert!(reply.get_visible(), "(the panel itself is hidden)");
        reply.set_text("yes-from-panel");
        reply.emit_activate();
        assert_eq!(reply.text(), "");
        wait_until(|| tab.terminal.export(20).contains("yes-from-panel"));
        // Typing a reply survives rows coming and going around it.
        panel::show(&window, true);
        wait_until(|| reply.is_mapped());
        let second = window.active.get().unwrap();
        wait_until(|| (0..).map_while(|i| ui.list.row_at_index(i)).count() == 2);
        // The other agent's row goes away (closing its workspace would
        // move the focus to a terminal on its own).
        let other = {
            let panes = window.panes.borrow();
            let pane = &workspace_panes(&window, second)[0];
            panes[pane].tabs.borrow()[0].clone()
        };
        select_workspace(&window, first);
        reply.grab_focus();
        reply.set_text("half a rep");
        let focus = GtkWindowExt::focus(&window.window).unwrap();
        assert!(focus.is_ancestor(&reply), "typing there");
        other.agent.replace(crate::agents::Agent::default());
        panel::refresh(&shared);
        assert_eq!((0..).map_while(|i| ui.list.row_at_index(i)).count(), 1);
        assert_eq!(reply.text(), "half a rep");
        let focus = GtkWindowExt::focus(&window.window).unwrap();
        assert!(focus.is_ancestor(&reply), "still typing there");
        reply.set_text("");
        panel::show(&window, false);

        // Clicking the row goes there and clears the marks.
        ui.list.emit_by_name::<()>("row-activated", &[&agent_row]);
        assert_eq!(window.active.get(), Some(first));
        assert!(!tab.agent.borrow().unseen);
        assert!(!ui.count.is_visible());
        assert!(!tab.dot.is_visible());

        // Shown or hidden, the panel comes back that way.
        assert!(!saved_window(window.id.get()).agents_panel);
        panel::toggle(&window);
        wait_until(|| saved_window(window.id.get()).agents_panel);
        window.window.close();
    });
}

fn zoomed_panes_fill_the_workspace_until_the_layout_changes() {
    isolated(
        "zoomed_panes_fill_the_workspace_until_the_layout_changes",
        || {
            let (_app, shared) = application();
            let window = new_window(&shared, None, true);
            let first = current_pane(&window).unwrap();
            split_pane(&window, &first, gtk::Orientation::Horizontal);
            let ws = active_workspace(&window).unwrap();
            let paned = ws
                .layout
                .first_child()
                .unwrap()
                .downcast::<gtk::Paned>()
                .unwrap();
            let second = paned.end_child().unwrap();

            zoom_pane(&window, &first);
            assert!(!second.is_visible(), "the other pane is hidden");
            assert!(first.has_css_class("zoomed"));
            // The layout that's saved is still the split.
            let saved = snapshot(&window);
            let Entry::Workspace(saved) = &saved.sidebar[0] else {
                panic!("workspace not saved")
            };
            assert!(matches!(saved.layout, Layout::Split { .. }));

            // Saved zoomed, it comes back zoomed.
            let mut copy = workspace_snapshot(&window, window.active.get().unwrap()).unwrap();
            let Layout::Split { start, .. } = &copy.layout else {
                panic!("a split")
            };
            assert!(matches!(**start, Layout::Pane { zoomed: true, .. }));
            copy.id = Uuid::new_v4();
            add_workspace_ui(&window, &copy);
            let restored = window.workspaces.borrow()[&copy.id].zoomed.upgrade();
            assert!(restored.is_some_and(|pane| pane.has_css_class("zoomed")));

            // Zooming again brings the other one back.
            zoom_pane(&window, &first);
            assert!(second.is_visible());
            assert!(!first.has_css_class("zoomed"));

            // So does splitting a zoomed pane.
            zoom_pane(&window, &first);
            split_pane(&window, &first, gtk::Orientation::Vertical);
            assert!(second.is_visible());
            assert!(ws.zoomed.upgrade().is_none());
            window.window.close();
        },
    );
}

fn closed_tabs_reopen_with_their_folder_and_output() {
    isolated("closed_tabs_reopen_with_their_folder_and_output", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let pane = current_pane(&window).unwrap();
        let folder = std::env::temp_dir();
        new_tab(&window, &pane, Some(folder.clone()));
        let tabs = || window.panes.borrow()[&pane].tabs.borrow().clone();
        let tab = tabs()[1].clone();
        tab.terminal.restore("hello from before\r\n");
        close_tab(&window, &pane, &tab);
        assert_eq!(tabs().len(), 1);

        reopen_tab(&window);
        let tabs = tabs();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[1].cwd, folder);
        assert!(tabs[1].terminal.export(10).contains("hello from before"));
        assert!(shared.session.borrow().closed.is_empty());
        // Nothing left to reopen: nothing happens.
        reopen_tab(&window);
        assert_eq!(window.panes.borrow()[&pane].tabs.borrow().len(), 2);
        window.window.close();
    });
}

fn typing_goes_to_every_pane_while_broadcasting() {
    isolated("typing_goes_to_every_pane_while_broadcasting", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let first = current_pane(&window).unwrap();
        split_pane(&window, &first, gtk::Orientation::Horizontal);
        let panes = workspace_panes(&window, window.active.get().unwrap());
        let tab = |pane: &gtk::Box| window.panes.borrow()[pane].current().unwrap();
        let (from, to) = (tab(&panes[0]), tab(&panes[1]));
        wait_until(|| from.terminal.pid().is_some() && to.terminal.pid().is_some());

        let typed = Input::Text("echo mirrored-$((40+2))\r".into());
        mirror_input(&window, &from, &typed);
        toggle_broadcast(&window);
        assert!(
            active_workspace(&window)
                .unwrap()
                .holder
                .has_css_class("broadcast")
        );
        mirror_input(&window, &from, &typed);
        wait_until(|| to.terminal.export(50).matches("mirrored-42").count() == 1);
        assert!(
            !from.terminal.export(50).contains("mirrored"),
            "not twice into the source"
        );
        toggle_broadcast(&window);
        assert!(!active_workspace(&window).unwrap().broadcast.get());
        window.window.close();
    });
}

fn tabs_show_the_progress_programs_report() {
    isolated("tabs_show_the_progress_programs_report", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let pane = current_pane(&window).unwrap();
        let tab = window.panes.borrow()[&pane].current().unwrap();
        wait_until(|| tab.terminal.pid().is_some());
        let bar = panel::find_descendant(tab.button.upcast_ref(), &|w| {
            w.has_css_class("tab-progress")
        })
        .and_downcast::<gtk::ProgressBar>()
        .unwrap();
        let ws = window.active.get().unwrap();
        let row = find_row(&window, &ws.to_string()).unwrap();
        let sidebar = panel::find_descendant(row.upcast_ref(), &|w| w.has_css_class("ws-progress"))
            .and_downcast::<gtk::ProgressBar>()
            .unwrap();
        // Ctrl+U first: the window is on the desktop, keys typed meanwhile
        // can land in it.
        let run = |command: &str| {
            tab.terminal
                .type_input(&Input::Text(format!("\x15{command}\r")))
        };
        run(r"printf '\033]9;4;1;50\007'");
        wait_until(|| bar.is_visible());
        assert_eq!(bar.fraction(), 0.5);
        assert!(sidebar.is_visible(), "seen from other workspaces too");
        assert_eq!(sidebar.fraction(), 0.5);
        run(r"printf '\033]9;4;2;80\007'");
        wait_until(|| bar.has_css_class("error"));
        run(r"printf '\033]9;4;0\007'");
        wait_until(|| !bar.is_visible());
        window.window.close();
    });
}

fn screen_readers_get_the_lines_and_the_cursor() {
    isolated("screen_readers_get_the_lines_and_the_cursor", || {
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        let pane = current_pane(&window).unwrap();
        let tab = window.panes.borrow()[&pane].current().unwrap();
        tab.terminal.restore("$ ls\r\nsrc  docs\r\n$ ec");
        let (text, caret) = tab.terminal.accessible_text();
        assert!(text.starts_with("$ ls\nsrc  docs\n$ ec\n"), "{text:?}");
        assert_eq!(caret, 19, "after \"$ ec\"");
        assert_eq!(
            tab.terminal.widget().accessible_role(),
            gtk::AccessibleRole::Terminal
        );
        window.window.close();
    });
}

fn task_context_preview_and_history_work_in_real_windows() {
    isolated(
        "task_context_preview_and_history_work_in_real_windows",
        || {
            let (_app, shared) = application();
            let state = new_window(&shared, None, true);
            let ws = active_workspace(&state).unwrap();
            let plan = home_dir().join("plan.md");
            fs::write(&plan, "# First plan\n\n**safe**").unwrap();
            let mut task =
                crate::tasks::TaskContext::new("Task smoke test".into(), ws.root_dir.clone());
            task.plan = Some(plan.clone());
            task.status = crate::tasks::TaskStatus::Review;
            ws.task.replace(Some(task.clone()));
            save_all(&shared);
            let snapshot = snapshot(&state);
            let saved = snapshot
                .sidebar
                .iter()
                .find_map(|entry| match entry {
                    Entry::Workspace(ws) => ws.task.as_deref(),
                    _ => None,
                })
                .unwrap();
            assert_eq!(saved, &task);
            state
                .window
                .lookup_action("task-preview")
                .unwrap()
                .activate(None);
            let panel = ws.plan_panel.borrow().clone().unwrap();
            wait_until(|| panel.text() == "# First plan\n\n**safe**");
            fs::write(&plan, "# Updated plan").unwrap();
            wait_until(|| panel.text() == "# Updated plan");
            let tab = state
                .panes
                .borrow()
                .values()
                .next()
                .unwrap()
                .current()
                .unwrap();
            tab.agent.borrow_mut().program = Some("claude".into());
            tab.agent.borrow_mut().report(
                crate::agents::State::Done,
                "",
                false,
                std::time::Instant::now(),
            );
            panel::show(&state, true);
            panel::refresh(&shared);
            wait_until(|| {
                mapped_label(
                    state.agents.panel.upcast_ref(),
                    "Task smoke test · Ready for review",
                )
                .is_some()
            });
            tasks::record_report(
                &shared,
                &tab,
                crate::agents::SessionMetadata {
                    provider: "claude".into(),
                    id: "smoke-session".into(),
                },
            );
            wait_until(|| {
                crate::tasks::History::load(&crate::tasks::history_path())
                    .is_ok_and(|h| h.sessions.len() == 1)
            });
            state
                .window
                .lookup_action("task-preview")
                .unwrap()
                .activate(None);
            assert!(ws.plan_split.end_child().is_none());
            state
                .window
                .lookup_action("task-history")
                .unwrap()
                .activate(None);
            state
                .window
                .lookup_action("task-changes")
                .unwrap()
                .activate(None);
            for (action, title) in [
                ("task-new", "New task worktree"),
                ("task-context", "Task details"),
                ("task-history", "Agent session history"),
            ] {
                state.window.lookup_action(action).unwrap().activate(None);
                let windows = gtk::Window::list_toplevels();
                let window = windows
                    .into_iter()
                    .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                    .find(|window| window.title().as_deref() == Some(title))
                    .expect("task utility window");
                assert!(
                    window.has_css_class("soi-tools"),
                    "{title} must inherit soi's theme"
                );
                wait_until(|| window.is_mapped());
                let expected = shared.theme.borrow().foreground();
                let actual = window.color();
                assert!((actual.red() - expected.red()).abs() < 0.01);
                assert!((actual.green() - expected.green()).abs() < 0.01);
                assert!((actual.blue() - expected.blue()).abs() < 0.01);
                if title == "Task details" {
                    // Saving a change records it in the task's folder,
                    // outside the workspace.
                    let status =
                        panel::find_descendant(window.upcast_ref(), &|w| w.is::<gtk::DropDown>())
                            .and_downcast::<gtk::DropDown>()
                            .unwrap();
                    status.set_selected(3);
                    let save = panel::find_descendant(window.upcast_ref(), &|w| {
                        w.downcast_ref::<gtk::Button>()
                            .is_some_and(|b| b.label().as_deref() == Some("_Save"))
                    })
                    .and_downcast::<gtk::Button>()
                    .unwrap();
                    save.emit_clicked();
                    let task = ws.task.borrow().clone().unwrap();
                    let folder = task.folder.clone().expect("saving gives it a folder");
                    assert!(folder.starts_with(crate::tasks::plans_home()));
                    let metadata = fs::read_to_string(folder.join("metadata.md")).unwrap();
                    assert!(metadata.contains("- Status: Finished\n"), "{metadata}");
                    assert!(
                        metadata.contains(" — Status: Ready for review → Finished\n"),
                        "{metadata}"
                    );
                }
                window.close();
            }
            // Child test exits after checking construction; all state is isolated.
        },
    );
}

fn agent_navigation_keeps_sidebar_geometry_and_terminal_zoom() {
    isolated(
        "agent_navigation_keeps_sidebar_geometry_and_terminal_zoom",
        || {
            use std::os::unix::fs::PermissionsExt;
            let (_app, shared) = application();
            let shell = home_dir().join("agent-shell");
            fs::write(&shell, "#!/bin/sh\nset -m\nsleep 60\nexec /bin/sh\n").unwrap();
            fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
            shared.settings.borrow_mut().shell = Some(shell.to_string_lossy().into_owned());
            shared.settings.borrow_mut().agents = Some("sleep".into());
            let window = new_window(&shared, None, true);
            let first = window.active.get().unwrap();
            wait_until(|| {
                current_terminal(&window).is_some_and(|terminal| terminal.pid().is_some())
            });
            new_terminal_workspace(&window, None);
            panel::show(&window, true);
            wait_until(|| {
                window.agents.list.row_at_index(1).is_some() && window.sidebar.width() > 0
            });
            let row = find_row(&window, &first.to_string()).unwrap();
            let label =
                panel::find_descendant(row.upcast_ref(), &|widget| widget.is::<gtk::Label>())
                    .unwrap()
                    .downcast::<gtk::Label>()
                    .unwrap();
            let geometry = (
                window.sidebar.width(),
                window.sidebar.height(),
                label.layout().pixel_size(),
            );
            let font = label.pango_context().font_description();
            let zoom = shared.zoom.get();
            for index in 0..20 {
                let row = window.agents.list.row_at_index(index % 2).unwrap();
                window
                    .agents
                    .list
                    .emit_by_name::<()>("row-activated", &[&row]);
                std::thread::sleep(std::time::Duration::from_millis(20));
                let context = glib::MainContext::default();
                for _ in 0..100 {
                    if !context.pending() {
                        break;
                    }
                    context.iteration(false);
                }
                assert_eq!(
                    (
                        window.sidebar.width(),
                        window.sidebar.height(),
                        label.layout().pixel_size()
                    ),
                    geometry
                );
                assert_eq!(label.pango_context().font_description(), font);
                assert_eq!(shared.zoom.get(), zoom);
            }
            window.close_confirmed.set(true);
            window.window.close();
        },
    );
}

fn bash_marks_its_commands() {
    isolated("bash_marks_its_commands", || {
        let (_app, shared) = application();
        shared.settings.borrow_mut().shell = Some("bash".into());
        fs::write(
            home_dir().join(".bashrc"),
            "PS1='own$ '\nPROMPT_COMMAND='printf user-prompt'\n",
        )
        .unwrap();
        let window = new_window(&shared, None, true);
        let pane = current_pane(&window).unwrap();
        let tab = window.panes.borrow()[&pane].current().unwrap();
        wait_until(|| tab.terminal.export(5).contains("own$"));
        tab.terminal.type_input(&Input::Text("false\r".into()));
        wait_until(|| tab.terminal.exit_statuses().contains(&Some(1)));
        assert!(tab.terminal.export(10).contains("user-prompt"));
        window.close_confirmed.set(true);
        window.window.close();
    });
}

fn closed_workspaces_reopen_and_closed_things_survive_a_restart() {
    isolated(
        "closed_workspaces_reopen_and_closed_things_survive_a_restart",
        || {
            let (_app, shared) = application();
            let window = new_window(&shared, None, true);
            new_terminal_workspace(&window, None);
            let ws = window.active.get().unwrap();
            let pane = current_pane(&window).unwrap();
            split_pane(&window, &pane, gtk::Orientation::Horizontal);
            let tab = window.panes.borrow()[&pane].current().unwrap();
            tab.terminal.restore("kept output\r\n");
            window.workspaces.borrow()[&ws]
                .name
                .replace("Project".into());
            close_workspace(&window, ws);
            assert!(!window.workspaces.borrow().contains_key(&ws));

            // Saved with the session: there after a restart.
            save_all(&shared);
            let saved = Session::restored();
            assert!(matches!(
                saved.closed.last(),
                Some(Closed::Workspace(saved)) if saved.id == ws
            ));
            assert!(saved.tab_ids().contains(&tab.id), "its output is kept");

            reopen_tab(&window);
            assert_eq!(window.active.get(), Some(ws));
            assert_eq!(workspace_panes(&window, ws).len(), 2, "both panes");
            assert_eq!(&*window.workspaces.borrow()[&ws].name.borrow(), "Project");
            let pane = workspace_panes(&window, ws)[0].clone();
            let tab = window.panes.borrow()[&pane].current().unwrap();
            assert!(tab.terminal.export(10).contains("kept output"));
            window.close_confirmed.set(true);
            window.window.close();
        },
    );
}

fn the_plan_beside_the_terminal_is_a_live_markdown_editor() {
    isolated(
        "the_plan_beside_the_terminal_is_a_live_markdown_editor",
        || {
            let (_app, shared) = application();
            let state = new_window(&shared, None, true);
            let ws = active_workspace(&state).unwrap();
            let path = home_dir().join(".worktrees/a-long-feature-branch-name/soi/.soi/plan.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "# Plan\n\n- [ ] first").unwrap();
            let mut task = crate::tasks::TaskContext::new("Editor".into(), ws.root_dir.clone());
            task.plan = Some(path.clone());
            ws.task.replace(Some(task));
            state
                .window
                .lookup_action("task-preview")
                .unwrap()
                .activate(None);
            let plan = ws.plan_panel.borrow().clone().unwrap();
            wait_until(|| plan.text() == "# Plan\n\n- [ ] first");
            let panel = ws.plan_split.end_child().unwrap();
            // The plan keeps its width while the terminal's title changes
            // (it used to follow, and rewrap with every command).
            wait_until(|| panel.width() > 0);
            let width = panel.width();
            let tab = state.panes.borrow()[&current_pane(&state).unwrap()]
                .current()
                .unwrap();
            wait_until(|| tab.terminal.pid().is_some());
            let titled = |title: &str| {
                tab.terminal
                    .type_input(&Input::Text(format!("\x15printf '\\033]0;{title}\\007'\r")));
                wait_until(|| tab.title.label() == title);
                // A few frames for the layout to follow, if it would.
                let until = std::time::Instant::now() + std::time::Duration::from_millis(300);
                while std::time::Instant::now() < until {
                    glib::MainContext::default().iteration(false);
                }
            };
            titled("a long title that a program sets while it works away");
            assert_eq!(panel.width(), width);
            titled("x");
            assert_eq!(panel.width(), width);
            // Typed text is saved shortly after, and Enter continues the list.
            let buffer = plan.buffer();
            buffer.place_cursor(&buffer.end_iter());
            assert_eq!(
                plan.key(gdk::Key::Return, gdk::ModifierType::empty()),
                glib::Propagation::Stop
            );
            buffer.insert_at_cursor("second");
            wait_until(|| {
                fs::read_to_string(&path).unwrap() == "# Plan\n\n- [ ] first\n- [ ] second"
            });
            // Cmd+arrows go to the line's ends or the plan's, as on a Mac;
            // with Shift they select.
            let meta = gdk::ModifierType::META_MASK;
            let shift = gdk::ModifierType::SHIFT_MASK;
            buffer.place_cursor(&buffer.iter_at_offset(0));
            plan.key(gdk::Key::Right, meta | shift);
            let (from, to) = buffer.selection_bounds().unwrap();
            assert_eq!(buffer.text(&from, &to, false), "# Plan");
            plan.key(gdk::Key::Down, meta);
            assert!(buffer.iter_at_mark(&buffer.get_insert()).is_end());
            plan.key(gdk::Key::Left, meta);
            assert!(buffer.iter_at_mark(&buffer.get_insert()).starts_line());
            plan.key(gdk::Key::Up, meta | shift);
            let (from, to) = buffer.selection_bounds().unwrap();
            assert!(from.is_start() && to.starts_line() && to.offset() > 0);
            buffer.place_cursor(&buffer.end_iter());
            // Enter on an empty item ends the list instead.
            plan.key(gdk::Key::Return, gdk::ModifierType::empty());
            plan.key(gdk::Key::Return, gdk::ModifierType::empty());
            assert!(plan.text().ends_with("- [ ] second\n"), "{:?}", plan.text());
            // Ctrl+B wraps the selection.
            buffer.insert_at_cursor("word");
            let end = buffer.end_iter();
            let mut start = end;
            start.backward_chars(4);
            buffer.select_range(&start, &end);
            plan.key(gdk::Key::b, gdk::ModifierType::CONTROL_MASK);
            wait_until(|| fs::read_to_string(&path).unwrap().ends_with("\n**word**"));
            // A change on disk replaces a clean editor.
            fs::write(&path, "# Changed elsewhere").unwrap();
            wait_until(|| plan.text() == "# Changed elsewhere");
            // A new color scheme recolors the open plan.
            let blue = |theme: &crate::theme::TerminalTheme| theme.ansi_colors()[4];
            let link = || {
                let tag = plan.buffer().tag_table().lookup("link").unwrap();
                tag.property::<gdk::RGBA>("foreground-rgba")
            };
            assert_eq!(link(), blue(&shared.theme.borrow()));
            let scheme = crate::theme::available_schemes()
                .into_iter()
                .filter_map(|choice| crate::theme::named_scheme(&choice.id))
                .find(|scheme| blue(scheme) != blue(&shared.theme.borrow()))
                .unwrap();
            shared.theme.borrow_mut().set_colors_from(scheme);
            apply_colors(&shared);
            assert_eq!(link(), blue(&shared.theme.borrow()));
            // A width dragged to is kept for the next time it opens.
            let split = ws.plan_split.clone();
            split.set_position(split.width() - 500);
            wait_until(|| panel.width() > 450);
            let dragged = split.width() - split.position();
            let toggle = || {
                state
                    .window
                    .lookup_action("task-preview")
                    .unwrap()
                    .activate(None)
            };
            toggle();
            wait_until(|| split.end_child().is_none());
            toggle();
            wait_until(|| split.end_child().is_some());
            assert_eq!(split.width() - split.position(), dragged);
        },
    );
}

fn agents_to_start_can_be_added_edited_and_removed() {
    isolated("agents_to_start_can_be_added_edited_and_removed", || {
        let (_app, _shared) = application();
        let section = presets_section();
        let find = |test: &dyn Fn(&gtk::Widget) -> bool| {
            let mut found = Vec::new();
            let mut stack = vec![section.clone()];
            while let Some(widget) = stack.pop() {
                if test(&widget) {
                    found.push(widget.clone());
                }
                let mut child = widget.last_child();
                while let Some(c) = child {
                    child = c.prev_sibling();
                    stack.push(c);
                }
            }
            found
        };
        let button = |label: &str| {
            find(&|w| {
                w.downcast_ref::<gtk::Button>()
                    .is_some_and(|b| b.label().as_deref() == Some(label))
            })
            .pop()
            .and_downcast::<gtk::Button>()
            .unwrap()
        };
        let saved = || crate::presets::load();
        assert_eq!(
            saved().len(),
            3,
            "Claude Code, OpenCode and Pi to begin with"
        );
        button("Add agent").emit_clicked();
        assert_eq!(saved().len(), 4);
        assert_eq!(saved()[3].name, "New agent");
        // The new card's model: the last "provider/model" entry.
        let model = find(&|w| {
            w.downcast_ref::<gtk::Entry>()
                .and_then(|e| e.placeholder_text())
                .is_some_and(|p| p.contains("provider/model"))
        })
        .into_iter()
        .map(|w| w.downcast::<gtk::Entry>().unwrap())
        .last()
        .unwrap();
        model.set_text("openrouter/qwen/qwen3-coder");
        assert_eq!(saved()[3].model, "openrouter/qwen/qwen3-coder");
        let previews: Vec<String> = find(&|w| w.has_css_class("monospace"))
            .into_iter()
            .map(|w| w.downcast::<gtk::Label>().unwrap().label().to_string())
            .collect();
        assert!(
            previews.contains(
                &"opencode --model openrouter/qwen/qwen3-coder --prompt '<brief>'".to_string()
            ),
            "{previews:?}"
        );
        let removes = find(&|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|b| b.tooltip_text().as_deref() == Some("Remove this agent"))
        });
        assert_eq!(removes.len(), 4);
        removes
            .last()
            .unwrap()
            .downcast_ref::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        assert_eq!(saved().len(), 3);
    });
}

fn agents_started_directly_show_in_the_agents_panel() {
    isolated("agents_started_directly_show_in_the_agents_panel", || {
        use std::os::unix::fs::PermissionsExt;
        let (_app, shared) = application();
        let window = new_window(&shared, None, true);
        // Like Start with brief: the agent is the tab's only process, not a
        // program started from a shell.
        let agent = home_dir().join("opencode");
        fs::write(&agent, "#!/bin/sh\nsleep 60\n").unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
        let pane = current_pane(&window).unwrap();
        // A shell at its prompt still runs nothing.
        let shell = window.panes.borrow()[&pane].current().unwrap();
        wait_until(|| shell.terminal.pid().is_some());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(shell.terminal.running_program(), None);
        let command = vec![agent.to_string_lossy().into_owned()];
        let index = add_command_tab(&window, &pane, &home_dir(), None, Some(command), &[]).unwrap();
        activate_tab(&window, &pane, index, true);
        let tab = window.panes.borrow()[&pane].tabs.borrow()[index].clone();
        wait_until(|| tab.terminal.pid().is_some());
        assert_eq!(tab.terminal.running_program().as_deref(), Some("opencode"));
        wait_until(|| {
            panel::refresh(&shared);
            panel::sorted_agents(&shared).iter().any(|(t, agent)| {
                Rc::ptr_eq(t, &tab) && agent.program.as_deref() == Some("opencode")
            })
        });
        window.close_confirmed.set(true);
        window.window.close();
    });
}

fn agents_started_through_a_wrapper_count_as_the_agent() {
    isolated(
        "agents_started_through_a_wrapper_count_as_the_agent",
        || {
            use std::os::unix::fs::PermissionsExt;
            let (_app, shared) = application();
            let window = new_window(&shared, None, true);
            // A wrapper like mate: it gets the tool's command, and runs on.
            let seen = home_dir().join("args");
            let wrapper = home_dir().join("mate");
            fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nsleep 60\n",
                    seen.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
            let preset = crate::presets::Preset {
                model: "openai/gpt-6.1-sol".into(),
                wrapper: wrapper.to_string_lossy().into_owned(),
                ..crate::presets::Preset::new("opencode")
            };
            let task = crate::tasks::TaskContext::new("x".into(), home_dir());
            let pane = current_pane(&window).unwrap();
            tasks::launch(
                &window,
                &pane,
                &home_dir(),
                // No opencode on this PATH: the options stay.
                tasks::agent_command(&preset, Some("# Task: x".into()), &task, ""),
            )
            .unwrap();
            wait_until(|| seen.exists() && fs::read_to_string(&seen).unwrap().ends_with('\n'));
            assert_eq!(
                fs::read_to_string(&seen).unwrap(),
                "opencode\n--model\nopenai/gpt-6.1-sol\n--prompt\n# Task: x\n"
            );
            let tab = window.panes.borrow()[&pane].current().unwrap();
            assert_eq!(tab.terminal.running_program().as_deref(), Some("opencode"));
            wait_until(|| {
                panel::refresh(&shared);
                panel::sorted_agents(&shared).iter().any(|(t, agent)| {
                    Rc::ptr_eq(t, &tab) && agent.program.as_deref() == Some("opencode")
                })
            });
            window.close_confirmed.set(true);
            window.window.close();
        },
    );
}
