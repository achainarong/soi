//! Small dialogs and form fields the task windows share.

use super::*;

/// Utility windows close with Escape, except while an operation has disabled
/// closing. Bubble-phase handling lets native controls dismiss their popovers
/// first; search entries also expose Escape through their stop-search signal.
pub(in crate::app) fn close_on_escape(window: &gtk::Window) {
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

pub(super) fn close_if_allowed(window: &gtk::Window) {
    if window.is_deletable() {
        window.close();
    }
}

pub(super) fn dialog(
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

pub(in crate::app) fn field(content: &gtk::Box, label: &str, value: &str) -> gtk::Entry {
    content.append(&gtk::Label::builder().label(label).xalign(0.0).build());
    let entry = gtk::Entry::builder().text(value).hexpand(true).build();
    content.append(&entry);
    entry
}

pub(in crate::app) fn note(content: &gtk::Box, text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    content.append(&label);
    label
}

pub(in crate::app) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(in crate::app) fn optional_path(text: &str, root: &Path) -> Option<PathBuf> {
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
