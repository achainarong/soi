//! A read-only workspace diff view. No index/worktree mutation actions.
use super::*;

pub(super) fn open(state: &App) {
    if let Some(ws) = active_workspace(state) {
        open_for(state, &ws);
    }
}

pub(super) fn open_for(state: &App, ws: &WorkspaceUi) {
    let directory = git_directory(state, ws);
    let window = gtk::Window::builder()
        .transient_for(&state.window)
        .title(format!("Changes — {}", ws.name.borrow()))
        .default_width(960)
        .default_height(700)
        .build();
    window.add_css_class("soi-tools");
    tasks::close_on_escape(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let refresh = gtk::Button::with_label("Refresh workspace changes");
    content.append(&refresh);
    content.append(
        &gtk::Label::builder()
            .label("All uncommitted changes in this worktree, by you or any agent; Soi does not attribute them to a particular agent.")
            .wrap(true)
            .xalign(0.0)
            .build(),
    );
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .vexpand(true)
        .build();
    let colors = state.shared.theme.borrow().ansi_colors();
    view.buffer()
        .create_tag(Some("addition"), &[("foreground-rgba", &colors[2])]);
    view.buffer()
        .create_tag(Some("deletion"), &[("foreground-rgba", &colors[1])]);
    view.buffer()
        .create_tag(Some("hunk"), &[("foreground-rgba", &colors[4])]);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();
    content.append(&scroll);
    window.set_child(Some(&content));
    let load: Rc<dyn Fn()> = {
        let view = view.downgrade();
        let refresh = refresh.downgrade();
        Rc::new(move || {
            let (Some(view), Some(refresh)) = (view.upgrade(), refresh.upgrade()) else {
                return;
            };
            refresh.set_sensitive(false);
            view.buffer().set_text("Reading workspace changes…");
            let (view, refresh, directory) =
                (view.downgrade(), refresh.downgrade(), directory.clone());
            tasks::background(
                move || crate::git::changes(&directory),
                move |result| {
                    let (Some(view), Some(refresh)) = (view.upgrade(), refresh.upgrade()) else {
                        return;
                    };
                    refresh.set_sensitive(true);
                    let text = result.unwrap_or_else(|e| e);
                    let buffer = view.buffer();
                    buffer.set_text(&text);
                    for (line, text) in text.lines().enumerate() {
                        let tag = if text.starts_with('+') {
                            Some("addition")
                        } else if text.starts_with('-') {
                            Some("deletion")
                        } else if text.starts_with("@@") {
                            Some("hunk")
                        } else {
                            None
                        };
                        if let (Some(tag), Some(start)) = (tag, buffer.iter_at_line(line as i32)) {
                            let mut end = start;
                            end.forward_to_line_end();
                            buffer.apply_tag_by_name(tag, &start, &end);
                        }
                    }
                },
            );
        })
    };
    let clicked = load.clone();
    refresh.connect_clicked(move |_| clicked());
    window.present();
    load();
}
