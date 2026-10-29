//! Choosing the skills, commands and MCP servers to request for a task, one
//! toggle each, and the collections that choose several at once (see
//! `crate::skills`, `crate::mcp`, `crate::collections`).
use super::*;
use crate::collections::{self, Collection};
use crate::skills::{self, Item, Kind};

type Changed = Rc<dyn Fn(Vec<String>, Vec<String>, Vec<String>)>;

pub(super) struct Picker {
    pub(super) widget: gtk::Box,
    /// "No collection", then the collections; `None` where none are offered.
    collection: Option<gtk::DropDown>,
    collections: Vec<Collection>,
    skills: gtk::FlowBox,
    commands: gtk::FlowBox,
    mcps: gtk::FlowBox,
    /// How many are chosen.
    summary: gtk::Label,
    repository: RefCell<PathBuf>,
    /// Told what is pressed when a toggle changes: skills, commands, MCP
    /// servers.
    on_change: Rc<RefCell<Option<Changed>>>,
}

impl Picker {
    /// Toggles for what is found for `repository`, with `skills`,
    /// `commands` and `mcps` pressed; those no longer there stay listed. With
    /// `collections`, a dropdown above chooses a collection's instead, and
    /// the collection named `choose` is chosen to begin with.
    pub(super) fn new(
        repository: &Path,
        skills: &[String],
        commands: &[String],
        mcps: &[String],
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
                "Chooses the collection's skills, commands and MCP servers; make collections in Settings ▸ Collections",
            ));
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&gtk::Label::new(Some("Collection")));
            row.append(&dropdown);
            widget.append(&row);
            dropdown
        });
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Filter skills, commands and MCP servers")
            .hexpand(true)
            .build();
        let summary = gtk::Label::builder()
            .css_classes(["dim-label", "caption"])
            .build();
        top.append(&search);
        top.append(&summary);
        widget.append(&top);
        let flow = |label: &str| {
            widget.append(
                &gtk::Label::builder()
                    .label(label)
                    .xalign(0.0)
                    .css_classes(["caption"])
                    .build(),
            );
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
        let skill_flow = flow("Skills: the brief names them");
        let command_flow = flow("Commands: the brief asks to run them");
        let mcp_flow = flow("MCP servers: started with Claude Code and OpenCode");
        for flow in [&skill_flow, &command_flow, &mcp_flow] {
            let search = search.clone();
            flow.set_filter_func(move |row| {
                let query = search.text().to_lowercase();
                query.is_empty()
                    || row
                        .child()
                        .and_downcast::<gtk::ToggleButton>()
                        .and_then(|toggle| toggle.label())
                        .is_some_and(|label| label.to_lowercase().contains(&query))
            });
        }
        // Chosen first, then by name.
        for flow in [&skill_flow, &command_flow, &mcp_flow] {
            flow.set_sort_func(|a, b| {
                let key = |row: &gtk::FlowBoxChild| {
                    let toggle = row.child().and_downcast::<gtk::ToggleButton>();
                    let active = toggle.as_ref().is_some_and(|t| t.is_active());
                    let label = toggle.and_then(|t| t.label()).unwrap_or_default();
                    (!active, label.to_lowercase())
                };
                key(a).cmp(&key(b)).into()
            });
        }
        let flows = [
            skill_flow.downgrade(),
            command_flow.downgrade(),
            mcp_flow.downgrade(),
        ];
        search.connect_search_changed(move |_| {
            for flow in &flows {
                if let Some(flow) = flow.upgrade() {
                    flow.invalidate_filter();
                }
            }
        });
        let picker = Rc::new(Self {
            widget,
            collection,
            collections: list,
            skills: skill_flow,
            commands: command_flow,
            mcps: mcp_flow,
            summary,
            repository: RefCell::new(repository.to_path_buf()),
            on_change: Rc::default(),
        });
        let chosen = choose.and_then(|name| picker.collections.iter().position(|c| c.name == name));
        match (&picker.collection, chosen) {
            (Some(dropdown), Some(i)) => {
                dropdown.set_selected(i as u32 + 1);
                let c = &picker.collections[i];
                picker.set(&c.skills, &c.commands, &c.mcps);
            }
            _ => picker.set(skills, commands, mcps),
        }
        if let Some(dropdown) = &picker.collection {
            let weak = Rc::downgrade(&picker);
            dropdown.connect_selected_notify(move |dropdown| {
                let Some(picker) = weak.upgrade() else { return };
                let chosen = (dropdown.selected() as usize)
                    .checked_sub(1)
                    .and_then(|i| picker.collections.get(i));
                if let Some(c) = chosen {
                    picker.set(&c.skills, &c.commands, &c.mcps);
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

    pub(super) fn mcps(&self) -> Vec<String> {
        pressed(&self.mcps)
    }

    /// The collection chosen in the dropdown, if any.
    pub(super) fn collection(&self) -> Option<String> {
        let dropdown = self.collection.as_ref()?;
        let i = (dropdown.selected() as usize).checked_sub(1)?;
        Some(self.collections.get(i)?.name.clone())
    }

    /// Takes the collection dropdown's row out of `widget`, to be shown
    /// apart from the toggles.
    pub(super) fn take_collection_row(&self) -> Option<gtk::Widget> {
        let row = self.collection.as_ref()?.parent()?;
        self.widget.remove(&row);
        Some(row)
    }

    /// Lists what is found for another repository, keeping what is pressed.
    pub(super) fn set_repository(&self, repository: &Path) {
        self.repository.replace(repository.to_path_buf());
        self.set(&self.skills(), &self.commands(), &self.mcps());
    }

    /// Tells `f` what is pressed whenever a toggle changes.
    pub(super) fn connect_changed(
        &self,
        f: impl Fn(Vec<String>, Vec<String>, Vec<String>) + 'static,
    ) {
        self.on_change.replace(Some(Rc::new(f)));
    }

    fn set(&self, skills: &[String], commands: &[String], mcps: &[String]) {
        let repository = self.repository.borrow().clone();
        // Weak, so the toggles don't keep the picker or each other.
        let (weak_skills, weak_commands, weak_mcps, weak_summary, on_change) = (
            self.skills.downgrade(),
            self.commands.downgrade(),
            self.mcps.downgrade(),
            self.summary.downgrade(),
            self.on_change.clone(),
        );
        let notify: Rc<dyn Fn()> = Rc::new(move || {
            let (Some(skills), Some(commands), Some(mcps), Some(summary)) = (
                weak_skills.upgrade(),
                weak_commands.upgrade(),
                weak_mcps.upgrade(),
                weak_summary.upgrade(),
            ) else {
                return;
            };
            skills.invalidate_sort();
            commands.invalidate_sort();
            mcps.invalidate_sort();
            let (skills, commands, mcps) = (pressed(&skills), pressed(&commands), pressed(&mcps));
            summary.set_label(&summarize(skills.len(), commands.len(), mcps.len()));
            let f = on_change.borrow().clone();
            if let Some(f) = f {
                f(skills, commands, mcps);
            }
        });
        let found = |kind| skills::discover(kind, &repository);
        fill(
            &self.skills,
            Some(Kind::Skill),
            found(Kind::Skill),
            skills,
            &notify,
        );
        fill(
            &self.commands,
            Some(Kind::Command),
            found(Kind::Command),
            commands,
            &notify,
        );
        let servers = crate::mcp::discover(&repository)
            .into_iter()
            .map(|server| Item {
                description: server.description(),
                name: server.name,
                path: server.path,
            })
            .collect();
        fill(&self.mcps, None, servers, mcps, &notify);
        self.summary.set_label(&summarize(
            self.skills().len(),
            self.commands().len(),
            self.mcps().len(),
        ));
    }
}

/// `2 skills, 1 command, 0 MCP servers chosen`.
fn summarize(skills: usize, commands: usize, mcps: usize) -> String {
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    format!(
        "{}, {}, {} chosen",
        plural(skills, "skill"),
        plural(commands, "command"),
        plural(mcps, "MCP server")
    )
}

/// Toggles for `items` in `flow`: skills, commands, or MCP servers (no
/// `kind`).
fn fill(
    flow: &gtk::FlowBox,
    kind: Option<Kind>,
    mut items: Vec<Item>,
    selected: &[String],
    on_change: &Rc<dyn Fn()>,
) {
    flow.remove_all();
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
                    Some(Kind::Skill) => "None found in ~/.claude/skills, ~/.agents/skills or the repository's .claude/skills and .agents/skills.",
                    Some(Kind::Command) => "None found in ~/.claude/commands, ~/.codex/prompts or the repository's .claude/commands.",
                    None => "None found in ~/.config/soi/mcp.json (Settings ▸ Collections), ~/.claude.json, OpenCode's opencode.json or the repository's .mcp.json.",
                })
                .wrap(true)
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        return;
    }
    let prefix = if kind == Some(Kind::Command) { "/" } else { "" };
    for item in items {
        let toggle = gtk::ToggleButton::builder()
            .label(format!("{prefix}{}", item.name))
            .active(selected.contains(&item.name))
            .css_classes(["soi-chip"])
            .build();
        let tip = if item.path.as_os_str().is_empty() {
            item.description
        } else {
            format!("{}\n\n{}", item.description, item.path.display())
        };
        toggle.set_tooltip_text(Some(tip.trim()));
        let f = on_change.clone();
        toggle.connect_toggled(move |_| f());
        flow.append(&toggle);
    }
}

/// The names pressed in `flow`, by name.
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
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// Settings ▸ Collections: choose one, then its name, whether it is the
/// default, and what it chooses.
struct Page {
    list: RefCell<Vec<Collection>>,
    names: gtk::StringList,
    dropdown: gtk::DropDown,
    delete: gtk::Button,
    editor: gtk::Box,
    /// The collection shown; the dropdown's selection lags behind changes
    /// to its list.
    current: Cell<usize>,
    /// Set while the dropdown is changed from here, not by the user.
    updating: Cell<bool>,
}

pub(super) fn collections_page() -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for side in [gtk::PositionType::Top, gtk::PositionType::Bottom] {
        match side {
            gtk::PositionType::Top => page.set_margin_top(12),
            _ => page.set_margin_bottom(12),
        }
    }
    page.set_margin_start(12);
    page.set_margin_end(12);
    page.append(
        &gtk::Label::builder()
            .label("Skill, command and MCP collections")
            .xalign(0.0)
            .css_classes(["soi-section"])
            .build(),
    );
    page.append(
        &gtk::Label::builder()
            .label(
                "Skills, commands and MCP servers to request together, say tdd, /review and a \
                 database server for backend work. New task worktree and Task details offer them. \
                 The default is chosen for new tasks; a group can choose its own (right-click the \
                 group). Your own are listed here; a repository's show up in the task dialogs.",
            )
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let names = gtk::StringList::new(&[]);
    let dropdown = gtk::DropDown::builder().model(&names).hexpand(true).build();
    let new = gtk::Button::with_label("New collection");
    let delete = gtk::Button::from_icon_name("user-trash-symbolic");
    delete.set_tooltip_text(Some("Delete this collection"));
    row.append(&dropdown);
    row.append(&new);
    row.append(&delete);
    page.append(&row);
    let editor = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.append(&editor);
    let state = Rc::new(Page {
        list: RefCell::new(collections::load()),
        names,
        dropdown,
        delete,
        editor,
        current: Cell::new(0),
        updating: Cell::new(false),
    });
    // Open on the default, so it's seen first.
    let default = state.list.borrow().iter().position(|c| c.default);
    refresh(&state, default.unwrap_or(0));

    let weak = Rc::downgrade(&state);
    state.dropdown.connect_selected_notify(move |dropdown| {
        if let Some(state) = weak.upgrade()
            && !state.updating.get()
        {
            // It passes through no selection while its list changes.
            if dropdown.selected() == gtk::INVALID_LIST_POSITION {
                return;
            }
            state.current.set(dropdown.selected() as usize);
            show_collection(&state);
        }
    });
    let weak = Rc::downgrade(&state);
    new.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let count = state.list.borrow().len();
        let name = match count {
            0 => "New collection".to_string(),
            n => format!("New collection {}", n + 1),
        };
        state.list.borrow_mut().push(Collection {
            name,
            ..Default::default()
        });
        store(&state.list);
        refresh(&state, count);
        // Ready to be named.
        if let Some(entry) = state
            .editor
            .first_child()
            .and_then(|row| row.first_child())
            .and_downcast::<gtk::Entry>()
        {
            entry.grab_focus();
        }
    });
    let weak = Rc::downgrade(&state);
    state.delete.connect_clicked(move |button| {
        let Some(state) = weak.upgrade() else { return };
        let i = state.current.get();
        let Some(name) = state.list.borrow().get(i).map(|c| c.name.clone()) else {
            return;
        };
        let window = button.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(&state);
        gtk::AlertDialog::builder()
            .message(format!("Delete the collection “{name}”?"))
            .detail("Tasks keep the skills, commands and MCP servers they were given.")
            .buttons(["Cancel", "Delete"])
            .cancel_button(0)
            .default_button(0)
            .build()
            .choose(window.as_ref(), gio::Cancellable::NONE, move |answer| {
                let Some(state) = weak.upgrade() else { return };
                if answer == Ok(1) && i < state.list.borrow().len() {
                    state.list.borrow_mut().remove(i);
                    store(&state.list);
                    refresh(&state, i.saturating_sub(1));
                }
            });
    });
    own_servers(&page, &state);
    // The page keeps its state; the handlers only refer to it.
    page.connect_destroy(move |_| {
        let _ = &state;
    });
    page.upcast()
}

/// soi's own MCP servers, edited as their JSON: those to start only for
/// some tasks, not with every agent.
fn own_servers(page: &gtk::Box, state: &Rc<Page>) {
    page.append(
        &gtk::Label::builder()
            .label("Your MCP servers")
            .xalign(0.0)
            .margin_top(12)
            .css_classes(["soi-section"])
            .build(),
    );
    let path = crate::mcp::own_path();
    page.append(
        &gtk::Label::builder()
            .label(format!(
                "Servers for collections and tasks to start, as in Claude Code's .mcp.json: \
                 \"command\", \"args\" and \"env\", or \"type\": \"http\" with \"url\" and \
                 \"headers\". Those in ~/.claude.json, OpenCode's opencode.json and a \
                 repository's .mcp.json are listed too. Kept in {}, readable only by you.",
                path.display()
            ))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| crate::mcp::OWN_TEMPLATE.into());
    let view = gtk::TextView::builder()
        .monospace(true)
        .top_margin(6)
        .bottom_margin(6)
        .left_margin(6)
        .right_margin(6)
        .build();
    view.buffer().set_text(&text);
    page.append(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(160)
            .has_frame(true)
            .build(),
    );
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let save = gtk::Button::with_label("Save MCP servers");
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["caption"])
        .build();
    row.append(&save);
    row.append(&status);
    page.append(&row);
    let weak = Rc::downgrade(state);
    save.connect_clicked(move |_| {
        let buffer = view.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        match crate::mcp::save_own(&text) {
            Ok(()) => {
                status.set_label("Saved.");
                // The collection's toggles list them now.
                if let Some(state) = weak.upgrade() {
                    show_collection(&state);
                }
            }
            Err(e) => status.set_label(&format!("Not saved: {e}")),
        }
    });
}

fn store(list: &RefCell<Vec<Collection>>) {
    if let Err(e) = collections::save(&list.borrow()) {
        crate::log::write(&format!("failed to save collections: {e}"));
    }
}

/// The dropdown's names again, with the one at `selected` chosen.
fn refresh(state: &Rc<Page>, selected: usize) {
    state.updating.set(true);
    let names: Vec<String> = state.list.borrow().iter().map(label).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    state.names.splice(0, state.names.n_items(), &names);
    // The handler skips this change, so the editor is told here which one.
    let selected = selected.min(names.len().saturating_sub(1));
    state.current.set(selected);
    state.dropdown.set_selected(selected as u32);
    state.updating.set(false);
    state.dropdown.set_sensitive(!names.is_empty());
    state.delete.set_sensitive(!names.is_empty());
    show_collection(state);
}

fn label(collection: &Collection) -> String {
    let name = match collection.name.as_str() {
        "" => "Unnamed",
        name => name,
    };
    if collection.default {
        format!("{name} (default)")
    } else {
        name.to_string()
    }
}

/// The editor for the collection chosen in the dropdown.
fn show_collection(state: &Rc<Page>) {
    while let Some(child) = state.editor.first_child() {
        state.editor.remove(&child);
    }
    let i = state.current.get();
    let Some(collection) = state.list.borrow().get(i).cloned() else {
        state.editor.append(
            &gtk::Label::builder()
                .label("No collections yet. Click New collection, name it, and click the skills, commands and MCP servers it should choose.")
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build(),
        );
        return;
    };
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let name = gtk::Entry::builder()
        .text(&collection.name)
        .placeholder_text("Name, e.g. C++ or Terraform")
        .hexpand(true)
        .build();
    let default = gtk::CheckButton::with_label("Default for new tasks");
    default.set_active(collection.default);
    top.append(&name);
    top.append(&default);
    state.editor.append(&top);
    // Only the user's folders: settings belong to no repository.
    let picker = Picker::new(
        &crate::dirs::home_dir(),
        &collection.skills,
        &collection.commands,
        &collection.mcps,
        false,
        None,
    );
    state.editor.append(&picker.widget);

    let weak = Rc::downgrade(state);
    name.connect_changed(move |entry| {
        let Some(state) = weak.upgrade() else { return };
        if let Some(c) = state.list.borrow_mut().get_mut(i) {
            c.name = entry.text().trim().to_string();
        }
        store(&state.list);
        let label = state.list.borrow().get(i).map(label).unwrap_or_default();
        state.updating.set(true);
        state.names.splice(i as u32, 1, &[label.as_str()]);
        state.dropdown.set_selected(i as u32);
        state.updating.set(false);
    });
    let weak = Rc::downgrade(state);
    default.connect_toggled(move |check| {
        let Some(state) = weak.upgrade() else { return };
        collections::set_default(&mut state.list.borrow_mut(), check.is_active().then_some(i));
        store(&state.list);
        // Only the names change: the editor and its focus stay.
        let names: Vec<String> = state.list.borrow().iter().map(label).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        state.updating.set(true);
        state.names.splice(0, state.names.n_items(), &names);
        state.dropdown.set_selected(i as u32);
        state.updating.set(false);
    });
    let weak = Rc::downgrade(state);
    picker.connect_changed(move |skills, commands, mcps| {
        let Some(state) = weak.upgrade() else { return };
        if let Some(c) = state.list.borrow_mut().get_mut(i) {
            c.skills = skills;
            c.commands = commands;
            c.mcps = mcps;
        }
        store(&state.list);
    });
}
