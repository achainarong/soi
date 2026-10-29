//! Settings ▸ Tasks: what New task worktree and Task details offer to start,
//! and the commands a new worktree opens.

use super::*;

pub(in crate::app) fn tasks_page(state: &App) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    page.set_margin_start(12);
    page.set_margin_end(12);
    let text = |text: &str, classes: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .css_classes(classes.to_vec())
            .build()
    };

    page.append(&text("Agents to start", &["soi-section"]));
    page.append(&text(
        "What Start with brief and New task worktree offer. Add a tool more than once \
         to start it with different models, say OpenCode with Qwen and with GPT. The model \
         is passed as you write it: `opencode models` lists OpenCode's (provider/model), \
         Claude Code takes an alias like opus or a full name like claude-opus-5-5, \
         and `pi --list-models` lists Pi's. \
         To start one through a wrapper, like mate (mate claude …), put it under Start \
         through; its own options go in the arguments after --, e.g. -- --no-git. \
         Under each agent is the command it starts. Shared by all profiles.",
        &["dim-label", "caption"],
    ));
    page.append(&presets_section());

    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&crate::app::requests::collections_page());

    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&text("Worktree commands", &["soi-section"]));
    page.append(&text(
        "Each opens a tab of its own in a new task worktree, e.g. \"nvim .\". \
         It runs in your shell, which stays when it ends. None by default.",
        &["dim-label", "caption"],
    ));
    let commands = state.shared.settings.borrow().worktree_commands.clone();
    page.append(&worktree_commands(state, &commands));
    page.upcast()
}
