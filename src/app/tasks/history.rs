//! Agent sessions: recording them, the history window, resuming and
//! bookmarking.

use super::*;

pub(in crate::app) fn task_for_tab(shared: &Shared, tab: &Tab) -> Option<TaskContext> {
    let owner = window_of(shared, tab.terminal.widget())?;
    let (_, ws) = workspace_of(&owner, tab.terminal.widget())?;
    ws.task.borrow().clone()
}

pub(in crate::app) fn record_report(
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

pub(in crate::app) fn history(state: &App) {
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

pub(in crate::app) fn resume_session(state: &App, session: &AgentSession) -> Result<(), String> {
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
pub(in crate::app) fn resume_agent(state: &App, tab: &Tab, session: &SessionMetadata) {
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

pub(in crate::app) fn bookmark(state: &App) {
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
