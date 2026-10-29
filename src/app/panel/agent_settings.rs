//! Settings ▸ Agents.

use super::*;

pub(in crate::app) fn settings_page(state: &App) -> gtk::Widget {
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

    page.append(&text("Agents", &["soi-section"]));
    page.append(&text(
        "Tabs running one of these programs appear in the agents panel (◨ in the sidebar). \
         Separate names with commas; leave empty for the defaults.",
        &["dim-label", "caption"],
    ));
    let list = gtk::Entry::builder()
        .text(
            state
                .shared
                .settings
                .borrow()
                .agents
                .clone()
                .unwrap_or_default(),
        )
        .placeholder_text(agents::DEFAULT_AGENTS.join(", "))
        .build();
    let weak = Rc::downgrade(state);
    list.connect_changed(move |entry| {
        if let Some(state) = weak.upgrade() {
            let value = entry.text().trim().to_string();
            update_settings(&state, |s| s.agents = (!value.is_empty()).then_some(value));
        }
    });
    page.append(&list);

    let notify = gtk::CheckButton::builder()
        .label("Desktop notifications when Soi isn't in front")
        .active(state.shared.settings.borrow().desktop_notifications())
        .build();
    let weak = Rc::downgrade(state);
    notify.connect_toggled(move |check| {
        if let Some(state) = weak.upgrade() {
            let on = check.is_active();
            update_settings(&state, |s| s.desktop_notifications = Some(on));
        }
    });
    page.append(&notify);
    let long = gtk::SpinButton::with_range(0.0, 86_400.0, 5.0);
    long.set_value(
        state
            .shared
            .settings
            .borrow()
            .long_command
            .unwrap_or(settings::DEFAULT_LONG_COMMAND) as f64,
    );
    let weak = Rc::downgrade(state);
    long.connect_value_changed(move |spin| {
        if let Some(state) = weak.upgrade() {
            let seconds = spin.value() as u32;
            update_settings(&state, |s| s.long_command = Some(seconds));
        }
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&gtk::Label::new(Some(
        "Say when a command ends that ran at least (seconds; 0: never)",
    )));
    row.append(&long);
    page.append(&row);
    page.append(&text(
        "When an agent finishes or asks something while you're in another app. \
         Click the notification to go to the agent.",
        &["dim-label", "caption"],
    ));

    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&text("Order in the panel", &["soi-section"]));
    page.append(&text(
        "The agents panel and the switcher list agents by state, top first. \
         Agents in the same state keep the sidebars' order.",
        &["dim-label", "caption"],
    ));
    page.append(&order_section(state));

    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&text("Agents to start", &["soi-section"]));
    page.append(&text(
        "What Start with brief and New task worktree offer. Add a tool more than once \
         to start it with different models, say OpenCode with Qwen and with GPT. The model \
         is passed as you write it: `opencode models` lists OpenCode's (provider/model), \
         Claude Code takes an alias like opus or a full name like claude-opus-5-5, \
         and `pi --list-models` lists Pi's. \
         To start one through a wrapper, like mate (mate claude …), put it under Start \
         through; its own options go in the arguments after --, e.g. -- --no-git. \
         Under each agent is the command it starts.",
        &["dim-label", "caption"],
    ));
    page.append(&presets_section());

    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&text("Exact states", &["soi-section"]));
    page.append(&text(
        "Without help, Soi sees whether an agent is busy or quiet. With these, \
         Claude Code, OpenCode and Pi tell Soi when they need your input and when they're done.",
        &["dim-label", "caption"],
    ));
    page.append(&integration_row(
        state,
        Integration {
            name: "Claude Code",
            description: format!(
                "Adds hooks to {} that run soi when Claude works, asks or finishes. \
                 Your other settings and hooks stay as they are.",
                tilde(&agents::claude_settings_path())
            ),
            preview: format!(
                "For SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Notification and \
                 Stop, a hook running:\n\n{}",
                agents::CLAUDE_COMMAND
            ),
            installed: agents::claude_hooks_installed,
            install: agents::install_claude_hooks,
        },
    ));
    page.append(&integration_row(
        state,
        Integration {
            name: "OpenCode",
            description: format!(
                "Writes a small plugin to {} that tells Soi what OpenCode is doing.",
                tilde(&agents::opencode_plugin_path())
            ),
            preview: format!(
                "Writes {}: a plugin that sends OpenCode's busy, permission, question, idle \
                 and error events to soi.",
                tilde(&agents::opencode_plugin_path())
            ),
            installed: agents::opencode_plugin_installed,
            install: agents::install_opencode_plugin,
        },
    ));
    page.append(&integration_row(
        state,
        Integration {
            name: "Pi",
            description: format!(
                "Writes a small extension to {} that tells Soi what Pi is doing.",
                tilde(&agents::pi_extension_path())
            ),
            preview: format!(
                "Writes {}: an extension that sends Pi's run, prompt and error events to Soi.",
                tilde(&agents::pi_extension_path())
            ),
            installed: agents::pi_extension_installed,
            install: agents::install_pi_extension,
        },
    ));
    page.append(&text(
        "Restart running agents afterwards. Outside Soi, the hooks, the plugin and the extension do nothing.",
        &["dim-label", "caption"],
    ));
    page.upcast()
}

/// One row per agent state, with buttons moving it up and down, and Reset.
fn order_section(state: &App) -> gtk::Widget {
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
    fill_order(&rows, state);
    let reset = gtk::Button::with_label("Reset");
    reset.set_halign(gtk::Align::Start);
    reset.set_tooltip_text(Some("What needs you first"));
    let (weak, rows_ref) = (Rc::downgrade(state), rows.downgrade());
    reset.connect_clicked(move |_| {
        let (Some(state), Some(rows)) = (weak.upgrade(), rows_ref.upgrade()) else {
            return;
        };
        update_settings(&state, |s| s.agent_order = None);
        fill_order(&rows, &state);
    });
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.append(&rows);
    column.append(&reset);
    column.upcast()
}

/// The states in the order the settings give; every move saves the whole
/// order and fills the rows again.
fn fill_order(rows: &gtk::Box, state: &App) {
    while let Some(row) = rows.first_child() {
        rows.remove(&row);
    }
    let order = AgentState::order(state.shared.settings.borrow().agent_order.as_deref());
    for (index, agent_state) in order.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label(agent_state.label())
                .xalign(0.0)
                .hexpand(true)
                .css_classes([agent_state.class()])
                .build(),
        );
        let moves = [
            ("go-up-symbolic", "Move up", index.checked_sub(1)),
            (
                "go-down-symbolic",
                "Move down",
                Some(index + 1).filter(|&below| below < order.len()),
            ),
        ];
        for (icon, tooltip, target) in moves {
            let button = gtk::Button::from_icon_name(icon);
            button.set_tooltip_text(Some(tooltip));
            button.set_sensitive(target.is_some());
            let mut moved = order.clone();
            if let Some(target) = target {
                moved.swap(index, target);
            }
            let (weak, rows_ref) = (Rc::downgrade(state), rows.downgrade());
            button.connect_clicked(move |_| {
                let (Some(state), Some(rows)) = (weak.upgrade(), rows_ref.upgrade()) else {
                    return;
                };
                let value = moved.iter().map(|s| s.key()).collect::<Vec<_>>().join(", ");
                update_settings(&state, |s| s.agent_order = Some(value));
                fill_order(&rows, &state);
            });
            row.append(&button);
        }
        rows.append(&row);
    }
}

pub(super) struct Integration {
    name: &'static str,
    description: String,
    /// What installing changes, shown before doing it.
    preview: String,
    installed: fn() -> bool,
    install: fn() -> Result<(), String>,
}

fn integration_row(state: &App, integration: Integration) -> gtk::Widget {
    let Integration {
        name,
        description,
        preview,
        installed,
        install,
    } = integration;
    let title = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let status = gtk::Label::new(None);
    status.add_css_class("caption");
    let button = gtk::Button::new();
    let update = {
        let (status, button) = (status.clone(), button.clone());
        move || {
            let done = installed();
            status.set_label(if done { "Installed" } else { "Not installed" });
            if done {
                status.add_css_class("soi-note");
            } else {
                status.remove_css_class("soi-note");
            }
            button.set_label(if done { "Reinstall" } else { "Install" });
        }
    };
    update();
    let update = Rc::new(update);
    let weak = Rc::downgrade(state);
    button.connect_clicked(move |button| {
        let Some(state) = weak.upgrade() else { return };
        let parent = button
            .root()
            .and_downcast::<gtk::Window>()
            .unwrap_or_else(|| state.window.clone().upcast());
        let confirm = gtk::AlertDialog::builder()
            .message(format!("Set up {name} for Soi?"))
            .detail(&preview)
            .buttons(["Cancel", "Set up"])
            .cancel_button(0)
            .default_button(1)
            .modal(true)
            .build();
        let update = update.clone();
        let window = parent.clone();
        confirm.choose(Some(&parent), gio::Cancellable::NONE, move |answer| {
            if answer != Ok(1) {
                return;
            }
            let dialog = match install() {
                Ok(()) => gtk::AlertDialog::builder()
                    .message(format!("{name} now reports to Soi"))
                    .detail("Agents started from now on show exact states. Restart running ones.")
                    .build(),
                Err(err) => gtk::AlertDialog::builder()
                    .message(format!("Couldn't set up {name}"))
                    .detail(err)
                    .build(),
            };
            dialog.show(Some(&window));
            update();
        });
    });
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    top.append(&title);
    top.append(&status);
    top.append(&button);
    let description = gtk::Label::builder()
        .label(&description)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.append(&top);
    row.append(&description);
    row.upcast()
}
