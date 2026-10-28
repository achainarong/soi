//! Settings ▸ Agents ▸ Agents to start: the presets the agent dropdowns
//! offer (see `crate::presets`).

use super::*;
use crate::presets::{self, Preset, TOOLS};

pub(in crate::app) fn presets_section() -> gtk::Widget {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 12);
    section.append(&rows);
    let list = Rc::new(RefCell::new(presets::load()));
    fill(&rows, &list);
    let add = gtk::Button::with_label("Add agent");
    add.set_halign(gtk::Align::Start);
    // The widgets keep the list: nothing else does, and it holds no widgets.
    let weak_rows = rows.downgrade();
    add.connect_clicked(move |_| {
        let Some(rows) = weak_rows.upgrade() else {
            return;
        };
        list.borrow_mut().push(Preset {
            name: "New agent".into(),
            ..Preset::new("opencode")
        });
        store(&list);
        fill(&rows, &list);
    });
    section.append(&add);
    section.upcast()
}

fn store(list: &RefCell<Vec<Preset>>) {
    if let Err(e) = presets::save(&list.borrow()) {
        crate::log::write(&format!("failed to save agent presets: {e}"));
    }
}

/// One card per preset: name, tool and remove; model and variant; wrapper
/// and arguments; and the command they make.
fn fill(rows: &gtk::Box, list: &Rc<RefCell<Vec<Preset>>>) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    let count = list.borrow().len();
    for (i, preset) in list.borrow().iter().enumerate() {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = gtk::Entry::builder()
            .text(&preset.name)
            .placeholder_text("Name in the dropdowns")
            .hexpand(true)
            .build();
        let tool = gtk::DropDown::from_strings(&TOOLS.map(|(_, label)| label));
        let selected = TOOLS
            .iter()
            .position(|(program, _)| *program == preset.program);
        tool.set_selected(selected.unwrap_or(0) as u32);
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.set_tooltip_text(Some("Remove this agent"));
        // The dropdowns need one to offer.
        remove.set_sensitive(count > 1);
        top.append(&name);
        top.append(&tool);
        top.append(&remove);
        let middle = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let model = gtk::Entry::builder()
            .text(&preset.model)
            .hexpand(true)
            .build();
        let variant = gtk::Entry::builder().text(&preset.variant).build();
        middle.append(&model);
        middle.append(&variant);
        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let wrapper = gtk::Entry::builder()
            .text(&preset.wrapper)
            .placeholder_text("Start through (optional), e.g. mate")
            .tooltip_text(
                "A command put in front of the tool's, for a wrapper that starts it: \
                 mate gives mate claude … or mate opencode …",
            )
            .build();
        let args = gtk::Entry::builder()
            .text(&preset.args)
            .placeholder_text("More arguments (optional), e.g. --agent plan")
            .hexpand(true)
            .build();
        bottom.append(&wrapper);
        bottom.append(&args);
        let preview = gtk::Label::builder()
            .label(preset.preview())
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .selectable(true)
            .css_classes(["dim-label", "caption", "monospace"])
            .tooltip_text("The command this starts; <brief> is the task brief")
            .build();
        card.append(&top);
        card.append(&middle);
        card.append(&bottom);
        card.append(&preview);
        rows.append(&card);
        explain(&preset.program, &model, &variant);
        let show = move |list: &RefCell<Vec<Preset>>, preview: &gtk::Label| {
            if let Some(preset) = list.borrow().get(i) {
                preview.set_label(&preset.preview());
            }
        };

        let edit = |entry: &gtk::Entry, set: fn(&mut Preset, String)| {
            let (list, preview) = (list.clone(), preview.clone());
            entry.connect_changed(move |entry| {
                if let Some(preset) = list.borrow_mut().get_mut(i) {
                    set(preset, entry.text().trim().to_string());
                }
                store(&list);
                show(&list, &preview);
            });
        };
        edit(&name, |p, v| p.name = v);
        edit(&model, |p, v| p.model = v);
        edit(&variant, |p, v| p.variant = v);
        edit(&args, |p, v| p.args = v);
        edit(&wrapper, |p, v| p.wrapper = v);
        let list_ = list.clone();
        tool.connect_selected_notify(move |tool| {
            let list = &list_;
            let program = TOOLS[tool.selected() as usize % TOOLS.len()].0;
            if let Some(preset) = list.borrow_mut().get_mut(i) {
                preset.program = program.into();
            }
            store(list);
            explain(program, &model, &variant);
            show(list, &preview);
        });
        let (weak_rows, list_) = (rows.downgrade(), list.clone());
        remove.connect_clicked(move |_| {
            let list = &list_;
            let Some(rows) = weak_rows.upgrade() else {
                return;
            };
            list.borrow_mut().remove(i);
            store(list);
            fill(&rows, list);
        });
    }
}

/// What the model and variant mean for the tool.
fn explain(program: &str, model: &gtk::Entry, variant: &gtk::Entry) {
    if program == "claude" {
        model.set_placeholder_text(Some("Model (optional), e.g. opus or sonnet"));
        variant.set_placeholder_text(Some("Effort: low … max"));
        variant.set_tooltip_text(None);
    } else if program == "pi" {
        model.set_placeholder_text(Some("Model (optional), as provider/model"));
        variant.set_placeholder_text(Some("Thinking: off … max"));
        variant.set_tooltip_text(Some(
            "Pi's thinking level: off, minimal, low, medium, high, xhigh or max",
        ));
    } else {
        model.set_placeholder_text(Some("Model (optional), as provider/model"));
        variant.set_placeholder_text(Some("Variant: see tooltip"));
        variant.set_tooltip_text(Some(
            "OpenCode's terminal app has no variant option. Define an OpenCode agent \
             with that model and variant, and pass --agent <name> as an argument.",
        ));
    }
    variant.set_sensitive(Preset::takes_variant(program));
}
