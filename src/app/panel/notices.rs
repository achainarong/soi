//! Telling about agents that need you: notices under the panel and desktop
//! notifications.

use super::*;

/// Tells about an agent that needs you, in every window.
pub(super) fn announce(shared: &Rc<Shared>, tab: &Rc<Tab>, agent: &Agent) {
    let Some((owner, workspace, _)) = locate(shared, tab) else {
        return;
    };
    let workspace = owner
        .workspaces
        .borrow()
        .get(&workspace)
        .map(|ws| ws.name.borrow().clone())
        .unwrap_or_default();
    let what = match agent.state {
        AgentState::Input => "needs your input",
        AgentState::Done => "is done",
        AgentState::Idle => "stopped",
        AgentState::Exited => "exited",
        AgentState::Working => return,
    };
    let program = agent.program.as_deref().unwrap_or("agent");
    let (title, body) = desktop_text(&workspace, program, what, &agent.message);
    notify_desktop(shared, tab.id, &title, &body, Picture::State(agent.state));
    let notice = Notice {
        workspace,
        what: format!("{program} {what}"),
        state: Some(agent.state),
        message: agent.message.clone(),
    };
    add_notice(shared, tab.id, &notice);
}

/// What a notification under the agents says.
pub(super) struct Notice {
    pub(super) workspace: String,
    /// "claude needs your input".
    pub(super) what: String,
    /// Colors `what` as the panel colors this state.
    pub(super) state: Option<AgentState>,
    pub(super) message: String,
}

/// Adds a notification under the agents in every window, in place of the
/// tab's earlier one; it stays until dismissed or the tab is looked at.
pub(super) fn add_notice(shared: &Rc<Shared>, tab: Uuid, notice: &Notice) {
    remove_notice(shared, tab);
    for state in shared.windows() {
        let ui = &state.agents;
        ui.notices.prepend(&notice_row(shared, tab, notice));
        while let Some(old) = ui.notices.row_at_index(MAX_NOTICES) {
            ui.notices.remove(&old);
        }
        if !ui.notices_box.is_visible() {
            ui.notices_box.set_visible(true);
            size_notices(&ui.split);
        }
    }
}

fn notice_row(shared: &Rc<Shared>, tab: Uuid, notice: &Notice) -> gtk::ListBoxRow {
    let workspace = gtk::Label::builder()
        .label(&notice.workspace)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["ws-name"])
        .build();
    let dismiss = gtk::Button::from_icon_name("window-close-symbolic");
    dismiss.add_css_class("ws-close");
    dismiss.set_valign(gtk::Align::Start);
    dismiss.set_tooltip_text(Some("Dismiss"));
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    top.append(&workspace);
    top.append(&dismiss);
    let what = gtk::Label::builder()
        .label(&notice.what)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["caption"])
        .build();
    set_state_class(&what, notice.state);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.add_css_class("agents-notice");
    content.append(&top);
    content.append(&what);
    if !notice.message.is_empty() {
        content.append(
            &gtk::Label::builder()
                .label(&notice.message)
                .tooltip_text(&notice.message)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .lines(2)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    // Clicking it goes to the agent (see `connect`).
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .name(tab.to_string())
        .tooltip_text("Go to this agent")
        .build();
    let weak = Rc::downgrade(shared);
    dismiss.connect_clicked(move |_| {
        if let Some(shared) = weak.upgrade() {
            remove_notice(&shared, tab);
        }
    });
    row
}

/// Removes the tab's notification from every window.
pub(super) fn remove_notice(shared: &Shared, tab: Uuid) {
    let name = tab.to_string();
    for state in shared.windows() {
        let ui = &state.agents;
        let rows: Vec<_> = (0..).map_while(|i| ui.notices.row_at_index(i)).collect();
        for row in rows.iter().filter(|row| row.widget_name() == name.as_str()) {
            ui.notices.remove(row);
        }
        if ui.notices.row_at_index(0).is_none() {
            ui.notices_box.set_visible(false);
        }
    }
}

/// Gives the notifications their share of the panel's height.
pub(super) fn size_notices(split: &gtk::Paned) {
    let height = split.height();
    if height > 0 {
        split.set_position((height as f64 * (1.0 - NOTICES_SHARE)) as i32);
    }
}

/// Tells the desktop about an agent that needs you, when no soi window is
/// in front (otherwise the notifications under the agents do). Clicking it goes to the agent.
pub(super) fn notify_desktop(
    shared: &Shared,
    tab: Uuid,
    title: &str,
    body: &str,
    picture: Picture,
) {
    let windows = shared.windows();
    if !shared.settings.borrow().desktop_notifications()
        || windows.iter().any(|state| state.window.is_active())
    {
        return;
    }
    let Some(app) = gio::Application::default() else {
        return;
    };
    let notification = gio::Notification::new(title);
    if !body.is_empty() {
        notification.set_body(Some(body));
    }
    // Without one, desktops fill the picture with whatever they find. A
    // file, not a theme name: macOS has no icon themes.
    if let Some(path) = state_icon(picture) {
        notification.set_icon(&gio::FileIcon::new(&gio::File::for_path(path)));
    }
    if picture == Picture::State(AgentState::Input) {
        notification.set_priority(gio::NotificationPriority::High);
    }
    notification.set_default_action_and_target_value(
        "app.focus-agent",
        Some(&tab.to_string().to_variant()),
    );
    app.send_notification(Some(&notification_id(tab)), &notification);
}

/// "Claude Code needs your input", and below it the workspace and what the
/// agent said: "randy · Which file should I edit?".
pub(super) fn desktop_text(
    workspace: &str,
    program: &str,
    what: &str,
    message: &str,
) -> (String, String) {
    let title = format!("{} {what}", agents::display_name(program));
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = match (workspace.is_empty(), message.is_empty()) {
        (false, false) => format!("{workspace} · {message}"),
        (false, true) => format!("In {workspace}"),
        (true, _) => message,
    };
    (title, body)
}

/// What a desktop notification shows: an agent's state, or a command that
/// failed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Picture {
    State(AgentState),
    Failed,
}

/// A picture for the state: a colored disc with a white mark, drawn once
/// into the cache directory.
fn state_icon(picture: Picture) -> Option<PathBuf> {
    let name = match picture {
        Picture::State(state) => state.class(),
        Picture::Failed => "command-failed",
    };
    // Bump the suffix when the drawing changes, so old files aren't reused.
    let path = glib::user_cache_dir()
        .join("soi")
        .join(format!("{name}-1.png"));
    if !path.exists() {
        fs::create_dir_all(path.parent()?).ok()?;
        draw_state_icon(picture)?.save_to_png(&path).ok()?;
    }
    Some(path)
}

fn draw_state_icon(picture: Picture) -> Option<gdk::Texture> {
    const SIZE: i32 = 128;
    let (r, g, b) = match picture {
        Picture::State(AgentState::Input) | Picture::Failed => (0.89, 0.30, 0.27),
        Picture::State(AgentState::Done) => (0.26, 0.69, 0.40),
        Picture::State(AgentState::Working) => (0.25, 0.52, 0.89),
        Picture::State(AgentState::Idle | AgentState::Exited) => (0.47, 0.49, 0.53),
    };
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, SIZE, SIZE).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        let c = SIZE as f64 / 2.0;
        cr.arc(c, c, c - 4.0, 0.0, std::f64::consts::TAU);
        cr.set_source_rgb(r, g, b);
        cr.fill().ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.set_line_width(12.0);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        match picture {
            // A check mark.
            Picture::State(AgentState::Done) => {
                cr.move_to(38.0, 66.0);
                cr.line_to(56.0, 84.0);
                cr.line_to(90.0, 46.0);
                cr.stroke().ok()?;
            }
            // Three dots: it waits for you.
            Picture::State(AgentState::Input | AgentState::Working) => {
                for x in [40.0, 64.0, 88.0] {
                    cr.arc(x, c, 9.0, 0.0, std::f64::consts::TAU);
                    cr.fill().ok()?;
                }
            }
            // A cross.
            Picture::State(AgentState::Exited) | Picture::Failed => {
                cr.move_to(46.0, 46.0);
                cr.line_to(82.0, 82.0);
                cr.move_to(82.0, 46.0);
                cr.line_to(46.0, 82.0);
                cr.stroke().ok()?;
            }
            // A pause.
            Picture::State(AgentState::Idle) => {
                for x in [52.0, 76.0] {
                    cr.move_to(x, 44.0);
                    cr.line_to(x, 84.0);
                }
                cr.stroke().ok()?;
            }
        }
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let pixels = surface.data().ok()?.to_vec();
    // Cairo's ARGB32, native-endian: B, G, R, A in memory on little-endian.
    let format = if cfg!(target_endian = "little") {
        gdk::MemoryFormat::B8g8r8a8Premultiplied
    } else {
        gdk::MemoryFormat::A8r8g8b8Premultiplied
    };
    Some(
        gdk::MemoryTexture::new(SIZE, SIZE, format, &glib::Bytes::from_owned(pixels), stride)
            .upcast(),
    )
}

/// Removes an agent's desktop notification: you've seen it.
pub(super) fn withdraw_notification(tab: Uuid) {
    if let Some(app) = gio::Application::default() {
        app.withdraw_notification(&notification_id(tab));
    }
}

fn notification_id(tab: Uuid) -> String {
    format!("agent-{tab}")
}

/// `app.focus-agent`, which desktop notifications run when clicked.
pub(in crate::app) fn install_app_action(app: &gtk::Application, shared: &Rc<Shared>) {
    let action = gio::SimpleAction::new("focus-agent", Some(glib::VariantTy::STRING));
    let weak = Rc::downgrade(shared);
    action.connect_activate(move |_, target| {
        let id = target
            .and_then(|v| v.get::<String>())
            .and_then(|id| Uuid::parse_str(&id).ok());
        if let (Some(shared), Some(id)) = (weak.upgrade(), id) {
            focus_tab(&shared, id);
        }
    });
    app.add_action(&action);
}
