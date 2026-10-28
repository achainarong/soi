//! Settings ▸ Shortcuts.

use super::*;

/// Every shortcut, rebindable, followed by the fixed browse-mode keys.
pub(in crate::app) fn shortcuts_page(state: &App) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    page.set_margin_start(12);
    page.set_margin_end(12);
    let intro = gtk::Label::builder()
        .label(
            "Click a shortcut, then press the new keys. Backspace turns it off, Esc cancels. \
             Changed shortcuts are highlighted; ↺ puts one back.",
        )
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();
    let reset_all = gtk::Button::with_label("Restore all defaults");
    let any_changed = BINDINGS
        .iter()
        .any(|b| state.shared.keybindings.borrow().is_changed(b.id));
    reset_all.set_sensitive(any_changed);
    reset_all.set_tooltip_text(Some(if any_changed {
        "Put every shortcut back to how soi ships"
    } else {
        "All shortcuts are at their defaults"
    }));
    let weak = Rc::downgrade(state);
    reset_all.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            state.shared.keybindings.borrow_mut().reset_all();
            if let Err(err) = state.shared.keybindings.borrow().save() {
                crate::log::write(&format!("failed to save shortcuts: {err}"));
            }
            reload_shortcuts(&state.shared);
            refresh_profile_ui(&state);
        }
    });
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    top.append(&intro);
    top.append(&reset_all);
    page.append(&top);
    if let Some(note) = state.shared.shortcut_note.take() {
        page.append(
            &gtk::Label::builder()
                .label(note)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["soi-note"])
                .build(),
        );
    }

    let grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(12)
        .build();
    let mut row = 0;
    let mut section = "";
    let section_label = |text: &str| {
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .margin_top(12)
            .css_classes(["soi-section"])
            .build()
    };
    for binding in BINDINGS {
        if binding.section != section {
            section = binding.section;
            grid.attach(&section_label(section), 0, row, 3, 1);
            row += 1;
        }
        let label = gtk::Label::builder()
            .label(binding.label)
            .xalign(0.0)
            .hexpand(true)
            .build();
        grid.attach(&label, 0, row, 1, 1);
        // The shortcut buttons on the right edge, in line with "Restore all
        // defaults"; the reset button (only for changed ones) before them.
        grid.attach(&shortcut_button(state, binding.id), 2, row, 1, 1);
        let reset = gtk::Button::with_label("↺");
        reset.add_css_class("flat");
        let default = keybindings::binding(binding.id).map_or("", |b| b.default_accel());
        reset.set_tooltip_text(Some(&format!(
            "Back to the default: {}",
            accel_label(default)
        )));
        reset.set_visible(state.shared.keybindings.borrow().is_changed(binding.id));
        let weak = Rc::downgrade(state);
        let id = binding.id;
        reset.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                rebind(&state, id, default);
            }
        });
        grid.attach(&reset, 1, row, 1, 1);
        row += 1;
    }
    page.append(&grid);

    // The fixed keys of browse mode, as a cheatsheet.
    page.append(&section_label("Browse mode (fixed keys, like vim)"));
    let cheats = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(12)
        .build();
    let browse_keys = [
        ("h j k l, arrows", "Move"),
        ("w  b  e", "Next word, previous word, end of word"),
        ("0  ^  $", "Start of line, first character, end of line"),
        ("gg  G", "Top, bottom"),
        ("H  M  L", "Top, middle, bottom of the screen"),
        ("Ctrl+u  Ctrl+d", "Half a page up, down"),
        ("Ctrl+b  Ctrl+f", "A page up, down"),
        ("/  ?", "Search forward, backward"),
        (
            "n  N  *",
            "Next match, previous match, word under the cursor",
        ),
        ("v  V", "Select characters, whole lines"),
        ("y  yy", "Copy the selection, copy the line"),
        ("[[  ]]", "Previous, next command"),
        ("o", "Open the link under the cursor"),
        ("q  Esc", "Leave browse mode"),
    ];
    for (i, (keys, what)) in browse_keys.iter().enumerate() {
        let keys = gtk::Label::builder()
            .label(*keys)
            .xalign(0.0)
            .css_classes(["monospace"])
            .build();
        let what = gtk::Label::builder()
            .label(*what)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        cheats.attach(&keys, 0, i as i32, 1, 1);
        cheats.attach(&what, 1, i as i32, 1, 1);
    }
    page.append(&cheats);
    page.upcast()
}

/// A button showing an action's shortcut; click it and press keys to
/// rebind.
pub(in crate::app) fn shortcut_button(state: &App, id: &'static str) -> gtk::Button {
    let current = state.shared.keybindings.borrow().get(id).to_string();
    let button = gtk::Button::with_label(&accel_label(&current));
    if state.shared.keybindings.borrow().is_changed(id) {
        // Shortcuts the user changed stand out from the defaults.
        button.add_css_class("changed");
    }
    button.set_width_request(190);
    let capturing = Rc::new(Cell::new(false));
    let stop = {
        let (capturing, current) = (capturing.clone(), current.clone());
        move |button: &gtk::Button| {
            capturing.set(false);
            button.remove_css_class("capturing");
            button.set_label(&accel_label(&current));
        }
    };

    let flag = capturing.clone();
    button.connect_clicked(move |button| {
        flag.set(true);
        button.add_css_class("capturing");
        button.set_label("Press keys…");
        button.grab_focus();
    });

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(state);
    let button_ref = button.downgrade();
    let flag = capturing.clone();
    let stop_on_key = stop.clone();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let (Some(state), Some(button)) = (weak.upgrade(), button_ref.upgrade()) else {
            return glib::Propagation::Proceed;
        };
        if !flag.get() {
            return glib::Propagation::Proceed;
        }
        let mods = modifiers & gtk::accelerator_get_default_mod_mask();
        let is_modifier = key.name().is_some_and(|name| {
            [
                "Shift",
                "Control",
                "Alt",
                "Super",
                "Meta",
                "Hyper",
                "ISO_Level3",
            ]
            .iter()
            .any(|m| name.starts_with(m))
        });
        if is_modifier {
            return glib::Propagation::Stop;
        }
        // Shift+Tab arrives as ISO_Left_Tab; store it as Tab.
        let key = if key == gdk::Key::ISO_Left_Tab {
            gdk::Key::Tab
        } else {
            key.to_lower()
        };
        let function_key = key.name().is_some_and(|name| {
            name.len() <= 3
                && name.starts_with('F')
                && name[1..].chars().all(|c| c.is_ascii_digit())
        });
        let plain = (mods - gdk::ModifierType::SHIFT_MASK).is_empty();
        match key {
            gdk::Key::Escape if mods.is_empty() => stop_on_key(&button),
            gdk::Key::BackSpace if mods.is_empty() => rebind(&state, id, ""),
            _ if plain && !function_key => {
                button.set_label("Add Ctrl, Alt or Super…");
            }
            _ => rebind(&state, id, &gtk::accelerator_name(key, mods)),
        }
        glib::Propagation::Stop
    });
    button.add_controller(keys);

    let focus = gtk::EventControllerFocus::new();
    let button_ref = button.downgrade();
    focus.connect_leave(move |_| {
        if let Some(button) = button_ref.upgrade()
            && capturing.get()
        {
            stop(&button);
        }
    });
    button.add_controller(focus);
    button
}
