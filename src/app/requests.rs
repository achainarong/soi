//! Choosing the skills and commands to request for a task, one toggle each,
//! and the collections that choose several at once (see `crate::skills`,
//! `crate::collections`).
use super::*;
use crate::collections::{self, Collection};
use crate::skills::{self, Item, Kind};

type Changed = Rc<dyn Fn(Vec<String>, Vec<String>)>;

pub(super) struct Picker {
    pub(super) widget: gtk::Box,
    /// "No collection", then the collections; `None` where none are offered.
    collection: Option<gtk::DropDown>,
    collections: Vec<Collection>,
    skills: gtk::FlowBox,
    commands: gtk::FlowBox,
    repository: RefCell<PathBuf>,
    /// Told what is pressed when a toggle changes: skills, commands.
    on_change: RefCell<Option<Changed>>,
}

impl Picker {
    /// Toggles for what is found for `repository`, with `skills` and
    /// `commands` pressed; those no longer there stay listed. With
    /// `collections`, a dropdown above chooses a collection's instead, and
    /// the collection named `choose` is chosen to begin with.
    pub(super) fn new(
        repository: &Path,
        skills: &[String],
        commands: &[String],
        collections: bool,
        choose: Option<&str>,
    ) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let list = if collections {
            collections::load()
        } else {
            Vec::new()
        };
        let collection = collections.then(|| {
            let names: Vec<&str> = std::iter::once("No collection")
                .chain(list.iter().map(|c| c.name.as_str()))
                .collect();
            let dropdown = gtk::DropDown::from_strings(&names);
            dropdown.set_tooltip_text(Some(
                "Chooses the collection's skills and commands; set collections up in Settings ▸ Agents",
            ));
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&gtk::Label::new(Some("Collection")));
            row.append(&dropdown);
            widget.append(&row);
            dropdown
        });
        let flow = |label: &str| {
            widget.append(&gtk::Label::builder().label(label).xalign(0.0).build());
            let flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .column_spacing(4)
                .row_spacing(4)
                .max_children_per_line(30)
                .homogeneous(false)
                .build();
            widget.append(&flow);
            flow
        };
        let skill_flow = flow("Skills (click to choose; the brief names them)");
        let command_flow = flow("Commands (click to choose; the brief asks to run them)");
        let picker = Rc::new(Self {
            widget,
            collection,
            collections: list,
            skills: skill_flow,
            commands: command_flow,
            repository: RefCell::new(repository.to_path_buf()),
            on_change: RefCell::default(),
        });
        let chosen = choose.and_then(|name| picker.collections.iter().position(|c| c.name == name));
        match (&picker.collection, chosen) {
            (Some(dropdown), Some(i)) => {
                dropdown.set_selected(i as u32 + 1);
                let c = &picker.collections[i];
                picker.set(&c.skills, &c.commands);
            }
            _ => picker.set(skills, commands),
        }
        if let Some(dropdown) = &picker.collection {
            let weak = Rc::downgrade(&picker);
            dropdown.connect_selected_notify(move |dropdown| {
                let Some(picker) = weak.upgrade() else { return };
                let chosen = (dropdown.selected() as usize)
                    .checked_sub(1)
                    .and_then(|i| picker.collections.get(i));
                if let Some(c) = chosen {
                    picker.set(&c.skills, &c.commands);
                }
            });
        }
        picker
    }

    pub(super) fn skills(&self) -> Vec<String> {
        pressed(&self.skills)
    }

    pub(super) fn commands(&self) -> Vec<String> {
        pressed(&self.commands)
    }

    /// The collection chosen in the dropdown, if any.
    pub(super) fn collection(&self) -> Option<String> {
        let dropdown = self.collection.as_ref()?;
        let i = (dropdown.selected() as usize).checked_sub(1)?;
        Some(self.collections.get(i)?.name.clone())
    }

    /// Lists what is found for another repository, keeping what is pressed.
    pub(super) fn set_repository(&self, repository: &Path) {
        self.repository.replace(repository.to_path_buf());
        self.set(&self.skills(), &self.commands());
    }

    /// Tells `f` what is pressed whenever a toggle changes.
    pub(super) fn connect_changed(&self, f: impl Fn(Vec<String>, Vec<String>) + 'static) {
        self.on_change.replace(Some(Rc::new(f)));
        self.set(&self.skills(), &self.commands());
    }

    fn set(&self, skills: &[String], commands: &[String]) {
        let repository = self.repository.borrow().clone();
        // Weak, so the toggles don't keep the picker or each other.
        let notify = self.on_change.borrow().clone().map(|f| {
            let (skills, commands) = (self.skills.downgrade(), self.commands.downgrade());
            Rc::new(move || {
                if let (Some(skills), Some(commands)) = (skills.upgrade(), commands.upgrade()) {
                    f(pressed(&skills), pressed(&commands));
                }
            }) as Rc<dyn Fn()>
        });
        fill(
            &self.skills,
            Kind::Skill,
            &repository,
            skills,
            notify.clone(),
        );
        fill(&self.commands, Kind::Command, &repository, commands, notify);
    }
}

fn fill(
    flow: &gtk::FlowBox,
    kind: Kind,
    repository: &Path,
    selected: &[String],
    on_change: Option<Rc<dyn Fn()>>,
) {
    flow.remove_all();
    let mut items = skills::discover(kind, repository);
    for name in selected {
        if !items.iter().any(|item| &item.name == name) {
            items.push(Item {
                name: name.clone(),
                description: "Not found on this computer any more".into(),
                path: PathBuf::new(),
            });
        }
    }
    if items.is_empty() {
        flow.append(
            &gtk::Label::builder()
                .label(match kind {
                    Kind::Skill => "None found in ~/.claude/skills, ~/.agents/skills or the repository's .claude/skills and .agents/skills.",
                    Kind::Command => "None found in ~/.claude/commands, ~/.codex/prompts or the repository's .claude/commands.",
                })
                .wrap(true)
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        return;
    }
    let prefix = if kind == Kind::Command { "/" } else { "" };
    for item in items {
        let toggle = gtk::ToggleButton::builder()
            .label(format!("{prefix}{}", item.name))
            .active(selected.contains(&item.name))
            .build();
        let tip = if item.path.as_os_str().is_empty() {
            item.description
        } else {
            format!("{}\n\n{}", item.description, item.path.display())
        };
        toggle.set_tooltip_text(Some(tip.trim()));
        if let Some(f) = &on_change {
            let f = f.clone();
            toggle.connect_toggled(move |_| f());
        }
        flow.append(&toggle);
    }
}

/// The names pressed in `flow`, in its order.
fn pressed(flow: &gtk::FlowBox) -> Vec<String> {
    let mut names = Vec::new();
    let mut child = flow.first_child();
    while let Some(row) = child {
        child = row.next_sibling();
        if let Some(toggle) = row
            .downcast_ref::<gtk::FlowBoxChild>()
            .and_then(|row| row.child())
            .and_downcast::<gtk::ToggleButton>()
            && toggle.is_active()
            && let Some(label) = toggle.label()
        {
            // Commands show a `/` that their names don't have.
            names.push(label.strip_prefix('/').unwrap_or(&label).to_string());
        }
    }
    names
}

/// Settings ▸ Agents ▸ Collections: name, default and what each chooses.
pub(super) fn collections_section() -> gtk::Widget {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 16);
    section.append(&rows);
    let list = Rc::new(RefCell::new(collections::load()));
    fill_collections(&rows, &list);
    let add = gtk::Button::with_label("Add collection");
    add.set_halign(gtk::Align::Start);
    let weak_rows = rows.downgrade();
    add.connect_clicked(move |_| {
        let Some(rows) = weak_rows.upgrade() else {
            return;
        };
        list.borrow_mut().push(Collection {
            name: "New collection".into(),
            ..Default::default()
        });
        store(&list);
        fill_collections(&rows, &list);
    });
    section.append(&add);
    section.upcast()
}

fn store(list: &RefCell<Vec<Collection>>) {
    if let Err(e) = collections::save(&list.borrow()) {
        crate::log::write(&format!("failed to save collections: {e}"));
    }
}

fn fill_collections(rows: &gtk::Box, list: &Rc<RefCell<Vec<Collection>>>) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    // Only the user's folders: settings belong to no repository.
    let home = crate::session::home_dir();
    for (i, collection) in list.borrow().iter().enumerate() {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = gtk::Entry::builder()
            .text(&collection.name)
            .placeholder_text("Name in the dropdowns")
            .hexpand(true)
            .build();
        let default = gtk::CheckButton::with_label("Default for new tasks");
        default.set_active(collection.default);
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.set_tooltip_text(Some("Remove this collection"));
        top.append(&name);
        top.append(&default);
        top.append(&remove);
        card.append(&top);
        let picker = Picker::new(&home, &collection.skills, &collection.commands, false, None);
        card.append(&picker.widget);
        rows.append(&card);

        let list_ = list.clone();
        name.connect_changed(move |entry| {
            if let Some(c) = list_.borrow_mut().get_mut(i) {
                c.name = entry.text().trim().to_string();
            }
            store(&list_);
        });
        let (list_, weak_rows) = (list.clone(), rows.downgrade());
        default.connect_toggled(move |check| {
            collections::set_default(&mut list_.borrow_mut(), check.is_active().then_some(i));
            store(&list_);
            // The others' checks go off.
            if let Some(rows) = weak_rows.upgrade() {
                let list = list_.clone();
                glib::idle_add_local_once(move || fill_collections(&rows, &list));
            }
        });
        let list_ = list.clone();
        picker.connect_changed(move |skills, commands| {
            if let Some(c) = list_.borrow_mut().get_mut(i) {
                c.skills = skills;
                c.commands = commands;
            }
            store(&list_);
        });
        let (list_, weak_rows) = (list.clone(), rows.downgrade());
        remove.connect_clicked(move |_| {
            let Some(rows) = weak_rows.upgrade() else {
                return;
            };
            list_.borrow_mut().remove(i);
            store(&list_);
            fill_collections(&rows, &list_);
        });
    }
}
