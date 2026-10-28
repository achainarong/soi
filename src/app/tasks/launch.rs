//! Starting an agent in a tab: its command, PATH and model.

use super::*;

pub(in crate::app) fn launch(
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
pub(in crate::app) fn agent_path(state: &App) -> String {
    let shell = state.shared.settings.borrow().shell_command();
    crate::agents::agent_path(shell.first().map_or("/bin/sh", String::as_str))
}

/// Where to change the model for one start; the preset's is filled in.
pub(in crate::app) fn model_entry() -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text("Model (optional; for this start only)")
        .tooltip_text("Passed to the agent as --model; the preset's is filled in")
        .hexpand(true)
        .build()
}

/// `preset`, with the model typed for this start.
pub(in crate::app) fn with_model(preset: &Preset, model: &gtk::Entry) -> Preset {
    Preset {
        model: model.text().trim().to_string(),
        ..preset.clone()
    }
}

/// A history entry for starting an agent.
pub(in crate::app) fn started(preset: &Preset, with_brief: bool) -> String {
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
pub(in crate::app) fn agent_command(
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
pub(in crate::app) struct Command {
    pub(super) argv: Vec<String>,
    pub(super) env: Vec<(String, String)>,
    /// The agent it runs, when that may not be its program: one started
    /// through a wrapper like `mate`.
    pub(super) agent: Option<String>,
}
