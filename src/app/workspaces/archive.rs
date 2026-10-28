//! The archive: workspaces put away when done with them, to bring back.

use super::*;

/// Lists the archived workspaces, each with a button to bring it back
/// into this window.
pub(in crate::app) fn archived(state: &App) {
    let window = gtk::Window::builder()
        .transient_for(&state.window)
        .title("Archived workspaces")
        .default_width(560)
        .default_height(420)
        .build();
    window.add_css_class("soi-tools");
    tasks::close_on_escape(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let message = tasks::note(&content, "");
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_show_separators(true);
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .build();
    content.append(&scrolled);
    window.set_child(Some(&content));

    let archive: Vec<Workspace> = state.shared.session.borrow().archived.clone();
    message.set_label(match archive.len() {
        0 => "Nothing archived. Archive a workspace from its menu in the sidebar.",
        _ => "Archived workspaces keep their folders, worktrees and saved output.",
    });
    // Newest first.
    for ws in archive.iter().rev() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(8);
        row.set_margin_end(8);
        let mut text = format!("{}\n{}", ws.name, tilde(&ws.root_dir));
        if let Some(branch) = ws.task.as_ref().and_then(|task| task.branch.as_ref()) {
            text.push_str(&format!(" · {branch}"));
        }
        let label = gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .build();
        row.append(&label);
        let restore = gtk::Button::with_label("Restore");
        restore.set_valign(gtk::Align::Center);
        if !ws.root_dir.is_dir() {
            restore.set_tooltip_text(Some("Its folder is gone; tabs open in your home folder"));
        }
        let (weak, id, row_ref, list_ref) = (
            Rc::downgrade(state),
            ws.id,
            row.downgrade(),
            list.downgrade(),
        );
        restore.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                unarchive_workspace(&state, id);
            }
            if let (Some(row), Some(list)) = (row_ref.upgrade(), list_ref.upgrade())
                && let Some(parent) = row.parent()
            {
                list.remove(&parent);
            }
        });
        row.append(&restore);
        list.append(&row);
    }
    window.present();
}
