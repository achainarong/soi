//! Task workspace UI, context editing and agent session bookmarks.
use super::*;
use crate::presets::Preset;
use crate::tasks::{AgentSession, History, TaskContext};
use std::time::{Duration, Instant};

mod dialogs;
mod history;
mod launch;

pub(super) use dialogs::*;
pub(super) use history::*;
pub(super) use launch::*;

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

/// How often git is asked about a folder shown, and about one in a
/// collapsed group or a window not in use.
const GIT_SHOWN_EVERY: Duration = Duration::from_secs(5);
const GIT_HIDDEN_EVERY: Duration = Duration::from_secs(60);

/// Keeps `label` showing the branch, whether it has changes, and the folder
/// `directory` returns. While shown in the window in use, the folder is
/// checked every second, so following a terminal's `cd` or focus shows up
/// quickly; git is asked again when it changes, and otherwise every five
/// seconds. Hidden, both wait a minute; shown again, it catches up.
pub(super) fn watch_git(label: &gtk::Label, directory: impl Fn() -> PathBuf + 'static) {
    let weak = label.downgrade();
    let busy = Rc::new(Cell::new(false));
    let shown: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let asked: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let update = move || {
        let Some(label) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let in_use = label.is_mapped()
            && label
                .root()
                .and_downcast::<gtk::Window>()
                .is_some_and(|window| window.is_active());
        let every = if in_use {
            GIT_SHOWN_EVERY
        } else {
            GIT_HIDDEN_EVERY
        };
        let due = asked.get().is_none_or(|at| at.elapsed() >= every);
        if !(in_use || due) || busy.get() {
            return glib::ControlFlow::Continue;
        }
        let directory = directory();
        let moved = shown.borrow().as_ref() != Some(&directory);
        if !(moved || due) {
            return glib::ControlFlow::Continue;
        }
        busy.set(true);
        asked.set(Some(Instant::now()));
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
    glib::timeout_add_local(Duration::from_secs(1), update);
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
    let requests = requests::Picker::new(&root, &[], &[], &[], true, chosen.as_deref());
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
        let (skills, commands, mcps, collection) = (requests.skills(), requests.commands(), requests.mcps(), requests.collection());
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
            task.mcps = mcps;
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
                    // The repository's own file, maybe a symlink out of it.
                    Err(crate::plan::CreateError::Exists) if path.starts_with(&destination) && crate::plan::leaves(&path, &destination) => {
                        Some(format!("{} points outside the worktree, so no plan was linked.", path.display()))
                    }
                    Err(crate::plan::CreateError::Exists) => {
                        task.plan = Some(path.clone());
                        Some(format!("{} already existed. It was linked, not overwritten.", path.display()))
                    }
                    Err(crate::plan::CreateError::Failed(e)) => Some(format!("The plan was not created: {e}")),
                },
                1 if path.is_file() && path.starts_with(&destination) && crate::plan::leaves(&path, &destination) => {
                    Some(format!("{} points outside the worktree, so no plan was linked.", path.display()))
                }
                1 if path.is_file() => { task.plan = Some(path); None }
                1 => Some(format!("{} does not exist, so no plan was linked.", path.display())),
                _ => None,
            };
            let command = preset.as_ref().map(|p| agent_command(p, with_brief.then(|| task.brief(&destination)), &task, &agent_path));
            let mut events = vec![format!("Created on branch {branch}, worktree {}", destination.display())];
            if !task.skills.is_empty() || !task.commands.is_empty() || !task.mcps.is_empty() {
                let from = collection.map(|name| format!(" (from collection {name})")).unwrap_or_default();
                events.push(format!(
                    "Requested skills: {}; commands: {}; MCP servers: {}{from}",
                    crate::tasks::skill_list(&task.skills),
                    crate::tasks::command_list(&task.commands),
                    crate::tasks::skill_list(&task.mcps)
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
                        let workspace = ws.id;
                        let shell = ws.tabs.first().map(|tab| tab.id);
                        insert_workspace(&state, ws, group);
                        if let (Some(command), Some(pane)) = (command, current_pane(&state)) {
                            match launch(&state, &pane, &directory, command) {
                                // The agent's tab comes first: the new
                                // workspace's shell, still unused, goes.
                                Ok(()) => if let Some(shell) = shell {
                                    close_unused_tab(&state, workspace, shell);
                                },
                                Err(error) => gtk::AlertDialog::builder().message("Worktree created, but agent could not start")
                                    .detail(error).build().show(Some(&state.window)),
                            }
                        }
                        run_worktree_commands(&state, &directory);
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

/// Closes a main tab nobody used yet, without keeping its output or
/// offering to reopen it.
fn close_unused_tab(state: &App, workspace: Uuid, id: Uuid) {
    if let Some((root, _)) = take_main_tab(state, workspace, id) {
        forget_tree(state, &root);
    }
    save_all(&state.shared);
}

/// Opens a tab in the new worktree for each of the settings' worktree
/// commands and types the command into its shell. The tab in front (the
/// agent's, when one started) stays there.
fn run_worktree_commands(state: &App, directory: &Path) {
    let commands = state.shared.settings.borrow().worktree_commands.clone();
    let (Some(workspace), Some(ws)) = (state.active.get(), active_workspace(state)) else {
        return;
    };
    if commands.is_empty() {
        return;
    }
    let front = ws.current_tab().id;
    for command in commands {
        let Some(pane) = new_main_tab(state, Some(directory.to_path_buf())) else {
            return;
        };
        let tab = state
            .panes
            .borrow()
            .get(&pane)
            .and_then(|pane| pane.tabs.borrow().first().cloned());
        // The shell reads it once it starts.
        if let Some(tab) = tab {
            tab.terminal.type_ahead(&format!("{command}\r"));
        }
    }
    select_main_tab_id(state, workspace, front);
    if let Some(pane) = current_pane(state) {
        let index = state
            .panes
            .borrow()
            .get(&pane)
            .map_or(0, |p| p.active.get());
        activate_tab(state, &pane, index, true);
    }
    // Their tabs aren't shown, so they start once the one in front has
    // its size, as at startup.
    let weak = Rc::downgrade(&state.shared);
    glib::timeout_add_local_once(UNSHOWN_START_DELAY, move || {
        if let Some(shared) = weak.upgrade() {
            start_unshown_terminals(&shared);
        }
    });
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
