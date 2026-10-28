//! The settings of a profile: colors, font, shell and the rest.

use super::*;

/// The General page: the active profile's settings.
pub(in crate::app) fn settings_form(state: &App) -> gtk::Widget {
    let current = state.shared.settings.borrow().clone();
    let profile = state.shared.profile.borrow().clone();
    let detected = TerminalTheme::detect();

    let grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let next_row = Cell::new(0);
    let add_row = |label: &str, widget: &gtk::Widget, hint: &str| {
        let row = next_row.get();
        let label = gtk::Label::builder().label(label).xalign(0.0).build();
        widget.set_hexpand(true);
        grid.attach(&label, 0, row, 1, 1);
        grid.attach(widget, 1, row, 1, 1);
        next_row.set(row + 1);
        if !hint.is_empty() {
            let hint = gtk::Label::builder()
                .label(hint)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label", "caption"])
                .build();
            grid.attach(&hint, 1, row + 1, 1, 1);
            next_row.set(row + 2);
        }
    };

    let (picker, naming) = profile_section(state);
    add_row(
        "Profile",
        &picker,
        "Everything below belongs to this profile. You can also switch from the sidebar.",
    );
    add_row("Profile name", &naming, "");
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    grid.attach(&separator, 0, next_row.get(), 2, 1);
    next_row.set(next_row.get() + 1);

    // Color scheme
    let schemes = theme::available_schemes();
    let mut labels = vec!["Automatic (from your terminal)".to_string()];
    labels.extend(schemes.iter().map(|c| c.label.clone()));
    let scheme =
        gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    scheme.set_enable_search(true);
    scheme.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
    let selected = current
        .theme
        .as_ref()
        .and_then(|id| schemes.iter().position(|c| &c.id == id))
        .map_or(0, |i| i + 1);
    scheme.set_selected(selected as u32);
    let ids: Vec<String> = schemes.into_iter().map(|c| c.id).collect();
    // By label, as the search filters the list and so changes positions.
    let by_label: Rc<std::collections::HashMap<String, Option<String>>> = Rc::new(
        labels
            .iter()
            .cloned()
            .zip(std::iter::once(None).chain(ids.iter().cloned().map(Some)))
            .collect(),
    );
    let chosen = move |dropdown: &gtk::DropDown| {
        (dropdown.selected() as usize)
            .checked_sub(1)
            .and_then(|i| ids.get(i).cloned())
    };
    let weak = Rc::downgrade(state);
    let shown = Rc::new(RefCell::new(current.theme.clone()));
    // Shows a scheme without saving it; `chosen` is what's saved.
    let show: Rc<dyn Fn(Option<String>)> = {
        let shown = shown.clone();
        Rc::new(move |id| {
            if let Some(state) = weak.upgrade()
                && *shown.borrow() != id
            {
                shown.replace(id.clone());
                state.shared.settings.borrow_mut().theme = id;
                apply_settings(&state.shared);
            }
        })
    };
    let weak = Rc::downgrade(state);
    let chosen = Rc::new(chosen);
    let choose = chosen.clone();
    scheme.connect_selected_notify(move |dropdown| {
        if let Some(state) = weak.upgrade() {
            let id = choose(dropdown);
            shown.replace(id.clone());
            update_settings(&state, |s| s.theme = id);
        }
    });
    // Hovering a scheme in the list previews it, until the pointer leaves
    // the list or it closes. Only once the pointer rests: sweeping over the
    // list doesn't recolor everything at each row.
    let restore: Rc<dyn Fn()> = {
        let (dropdown, show) = (scheme.downgrade(), show.clone());
        Rc::new(move || {
            if let Some(dropdown) = dropdown.upgrade() {
                show(chosen(&dropdown));
            }
        })
    };
    // The scheme under the pointer, and a count that cancels older waits.
    let hovered: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let waits = Rc::new(Cell::new(0u64));
    let settle: Rc<dyn Fn()> = {
        let (hovered, waits, restore) = (hovered.clone(), waits.clone(), restore.clone());
        Rc::new(move || {
            waits.set(waits.get() + 1);
            let wait = waits.get();
            let (hovered, waits, show, restore) = (
                hovered.clone(),
                waits.clone(),
                show.clone(),
                restore.clone(),
            );
            glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                if waits.get() == wait {
                    match hovered.borrow().clone() {
                        Some(id) => show(id),
                        None => restore(),
                    }
                }
            });
        })
    };
    let factory = gtk::SignalListItemFactory::new();
    let (setup_hovered, setup_settle) = (hovered.clone(), settle.clone());
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::builder().xalign(0.0).build();
        let motion = gtk::EventControllerMotion::new();
        let scheme_of = {
            let (by_label, label) = (by_label.clone(), label.downgrade());
            move || {
                let label = label.upgrade()?;
                by_label.get(label.label().as_str()).cloned()
            }
        };
        let (hovered, settle, scheme) = (
            setup_hovered.clone(),
            setup_settle.clone(),
            scheme_of.clone(),
        );
        motion.connect_enter(move |_, _, _| {
            if let Some(id) = scheme() {
                hovered.replace(Some(id));
                settle();
            }
        });
        let (hovered, settle) = (setup_hovered.clone(), setup_settle.clone());
        motion.connect_leave(move |_| {
            // Unless the next row was entered first.
            let left = scheme_of();
            if left.is_some() && *hovered.borrow() == left {
                hovered.replace(None);
                settle();
            }
        });
        label.add_controller(motion);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let (Some(label), Some(text)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_label(&text.string());
        }
    });
    scheme.set_list_factory(Some(&factory));
    let mut child = scheme.first_child();
    while let Some(widget) = child {
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
            let (hovered, waits, restore) = (hovered.clone(), waits.clone(), restore.clone());
            popover.connect_closed(move |_| {
                hovered.replace(None);
                waits.set(waits.get() + 1);
                restore();
            });
        }
        child = widget.next_sibling();
    }
    add_row(
        "Color scheme",
        scheme.upcast_ref(),
        "Automatic uses the colors of your Konsole or Ghostty profile.",
    );

    // Font
    let detected_font = detected
        .font
        .clone()
        .unwrap_or_else(crate::term::default_font);
    let font_button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    font_button.set_font_desc(
        &current
            .font
            .as_deref()
            .map(pango::FontDescription::from_string)
            .unwrap_or_else(|| detected_font.clone()),
    );
    // Set while a Reset button moves its widget, so that isn't saved as a choice.
    let resetting = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(state);
    let guard = resetting.clone();
    font_button.connect_font_desc_notify(move |button| {
        if let (false, Some(state), Some(font)) = (guard.get(), weak.upgrade(), button.font_desc())
        {
            update_settings(&state, |s| s.font = Some(font.to_str().into()));
        }
    });
    let reset_font = gtk::Button::with_label("Reset");
    let weak = Rc::downgrade(state);
    let button_ref = font_button.downgrade();
    reset_font.connect_clicked(move |_| {
        let (Some(state), Some(button)) = (weak.upgrade(), button_ref.upgrade()) else {
            return;
        };
        resetting.set(true);
        button.set_font_desc(&detected_font);
        resetting.set(false);
        update_settings(&state, |s| s.font = None);
    });
    let font_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    font_button.set_hexpand(true);
    font_row.append(&font_button);
    font_row.append(&reset_font);
    add_row(
        "Terminal font",
        font_row.upcast_ref(),
        "Reset goes back to your terminal's font.",
    );

    // App font: everything but the terminals.
    let app_font_button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    app_font_button.set_font_desc(&app_font());
    let resetting = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(state);
    let guard = resetting.clone();
    app_font_button.connect_font_desc_notify(move |button| {
        if let (false, Some(state), Some(font)) = (guard.get(), weak.upgrade(), button.font_desc())
        {
            update_settings(&state, |s| s.app_font = Some(font.to_str().into()));
        }
    });
    let reset_app_font = gtk::Button::with_label("Reset");
    let weak = Rc::downgrade(state);
    let button_ref = app_font_button.downgrade();
    reset_app_font.connect_clicked(move |_| {
        let (Some(state), Some(button)) = (weak.upgrade(), button_ref.upgrade()) else {
            return;
        };
        // First back to the system's font, then show which one that is.
        update_settings(&state, |s| s.app_font = None);
        resetting.set(true);
        button.set_font_desc(&app_font());
        resetting.set(false);
    });
    let app_font_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    app_font_button.set_hexpand(true);
    app_font_row.append(&app_font_button);
    app_font_row.append(&reset_app_font);
    add_row(
        "App font",
        app_font_row.upcast_ref(),
        "The sidebar, tabs, menus and Settings. Reset goes back to the system's font.",
    );

    // Opacity
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_OPACITY, 1.0, 0.05);
    scale.set_value(opacity(&state.shared));
    scale.set_digits(2);
    scale.set_draw_value(true);
    // Beside the slider, not above it: the row keeps the height of the others.
    scale.set_value_pos(gtk::PositionType::Right);
    // Dragging it leaves no focus ring; Tab still reaches it.
    scale.set_focus_on_click(false);
    let resetting = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(state);
    let guard = resetting.clone();
    scale.connect_value_changed(move |scale| {
        if let (false, Some(state)) = (guard.get(), weak.upgrade()) {
            update_settings(&state, |s| s.background_opacity = Some(scale.value()));
        }
    });
    let reset_opacity = gtk::Button::with_label("Reset");
    reset_opacity.set_valign(gtk::Align::Center);
    let weak = Rc::downgrade(state);
    let scale_ref = scale.downgrade();
    reset_opacity.connect_clicked(move |_| {
        let (Some(state), Some(scale)) = (weak.upgrade(), scale_ref.upgrade()) else {
            return;
        };
        update_settings(&state, |s| s.background_opacity = None);
        resetting.set(true);
        scale.set_value(opacity(&state.shared));
        resetting.set(false);
    });
    let opacity_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    scale.set_hexpand(true);
    opacity_row.append(&scale);
    opacity_row.append(&reset_opacity);
    add_row(
        "Background opacity",
        opacity_row.upcast_ref(),
        "Reset goes back to the color scheme's opacity.",
    );

    // Window scale
    let ui_scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        settings::MIN_UI_SCALE,
        settings::MAX_UI_SCALE,
        0.05,
    );
    ui_scale.set_value(current.ui_scale());
    ui_scale.set_draw_value(true);
    ui_scale.set_value_pos(gtk::PositionType::Right);
    ui_scale.set_focus_on_click(false);
    ui_scale.set_format_value_func(|_, value| format!("{:.0}%", value * 100.0));
    // No mark at 100%: with one, themes draw the knob as a pointer, unlike
    // the opacity slider's. Reset goes back there.
    let weak = Rc::downgrade(state);
    ui_scale.connect_value_changed(move |scale| {
        if let Some(state) = weak.upgrade() {
            update_settings(&state, |s| s.ui_scale = Some(scale.value()));
        }
    });
    let reset_scale = gtk::Button::with_label("Reset");
    reset_scale.set_valign(gtk::Align::Center);
    let scale_ref = ui_scale.downgrade();
    reset_scale.connect_clicked(move |_| {
        // Moving the slider saves the new value.
        if let Some(scale) = scale_ref.upgrade() {
            scale.set_value(1.0);
        }
    });
    let scale_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    ui_scale.set_hexpand(true);
    scale_row.append(&ui_scale);
    scale_row.append(&reset_scale);
    add_row(
        "Window scale",
        scale_row.upcast_ref(),
        "Size of text and terminals in the window, from 75% to 150%.",
    );

    // Shell
    let default_shell = settings::default_shell();
    let shell = gtk::Entry::builder()
        .text(current.shell.clone().unwrap_or_default())
        .placeholder_text(format!("{default_shell} (default)"))
        .build();
    let weak = Rc::downgrade(state);
    shell.connect_changed(move |entry| {
        if let Some(state) = weak.upgrade() {
            let command = entry.text().trim().to_string();
            update_settings(&state, |s| {
                s.shell = (!command.is_empty()).then_some(command)
            });
        }
    });
    add_row(
        "Shell",
        shell.upcast_ref(),
        "Command started in new terminals, e.g. \"fish\" or \"zsh -l\". Empty uses the shell your Konsole or Ghostty profile starts, else $SHELL (on a Mac as a login shell).",
    );

    // Shell integration
    let integration = gtk::CheckButton::builder()
        .label("Mark commands in bash and zsh")
        .active(current.shell_integration())
        .build();
    let weak = Rc::downgrade(state);
    integration.connect_toggled(move |check| {
        if let Some(state) = weak.upgrade() {
            let on = check.is_active();
            update_settings(&state, |s| s.shell_integration = Some(on));
        }
    });
    add_row(
        "Shell integration",
        integration.upcast_ref(),
        "Starts them with a small script that loads your configuration, then marks prompts and commands (fish does on its own). For failed-command marks, copying the last output and notices when long commands end. New tabs only.",
    );

    // Scrollback
    let scrollback = gtk::SpinButton::with_range(0.0, 1_000_000.0, 1_000.0);
    scrollback.set_value(current.scrollback_lines() as f64);
    let weak = Rc::downgrade(state);
    scrollback.connect_value_changed(move |spin| {
        if let Some(state) = weak.upgrade() {
            update_settings(&state, |s| s.scrollback = Some(spin.value() as u32));
        }
    });
    add_row(
        "Scrollback lines",
        scrollback.upcast_ref(),
        &format!("Default {}.", settings::DEFAULT_SCROLLBACK),
    );

    // Cursor
    let blink = gtk::CheckButton::builder()
        .label("Blink")
        .active(current.cursor_blink())
        .build();
    let weak = Rc::downgrade(state);
    blink.connect_toggled(move |check| {
        if let Some(state) = weak.upgrade() {
            let on = check.is_active();
            update_settings(&state, |s| s.cursor_blink = Some(on));
        }
    });
    add_row(
        "Cursor",
        blink.upcast_ref(),
        "Programs can still choose for themselves (e.g. Neovim's insert mode).",
    );

    // Start directory
    let start_dir = gtk::Entry::builder()
        .text(current.start_dir.clone().unwrap_or_default())
        .placeholder_text("~ (default)")
        .build();
    let weak = Rc::downgrade(state);
    start_dir.connect_changed(move |entry| {
        if let Some(state) = weak.upgrade() {
            let dir = entry.text().trim().to_string();
            update_settings(&state, |s| s.start_dir = (!dir.is_empty()).then_some(dir));
        }
    });
    let browse = gtk::Button::with_label("Browse…");
    let weak = Rc::downgrade(state);
    let entry_ref = start_dir.downgrade();
    browse.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let entry_ref = entry_ref.clone();
        let parent = state.shared.settings_window.upgrade();
        gtk::FileDialog::builder()
            .title("Start directory")
            .modal(true)
            .build()
            .select_folder(parent.as_ref(), gio::Cancellable::NONE, move |result| {
                if let (Ok(folder), Some(entry)) = (result, entry_ref.upgrade())
                    && let Some(path) = folder.path()
                {
                    entry.set_text(&tilde(&path));
                }
            });
    });
    let start_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    start_dir.set_hexpand(true);
    start_row.append(&start_dir);
    start_row.append(&browse);
    add_row(
        "Start directory",
        start_row.upcast_ref(),
        "Where new terminals (the + button) open. Empty uses your home folder.",
    );

    let path = settings::config_path()
        .and_then(|p| Some(p.parent()?.join("profiles").join(format!("{profile}.conf"))))
        .map(|p| tilde(&p))
        .unwrap_or_default();
    let footer = gtk::Label::builder()
        .label(format!("Saved to {path}"))
        .xalign(0.0)
        .margin_top(8)
        .css_classes(["dim-label", "caption"])
        .build();
    grid.attach(&footer, 0, next_row.get(), 2, 1);
    grid.upcast()
}
