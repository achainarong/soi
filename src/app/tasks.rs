//! Task workspace UI, context editing and agent session bookmarks.
use super::*;
use crate::presets::Preset;
use crate::tasks::{AgentSession, History, TaskContext};

pub(super) fn background<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    let (send, receive) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = send.send_blocking(work());
    });
    glib::spawn_future_local(async move {
        if let Ok(result) = receive.recv().await {
            done(result);
        }
    });
}

pub(super) fn connect(state: &App) {
    for (name, handler) in [
        ("task-new", (|state| create(state, None)) as fn(&App)),
        ("task-context", task_view::open),
        ("task-history", history),
        ("task-archive", archived),
        ("task-changes", changes::open),
        ("task-preview", preview::toggle),
    ] {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(state);
        action.connect_activate(move |_, _| {
            if let Some(state) = weak.upgrade() {
                handler(&state);
            }
        });
        state.window.add_action(&action);
    }
    // Ask the shell for its PATH now, so starting an agent doesn't wait on it.
    let shell = state.shared.settings.borrow().shell_command();
    if let Some(shell) = shell.into_iter().next() {
        std::thread::spawn(move || crate::agents::agent_path(&shell));
    }
}

/// Deletes a task workspace's worktree after saying what would be lost, and
/// its branch too when asked and merged. The repository's own checkout is
/// never removed.
pub(super) fn remove_worktree(state: &App, id: Uuid) {
    let Some((name, worktree, task)) = state.workspaces.borrow().get(&id).and_then(|ws| {
        let task = ws.task.borrow().clone()?;
        Some((ws.name.borrow().clone(), ws.root_dir.clone(), task))
    }) else {
        return;
    };
    let Some(branch) = task.branch.clone() else {
        return;
    };
    let same = |a: &Path, b: &Path| a.canonicalize().ok() == b.canonicalize().ok();
    if same(&worktree, &task.repository) {
        gtk::AlertDialog::builder()
            .message("This is the repository itself")
            .detail("Only worktrees created for tasks can be removed.")
            .build()
            .show(Some(&state.window));
        return;
    }
    let running = running_in(state, &workspace_panes(state, id));
    let weak = Rc::downgrade(state);
    let (directory, git_branch) = (worktree.clone(), branch.clone());
    background(
        move || crate::git::worktree_state(&directory, &git_branch),
        move |result| {
            let Some(state) = weak.upgrade() else { return };
            let found = match result {
                Ok(found) => found,
                Err(e) => {
                    return gtk::AlertDialog::builder()
                        .message("Can't check the worktree")
                        .detail(e)
                        .build()
                        .show(Some(&state.window));
                }
            };
            let plural = |n: usize, one: &str, many: &str| {
                format!("{n} {}", if n == 1 { one } else { many })
            };
            let mut detail = vec![format!("Deletes {}.", tilde(&worktree))];
            if found.changes > 0 {
                detail.push(format!(
                    "{} will be lost.",
                    plural(found.changes, "uncommitted change", "uncommitted changes")
                ));
            }
            if task
                .plan
                .as_ref()
                .is_some_and(|plan| plan.starts_with(&worktree))
            {
                detail.push("Its plan is deleted with it.".into());
            }
            if !running.is_empty() {
                detail.push(format!("Stops {}.", running.join(", ")));
            }
            let mut buttons = vec!["Cancel", "Remove worktree"];
            if found.unmerged > 0 {
                detail.push(format!(
                    "{} on {branch} aren't on any other branch or remote, so the branch is kept.",
                    plural(found.unmerged, "commit", "commits")
                ));
            } else {
                buttons.push("Remove worktree and branch");
            }
            let dialog = gtk::AlertDialog::builder()
                .message(format!("Remove the worktree of “{name}”?"))
                .detail(detail.join("\n\n"))
                .buttons(buttons)
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let weak = Rc::downgrade(&state);
            dialog.choose(Some(&state.window), gio::Cancellable::NONE, move |answer| {
                let with_branch = match answer {
                    Ok(1) => false,
                    Ok(2) => true,
                    _ => return,
                };
                let Some(state) = weak.upgrade() else { return };
                // Its programs stop first, so nothing writes into the folder.
                discard_workspace(&state, id);
                let weak = Rc::downgrade(&state);
                background(
                    move || {
                        let result = crate::git::remove_worktree(
                            &task.repository,
                            &worktree,
                            with_branch.then_some(branch.as_str()),
                        );
                        // The task's folder stays, with its plan and history.
                        if let Ok(kept) = &result {
                            let event = match (with_branch, kept) {
                                (true, None) => format!("Removed the worktree and branch {branch}"),
                                (true, Some(_)) => format!(
                                    "Removed the worktree; kept branch {branch}, which isn't merged"
                                ),
                                (false, _) => "Removed the worktree".into(),
                            };
                            if let Err(e) = task.record(&[event]) {
                                crate::log::write(&format!("failed to record task history: {e}"));
                            }
                        }
                        result
                    },
                    move |result| {
                        let Some(state) = weak.upgrade() else { return };
                        let (message, detail) = match result {
                            Ok(None) => return,
                            Ok(Some(note)) => ("Worktree removed".to_string(), note),
                            Err(e) => ("Couldn't remove the worktree".to_string(), e),
                        };
                        gtk::AlertDialog::builder()
                            .message(message)
                            .detail(detail)
                            .build()
                            .show(Some(&state.window));
                    },
                );
            });
        },
    );
}

/// Keeps `label` showing the branch, whether it has changes, and the folder
/// `directory` returns. The folder is checked every second, so following a
/// terminal's `cd` or focus shows up quickly; git is asked again when it
/// changes, and otherwise every five seconds.
pub(super) fn watch_git(label: &gtk::Label, directory: impl Fn() -> PathBuf + 'static) {
    let weak = label.downgrade();
    let busy = Rc::new(Cell::new(false));
    let shown: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let ticks = Rc::new(Cell::new(0u32));
    let update = move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        let directory = directory();
        let moved = shown.borrow().as_ref() != Some(&directory);
        let due = ticks.get().is_multiple_of(5);
        ticks.set(ticks.get().wrapping_add(1));
        if !(moved || due) || busy.replace(true) {
            return glib::ControlFlow::Continue;
        }
        *shown.borrow_mut() = Some(directory.clone());
        let (weak, busy) = (weak.clone(), busy.clone());
        background(
            move || match crate::git::status(&directory) {
                Ok(status) => {
                    let mut lines = status.lines();
                    let branch = lines.next().unwrap_or("").trim_start_matches("## ");
                    let dirty = lines.next().is_some();
                    format!(
                        "{branch}{} • {}",
                        if dirty { " *" } else { "" },
                        tilde(&directory)
                    )
                }
                // Not a repository: just the folder.
                Err(_) => tilde(&directory),
            },
            move |text| {
                busy.set(false);
                if let Some(label) = weak.upgrade() {
                    label.set_label(&text);
                }
            },
        );
        glib::ControlFlow::Continue
    };
    update();
    glib::timeout_add_local(std::time::Duration::from_secs(1), update);
}

/// Utility windows close with Escape, except while an operation has disabled
/// closing. Bubble-phase handling lets native controls dismiss their popovers
/// first; search entries also expose Escape through their stop-search signal.
pub(super) fn close_on_escape(window: &gtk::Window) {
    let keys = gtk::EventControllerKey::new();
    let weak = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if key != gdk::Key::Escape || !modifiers.is_empty() {
            return glib::Propagation::Proceed;
        }
        if let Some(window) = weak.upgrade() {
            close_if_allowed(&window);
        }
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}

fn close_if_allowed(window: &gtk::Window) {
    if window.is_deletable() {
        window.close();
    }
}

fn dialog(
    state: &App,
    title: &str,
    accept: &str,
) -> (gtk::Window, gtk::Box, gtk::Button, gtk::Button) {
    let dialog = gtk::Window::builder()
        .transient_for(&state.window)
        .title(title)
        .modal(true)
        .default_width(620)
        .default_height(640)
        .build();
    dialog.add_css_class("soi-tools");
    close_on_escape(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for side in [
        gtk::PositionType::Top,
        gtk::PositionType::Bottom,
        gtk::PositionType::Left,
        gtk::PositionType::Right,
    ] {
        match side {
            gtk::PositionType::Top => content.set_margin_top(12),
            gtk::PositionType::Bottom => content.set_margin_bottom(12),
            gtk::PositionType::Left => content.set_margin_start(12),
            _ => content.set_margin_end(12),
        }
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&content)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    body.append(&scroll);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label(accept);
    actions.append(&cancel);
    actions.append(&save);
    body.append(&actions);
    dialog.set_child(Some(&body));
    let weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    (dialog, content, save, cancel)
}

pub(super) fn field(content: &gtk::Box, label: &str, value: &str) -> gtk::Entry {
    content.append(&gtk::Label::builder().label(label).xalign(0.0).build());
    let entry = gtk::Entry::builder().text(value).hexpand(true).build();
    content.append(&entry);
    entry
}

pub(super) fn note(content: &gtk::Box, text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    content.append(&label);
    label
}

pub(super) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(super) fn optional_path(text: &str, root: &Path) -> Option<PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| {
        let path = PathBuf::from(text);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    })
}

/// The New task worktree dialog. The task goes into `group`, or beside the
/// active workspace without one; that group's collection is chosen to
/// begin with, or the default collection.
pub(super) fn create(state: &App, group: Option<Uuid>) {
    // A group's task is likely for the project its workspaces are in.
    let group_root = group.and_then(|group| {
        let tree = state.tree.borrow();
        let first = tree.iter().find_map(|node| match node {
            Node::Group { id, workspaces, .. } if *id == group => workspaces.first().copied(),
            _ => None,
        })?;
        Some(state.workspaces.borrow().get(&first)?.root_dir.clone())
    });
    let root = group_root
        .or_else(|| {
            current_pane(state)
                .and_then(|pane| state.panes.borrow().get(&pane).map(|p| pane_cwd(p)))
        })
        .or_else(|| active_workspace(state).map(|ws| ws.root_dir.clone()))
        .unwrap_or_else(home_dir);
    let in_group = group.or_else(|| {
        let active = state.active.get()?;
        group_of(&state.tree.borrow(), active)
    });
    let chosen = in_group
        .and_then(|group| group_collection(state, group))
        .or_else(|| crate::collections::default_name(&crate::collections::load()));
    let (dialog, content, save, cancel) = dialog(state, "New task worktree", "Create");
    let title = field(&content, "Task name", "");
    let ticket = field(
        &content,
        "Ticket number (optional; names the task's folder)",
        "",
    );
    ticket.set_placeholder_text(Some("PROJ-123"));
    let description = text_area(&content, "Description (optional)", 80);
    let acceptance = text_area(&content, "Acceptance criteria (optional)", 60);
    let branch = field(&content, "New branch", "");
    let repository = field(&content, "Repository", &path_text(&root));
    let requests = requests::Picker::new(&root, &[], &[], true, chosen.as_deref());
    if let Some(row) = requests.take_collection_row() {
        content.append(&row);
    }
    let presets = Rc::new(crate::presets::load());
    let names: Vec<&str> = std::iter::once("Shell only")
        .chain(presets.iter().map(|preset| preset.name.as_str()))
        .collect();
    let agent = gtk::DropDown::from_strings(&names);
    agent.set_tooltip_text(Some("Set these up in Settings ▸ Agents"));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&gtk::Label::new(Some("Agent")));
    row.append(&agent);
    content.append(&row);
    // The rest has sensible defaults, so it stays folded away.
    let advanced = gtk::Expander::new(Some("Advanced settings"));
    let more = gtk::Box::new(gtk::Orientation::Vertical, 8);
    more.set_margin_top(8);
    advanced.set_child(Some(&more));
    content.append(&advanced);
    let issue = field(&more, "Issue link (optional reference)", "");
    let checklist = text_area(&more, "Checklist (optional; one item per line)", 60);
    let base = field(&more, "Base branch (optional; a branch, tag or commit)", "");
    base.set_placeholder_text(Some("What the repository has checked out when left empty"));
    let destination = field(
        &more,
        "Worktree directory (optional; relative to repository or absolute)",
        "",
    );
    destination.set_placeholder_text(Some(".worktrees/<branch>/<repository> when left empty"));
    note(&more, "Plan");
    let plan_mode = gtk::DropDown::from_strings(&[
        "Create a new plan from these details",
        "Link an existing plan",
        "No plan",
    ]);
    more.append(&plan_mode);
    let plan = field(
        &more,
        "Plan file (optional; relative to the new worktree; never overwritten)",
        "",
    );
    plan.set_placeholder_text(Some(
        "~/.local/state/soi/plans/<project>/<ticket or id>-<task>/plan.md",
    ));
    note(&more, "Agent model");
    let model = model_entry();
    model.set_sensitive(false);
    more.append(&model);
    let with_brief =
        gtk::CheckButton::with_label("Give the agent the task brief as its first prompt");
    // On by default: it's folded away under Advanced settings, where an
    // unticked box went unnoticed and agents started without their task.
    with_brief.set_active(true);
    with_brief.set_sensitive(false);
    more.append(&with_brief);
    more.append(&requests.widget);
    // Project skills and commands come with the repository chosen.
    let weak_requests = Rc::downgrade(&requests);
    repository.connect_changed(move |entry| {
        let path = PathBuf::from(entry.text().trim());
        if let Some(requests) = weak_requests.upgrade()
            && path.is_dir()
        {
            requests.set_repository(&path);
        }
    });
    let (weak_brief, weak_model, choices) =
        (with_brief.downgrade(), model.downgrade(), presets.clone());
    agent.connect_selected_notify(move |agent| {
        let preset = (agent.selected() as usize)
            .checked_sub(1)
            .and_then(|i| choices.get(i));
        if let Some(check) = weak_brief.upgrade() {
            check.set_sensitive(preset.is_some());
        }
        if let Some(model) = weak_model.upgrade() {
            model.set_sensitive(preset.is_some());
            model.set_text(preset.map_or("", |p| p.model.as_str()));
        }
    });
    let error = note(
        &content,
        "Closing a workspace never deletes its worktree or branch.",
    );
    let weak = Rc::downgrade(state);
    let weak_dialog = dialog.downgrade();
    save.connect_clicked(move |save| {
        let Some(dialog) = weak_dialog.upgrade() else { return };
        let Some(state) = weak.upgrade() else { dialog.close(); return };
        let title = title.text().trim().to_string();
        let branch = branch.text().trim().to_string();
        let base = Some(base.text().trim().to_string()).filter(|base| !base.is_empty());
        let repository = PathBuf::from(repository.text().trim());
        let destination = PathBuf::from(destination.text().trim());
        if title.is_empty() { error.set_label("Enter a task name."); return; }
        if branch.is_empty() { error.set_label("Enter a new branch name."); return; }
        if !repository.is_absolute() { error.set_label("Choose an existing Git repository using its absolute path."); return; }
        let issue = match crate::tasks::validate_issue(issue.text().as_str()) {
            Ok(issue) => issue,
            Err(e) => { advanced.set_expanded(true); error.set_label(&e); return; }
        };
        let ticket = match crate::tasks::validate_ticket(ticket.text().as_str()) {
            Ok(ticket) => ticket,
            Err(e) => { error.set_label(&e); return; }
        };
        let description = buffer_text(&description).trim_end().to_string();
        let acceptance = buffer_text(&acceptance).trim_end().to_string();
        let checklist = crate::tasks::parse_checklist(&buffer_text(&checklist));
        let (skills, commands, collection) = (requests.skills(), requests.commands(), requests.collection());
        let (plan_mode, plan) = (plan_mode.selected(), plan.text().to_string());
        if plan_mode == 1 && plan.trim().is_empty() { advanced.set_expanded(true); error.set_label("Enter the existing plan's path."); return; }
        let preset = (agent.selected() as usize).checked_sub(1).and_then(|i| presets.get(i))
            .map(|preset| with_model(preset, &model));
        let with_brief = with_brief.is_sensitive() && with_brief.is_active();
        save.set_sensitive(false);
        // Keep the dialog open while Git runs; no duplicate requests or closing
        // mid-operation with an unreported worktree left behind.
        cancel.set_sensitive(false);
        dialog.set_deletable(false);
        error.set_label("Creating worktree…");
        let agent_path = agent_path(&state);
        let (weak, dialog, error, save, cancel) = (Rc::downgrade(&state), dialog.clone(), error.clone(), save.clone(), cancel.clone());
        background(move || {
            let root = crate::git::repository(&repository)?;
            let destination = crate::git::worktree_destination(&root, &branch, &destination)?;
            let warning = crate::git::worktree_ignore_warning(&root, &destination)?;
            crate::git::create_worktree(&root, &branch, base.as_deref(), &destination)?;
            let mut task = TaskContext::new(title, root.clone());
            task.branch = Some(branch.clone());
            task.issue = issue;
            task.ticket = ticket;
            task.description = description;
            task.acceptance = acceptance;
            task.checklist = checklist;
            task.skills = skills;
            task.commands = commands;
            // Worktrees of one repository share its folder of tasks.
            let main = crate::git::main_repository(&root).unwrap_or(root);
            let folder = task.task_folder(&crate::tasks::project_folder(&main));
            task.folder = Some(folder.clone());
            // The worktree exists from here on, so plan problems are notes,
            // not failures that would hide it.
            let path = optional_path(&plan, &destination)
                .unwrap_or_else(|| folder.join("plan.md"));
            let plan_note = match plan_mode {
                0 => match crate::plan::create(&path, &task.plan_template()) {
                    Ok(()) => { task.plan = Some(path); None }
                    Err(crate::plan::CreateError::Exists) => {
                        task.plan = Some(path.clone());
                        Some(format!("{} already existed. It was linked, not overwritten.", path.display()))
                    }
                    Err(crate::plan::CreateError::Failed(e)) => Some(format!("The plan was not created: {e}")),
                },
                1 if path.is_file() => { task.plan = Some(path); None }
                1 => Some(format!("{} does not exist, so no plan was linked.", path.display())),
                _ => None,
            };
            let command = preset.as_ref().map(|p| agent_command(p, with_brief.then(|| task.brief(&destination)), &task, &agent_path));
            let mut events = vec![format!("Created on branch {branch}, worktree {}", destination.display())];
            if !task.skills.is_empty() || !task.commands.is_empty() {
                let from = collection.map(|name| format!(" (from collection {name})")).unwrap_or_default();
                events.push(format!(
                    "Requested skills: {}; commands: {}{from}",
                    crate::tasks::skill_list(&task.skills),
                    crate::tasks::command_list(&task.commands)
                ));
            }
            if let Some(preset) = &preset {
                events.push(started(preset, with_brief));
            }
            let record_note = task.record(&events).err().map(|e| format!("The task's history was not saved: {e}"));
            let notes: Vec<String> = warning.into_iter().chain(plan_note).chain(record_note).collect();
            Ok::<_, String>((destination, task, command, notes))
        }, move |result| {
            save.set_sensitive(true);
            cancel.set_sensitive(true);
            dialog.set_deletable(true);
            match result {
                Ok((directory, task, command, notes)) => {
                    if let Some(state) = weak.upgrade() {
                        let mut ws = Workspace::new(directory.clone());
                        ws.name = task.title.clone(); ws.task = Some(Box::new(task));
                        insert_workspace(&state, ws, group);
                        if let (Some(command), Some(pane)) = (command, current_pane(&state))
                            && let Err(error) = launch(&state, &pane, &directory, command) {
                                gtk::AlertDialog::builder().message("Worktree created, but agent could not start")
                                    .detail(error).build().show(Some(&state.window));
                        }
                        if !notes.is_empty() {
                            gtk::AlertDialog::builder().message("Worktree created — please check")
                                .detail(notes.join("\n\n")).build().show(Some(&state.window));
                        }
                    }
                    dialog.close();
                }
                Err(e) => error.set_label(&format!("{e}\nIf Git partially created a worktree, inspect it before retrying; Soi will not delete it.")),
            }
        });
    });
    dialog.present();
}

fn text_area(content: &gtk::Box, label: &str, height: i32) -> gtk::TextView {
    content.append(&gtk::Label::builder().label(label).xalign(0.0).build());
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .height_request(height)
        .accepts_tab(false)
        .build();
    content.append(&view);
    view
}

fn buffer_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .into()
}

pub(super) fn launch(
    state: &App,
    pane: &gtk::Box,
    directory: &Path,
    command: Command,
) -> Result<(), String> {
    let program = command.argv.first().ok_or("No command to run.")?;
    if crate::agents::find_program(program, &agent_path(state)).is_none() {
        return Err(format!(
            "{} ({program}) isn't installed or isn't on your PATH.",
            crate::agents::display_name(program)
        ));
    }
    if let Some((workspace, ws)) = workspace_of(state, pane)
        && let Some(main) = main_tab_of(&ws, pane)
    {
        select_main_tab_id(state, workspace, main.id);
    }
    let pane = new_command_main_tab(state, directory, command.argv, &command.env)
        .ok_or("Cannot open an agent tab. Check that the workspace directory still exists.")?;
    let index = 0;
    // Started through a wrapper, the tab runs the agent, not the wrapper.
    if let Some(agent) = &command.agent
        && let Some(tab) = state
            .panes
            .borrow()
            .get(&pane)
            .and_then(|pane| pane.tabs.borrow().get(index).cloned())
    {
        tab.terminal.set_program(agent);
    }
    activate_tab(state, &pane, index, true);
    save_all(&state.shared);
    Ok(())
}

/// The PATH agent tabs run with (see `crate::agents::agent_path`).
pub(super) fn agent_path(state: &App) -> String {
    let shell = state.shared.settings.borrow().shell_command();
    crate::agents::agent_path(shell.first().map_or("/bin/sh", String::as_str))
}

/// Where to change the model for one start; the preset's is filled in.
pub(super) fn model_entry() -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text("Model (optional; for this start only)")
        .tooltip_text("Passed to the agent as --model; the preset's is filled in")
        .hexpand(true)
        .build()
}

/// `preset`, with the model typed for this start.
pub(super) fn with_model(preset: &Preset, model: &gtk::Entry) -> Preset {
    Preset {
        model: model.text().trim().to_string(),
        ..preset.clone()
    }
}

/// A history entry for starting an agent.
pub(super) fn started(preset: &Preset, with_brief: bool) -> String {
    let model = match preset.model.trim() {
        "" => String::new(),
        model => format!(" ({model})"),
    };
    let brief = if with_brief { " with the brief" } else { "" };
    format!("Started {}{model}{brief}", preset.name)
}

/// The command that starts `preset` for `task`. The agent may also use the
/// project's task folders, which are outside the worktree: the plan, its
/// history, and earlier tasks'.
/// `path` is the PATH it runs with, to see which tool version is installed.
pub(super) fn agent_command(
    preset: &Preset,
    brief: Option<String>,
    task: &TaskContext,
    path: &str,
) -> Command {
    let tasks = task.folder.as_ref().and_then(|folder| folder.parent());
    Command {
        argv: preset.launch_command(brief, tasks, path),
        env: preset.env(tasks),
        agent: Some(preset.program.clone()),
    }
}

/// A program to start in a tab, and what its environment adds.
pub(super) struct Command {
    pub(super) argv: Vec<String>,
    pub(super) env: Vec<(String, String)>,
    /// The agent it runs, when that may not be its program: one started
    /// through a wrapper like `mate`.
    pub(super) agent: Option<String>,
}

pub(super) fn task_for_tab(shared: &Shared, tab: &Tab) -> Option<TaskContext> {
    let owner = window_of(shared, tab.terminal.widget())?;
    let (_, ws) = workspace_of(&owner, tab.terminal.widget())?;
    ws.task.borrow().clone()
}

pub(super) fn record_report(
    shared: &Rc<Shared>,
    tab: &Rc<Tab>,
    metadata: crate::agents::SessionMetadata,
) {
    if crate::tasks::validate_id(&metadata.id).is_err() || metadata.provider.len() > 80 {
        return;
    }
    let workspace = window_of(shared, tab.terminal.widget())
        .and_then(|owner| workspace_of(&owner, tab.terminal.widget()));
    let task = workspace
        .as_ref()
        .and_then(|(_, ws)| ws.task.borrow().clone());
    // Named after the task, else the workspace: "claude" alone says little.
    let title = task
        .as_ref()
        .map(|t| t.title.clone())
        .or_else(|| workspace.as_ref().map(|(_, ws)| ws.name.borrow().clone()))
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(metadata.provider.clone());
    let session = AgentSession {
        provider: metadata.provider,
        session_id: metadata.id,
        title,
        directory: tab_cwd(tab),
        task_id: task.map(|t| t.id),
        workspace_id: workspace.map(|(id, _)| id),
        updated: crate::tasks::now(),
    };
    background(
        move || crate::tasks::remember(session),
        |result| {
            if let Err(e) = result {
                crate::log::write(&format!("cannot save agent session: {e}"));
            }
        },
    );
}

pub(super) fn history(state: &App) {
    let window = gtk::Window::builder()
        .transient_for(&state.window)
        .title("Agent session history")
        .default_width(760)
        .default_height(520)
        .build();
    window.add_css_class("soi-tools");
    close_on_escape(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search task, provider, session ID or directory…")
        .build();
    let weak = window.downgrade();
    search.connect_stop_search(move |_| {
        if let Some(window) = weak.upgrade() {
            close_if_allowed(&window);
        }
    });
    content.append(&search);
    let add = gtk::Button::with_label("Bookmark session…");
    content.append(&add);
    let message = note(&content, "Loading local bookmarks…");
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_show_separators(true);
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .build();
    content.append(&scrolled);
    window.set_child(Some(&content));
    let weak = Rc::downgrade(state);
    let history_window = window.downgrade();
    add.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            if let Some(window) = history_window.upgrade() {
                window.close();
            }
            bookmark(&state);
        }
    });
    let records = Rc::new(RefCell::new(Vec::<AgentSession>::new()));
    let fill: Rc<dyn Fn(&str)> = {
        let (records, list, message, weak) = (
            records.clone(),
            list.clone(),
            message.clone(),
            Rc::downgrade(state),
        );
        Rc::new(move |query| {
            list.remove_all();
            let sessions = records.borrow();
            let mut count = 0;
            for session in sessions.iter().filter(|s| s.matches(query)) {
                count += 1;
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                row.set_margin_top(8);
                row.set_margin_bottom(8);
                row.set_margin_start(8);
                row.set_margin_end(8);
                let label = gtk::Label::builder()
                    .label(format!(
                        "{} · {}\n{}\n{} · updated {}",
                        session.title,
                        session.provider,
                        session.directory.display(),
                        session.session_id,
                        session.updated
                    ))
                    .xalign(0.0)
                    .hexpand(true)
                    .selectable(true)
                    .wrap(true)
                    .build();
                row.append(&label);
                let resume = gtk::Button::with_label("Resume");
                resume.set_valign(gtk::Align::Center);
                let available = session.resume_command().is_ok() && session.directory.is_dir();
                resume.set_sensitive(available);
                if !available {
                    resume.set_tooltip_text(Some(
                        "Unsupported provider, invalid ID, or missing directory",
                    ));
                }
                let (weak, session, message) = (weak.clone(), session.clone(), message.clone());
                resume.connect_clicked(move |_| {
                    if let Some(state) = weak.upgrade() {
                        match resume_session(&state, &session) {
                            Ok(()) => message.set_label("Opened in a new terminal tab."),
                            Err(e) => message.set_label(&e),
                        }
                    }
                });
                row.append(&resume);
                list.append(&row);
            }
            message.set_label(&format!(
                "{count} matching sessions. Provider-owned conversations stay with the provider."
            ));
        })
    };
    let changed = fill.clone();
    search.connect_search_changed(move |entry| changed(entry.text().as_str()));
    let weak_window = window.downgrade();
    background(
        || History::load(&crate::tasks::history_path()),
        move |result| {
            if weak_window.upgrade().is_none() {
                return;
            }
            match result {
                Ok(history) => {
                    records.replace(history.sessions);
                    fill(search.text().as_str());
                }
                Err(e) => message.set_label(&format!(
                    "Cannot read history: {e}. The file has not been overwritten."
                )),
            }
        },
    );
    window.present();
}

pub(super) fn resume_session(state: &App, session: &AgentSession) -> Result<(), String> {
    if !session.directory.is_dir() {
        return Err("The session directory is missing. Restore its worktree first.".into());
    }
    let command = as_installed(state, session.resume_command()?);
    // Never change the cwd of an existing running process. Reuse the workspace
    // it ran in while it exists, in whichever window, else one at the same
    // directory; otherwise open a new one at the recorded directory.
    let windows = state.shared.windows();
    let find = |matches: &dyn Fn(&Uuid, &WorkspaceUi) -> bool| {
        windows.iter().find_map(|window| {
            let id = window
                .workspaces
                .borrow()
                .iter()
                .find(|(id, ws)| matches(id, ws))
                .map(|(id, _)| *id)?;
            Some((window.clone(), id))
        })
    };
    let matching = session
        .workspace_id
        .and_then(|wanted| find(&|id, _| *id == wanted))
        .or_else(|| find(&|_, ws| ws.root_dir == session.directory));
    let state = match &matching {
        Some((window, _)) => window,
        None => state,
    };
    if let Some((_, id)) = matching {
        select_workspace(state, id);
    } else {
        let mut ws = Workspace::new(session.directory.clone());
        ws.name = session.title.clone();
        if let Some(task_id) = session.task_id {
            let saved = state.shared.session.borrow();
            for window in &saved.windows {
                for entry in &window.sidebar {
                    let workspaces: &[Workspace] = match entry {
                        Entry::Workspace(workspace) => std::slice::from_ref(workspace),
                        Entry::Group(group) => &group.workspaces,
                    };
                    if let Some(task) = workspaces
                        .iter()
                        .filter_map(|ws| ws.task.as_ref())
                        .find(|task| task.id == task_id)
                    {
                        ws.task = Some(task.clone());
                    }
                }
            }
        }
        insert_workspace(state, ws, None);
    }
    let pane = current_pane(state).ok_or("No terminal pane available")?;
    launch(
        state,
        &pane,
        &session.directory,
        Command {
            argv: command,
            env: Vec::new(),
            agent: None,
        },
    )?;
    state.window.present();
    Ok(())
}

/// Resumes the agent session a restored tab ran, in the tab's shell, so it
/// is a shell again once the agent quits. The command is typed ahead; the
/// shell reads it when it has started.
pub(super) fn resume_agent(state: &App, tab: &Tab, session: &SessionMetadata) {
    // Kept until the agent quits, also when it can't start now.
    tab.resume.replace(Some(session.clone()));
    // What it shows is new: save it.
    tab.untouched.set(false);
    let command = match session.resume_command() {
        Ok(command) => as_installed(state, command),
        Err(e) => {
            crate::log::write(&format!("cannot resume {} session: {e}", session.provider));
            return;
        }
    };
    if crate::agents::find_program(&command[0], &agent_path(state)).is_none() {
        crate::log::write(&format!("cannot resume {}: not installed", command[0]));
        return;
    }
    // IDs are letters, digits, `-` and `_` (see `resume_command`): nothing
    // the shell would interpret.
    tab.terminal.type_ahead(&format!("{}\r", command.join(" ")));
}

/// `command` with what the tool needs as installed (see `Compat`).
fn as_installed(state: &App, mut command: Vec<String>) -> Vec<String> {
    if crate::presets::Compat::of(&command[0], &agent_path(state)).standalone {
        command.insert(1, "--standalone".into());
    }
    command
}

pub(super) fn bookmark(state: &App) {
    let Some(ws) = active_workspace(state) else {
        return;
    };
    let workspace_id = state.active.get();
    let (dialog, content, save, _) = dialog(state, "Bookmark agent session", "Save");
    let title = field(
        &content,
        "Title",
        &ws.task
            .borrow()
            .as_ref()
            .map(|t| t.title.clone())
            .unwrap_or_else(|| ws.name.borrow().clone()),
    );
    let provider = field(
        &content,
        "Provider (claude, opencode, pi, codex, or another name)",
        "opencode",
    );
    let id = field(&content, "Provider session ID", "");
    let error = note(
        &content,
        "Claude hook reports create bookmarks automatically. Other sessions can be linked here.",
    );
    let weak = Rc::downgrade(state);
    let weak_dialog = dialog.downgrade();
    save.connect_clicked(move |save| {
        let Some(dialog) = weak_dialog.upgrade() else {
            return;
        };
        if let Err(e) = crate::tasks::validate_id(id.text().as_str()) {
            error.set_label(&e);
            return;
        }
        if title.text().trim().is_empty() || provider.text().trim().is_empty() {
            error.set_label("Enter a title and provider.");
            return;
        }
        let session = AgentSession {
            provider: provider.text().trim().into(),
            session_id: id.text().into(),
            title: title.text().trim().into(),
            directory: ws.root_dir.clone(),
            task_id: ws.task.borrow().as_ref().map(|t| t.id),
            workspace_id,
            updated: crate::tasks::now(),
        };
        save.set_sensitive(false);
        let (dialog, error, weak, save) =
            (dialog.clone(), error.clone(), weak.clone(), save.clone());
        background(
            move || crate::tasks::remember(session),
            move |result| {
                save.set_sensitive(true);
                match result {
                    Ok(()) => {
                        dialog.close();
                        if let Some(state) = weak.upgrade() {
                            history(&state);
                        }
                    }
                    Err(e) => error.set_label(&e),
                }
            },
        );
    });
    dialog.present();
}
