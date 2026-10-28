//! Picking, adding, renaming and deleting profiles.

use super::*;

/// A dropdown of all profiles that switches to the chosen one.
pub(in crate::app) fn profile_dropdown(state: &App) -> gtk::DropDown {
    let names = settings::list_profiles();
    let dropdown =
        gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    dropdown.set_tooltip_text(Some("Settings profile"));
    let current = state.shared.profile.borrow().clone();
    if let Some(i) = names.iter().position(|n| *n == current) {
        dropdown.set_selected(i as u32);
    }
    let weak = Rc::downgrade(state);
    dropdown.connect_selected_notify(move |dropdown| {
        if let (Some(state), Some(name)) = (weak.upgrade(), names.get(dropdown.selected() as usize))
        {
            switch_profile(&state, name);
        }
    });
    dropdown
}

pub(in crate::app) fn switch_profile(state: &App, name: &str) {
    if *state.shared.profile.borrow() == name {
        return;
    }
    if let Err(err) = settings::set_active_profile(name) {
        crate::log::write(&format!("failed to switch profile: {err}"));
    }
    state.shared.profile.replace(name.to_string());
    state.shared.settings.replace(Settings::load_profile(name));
    apply_settings(&state.shared);
    refresh_profile_ui(state);
}

/// Rebuilds the widgets that show profiles: the toolbar switcher and, if it
/// is open, the settings window. Deferred, since it's often triggered from
/// a signal of one of the widgets being replaced.
pub(in crate::app) fn refresh_profile_ui(state: &App) {
    let weak = Rc::downgrade(state);
    glib::idle_add_local_once(move || {
        let Some(state) = weak.upgrade() else { return };
        for window in state.shared.windows() {
            while let Some(child) = window.profile_slot.first_child() {
                window.profile_slot.remove(&child);
            }
            window.profile_slot.append(&profile_dropdown(&window));
        }
        if let Some(window) = state.shared.settings_window.upgrade() {
            window.set_child(Some(&settings_content(&state)));
        }
    });
}

/// The "Profile" rows at the top of the settings window: a switcher with
/// Duplicate/Delete, and the name with Rename. Default can be neither
/// deleted nor renamed, so there is always a profile to fall back to.
pub(in crate::app) fn profile_section(state: &App) -> (gtk::Widget, gtk::Widget) {
    let profile = state.shared.profile.borrow().clone();
    let is_default = profile == settings::DEFAULT_PROFILE;

    let duplicate = gtk::Button::with_label("Duplicate");
    let weak = Rc::downgrade(state);
    duplicate.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let name =
            settings::unique_profile_name(&format!("{} copy", state.shared.profile.borrow()));
        if let Err(err) = state.shared.settings.borrow().save_profile(&name) {
            return crate::log::write(&format!("failed to create profile: {err}"));
        }
        switch_profile(&state, &name);
    });
    let delete = gtk::Button::with_label("Delete");
    delete.set_sensitive(!is_default);
    let weak = Rc::downgrade(state);
    delete.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let current = state.shared.profile.borrow().clone();
        if let Err(err) = settings::delete_profile(&current) {
            return crate::log::write(&format!("failed to delete profile: {err}"));
        }
        switch_profile(&state, settings::DEFAULT_PROFILE);
    });
    let picker = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let dropdown = profile_dropdown(state);
    dropdown.set_hexpand(true);
    picker.append(&dropdown);
    picker.append(&duplicate);
    picker.append(&delete);

    let name = gtk::Entry::builder().text(&profile).build();
    let rename = gtk::Button::with_label("Rename");
    name.set_sensitive(!is_default);
    rename.set_sensitive(!is_default);
    let weak = Rc::downgrade(state);
    let entry_ref = name.downgrade();
    let do_rename = Rc::new(move || {
        let (Some(state), Some(entry)) = (weak.upgrade(), entry_ref.upgrade()) else {
            return;
        };
        let new = entry.text().trim().to_string();
        let old = state.shared.profile.borrow().clone();
        if new == old {
            return;
        }
        let result = if settings::valid_profile_name(&new) {
            settings::rename_profile(&old, &new)
        } else {
            Err(std::io::Error::other(
                "names can't be empty or contain \"/\"",
            ))
        };
        match result {
            Ok(()) => {
                state.shared.profile.replace(new);
                refresh_profile_ui(&state);
            }
            Err(err) => {
                entry.add_css_class("error");
                entry.set_tooltip_text(Some(&err.to_string()));
            }
        }
    });
    let on_click = do_rename.clone();
    rename.connect_clicked(move |_| on_click());
    name.connect_activate(move |_| do_rename());
    name.connect_changed(|entry| {
        entry.remove_css_class("error");
        entry.set_tooltip_text(None);
    });
    let naming = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    name.set_hexpand(true);
    naming.append(&name);
    naming.append(&rename);
    (picker.upcast(), naming.upcast())
}
