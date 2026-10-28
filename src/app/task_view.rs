//! The task view: ticket details, the plan editor, agent hand-off and
//! workspace changes in one window beside the terminals. Nothing reaches an
//! agent unless the user copies the brief or starts an agent with it.
use super::*;
use crate::plan::{CreateError, SaveError};
use crate::tasks::{ChecklistItem, History, TaskContext, TaskStatus};
use tasks::{background, field, note, optional_path, path_text};

const TABS: [(&str, &str); 4] = [
    ("details", "Details"),
    ("plan", "Plan"),
    ("agents", "Agents"),
    ("changes", "Changes"),
];

struct View {
    state: Weak<State>,
    ws: Rc<WorkspaceUi>,
    window: gtk::Window,
    /// Fields the form does not edit (id, repository, branch) come from here.
    initial: TaskContext,
    title: gtk::Entry,
    status: gtk::DropDown,
    ticket: gtk::Entry,
    issue: gtk::Entry,
    description: gtk::TextView,
    acceptance: gtk::TextView,
    checklist: gtk::Box,
    new_item: gtk::Entry,
    files: gtk::TextView,
    requests: Rc<requests::Picker>,
    tabs: gtk::Stack,
    message: gtk::Label,
    agent_message: gtk::Label,
    plan: Plan,
    /// Set once the user chose to close without saving.
    discard: Cell<bool>,
}

struct Plan {
    path: RefCell<Option<PathBuf>>,
    /// The disk content the editor started from; saving requires the file
    /// to still hold it. `None` while nothing is loaded.
    base: RefCell<Option<String>>,
    /// A disk version the user has been told about but not resolved.
    disk: RefCell<Option<Option<String>>>,
    loading: Cell<bool>,
    dirty: Cell<bool>,
    saving: Cell<bool>,
    polling: Cell<bool>,
    /// Bumped whenever `base` changes, so a disk read that started earlier
    /// is not mistaken for an external edit.
    generation: Cell<u64>,
    entry: gtk::Entry,
    location: gtk::Label,
    visibility: gtk::Label,
    editor: gtk::TextView,
    preview: gtk::Label,
    modes: gtk::Stack,
    status: gtk::Label,
    conflict: gtk::Box,
    conflict_label: gtk::Label,
    use_disk: gtk::Button,
    create: gtk::Button,
    link: gtk::Button,
    unlink: gtk::Button,
}

fn workspace_id(state: &App, ws: &Rc<WorkspaceUi>) -> Option<Uuid> {
    state
        .workspaces
        .borrow()
        .iter()
        .find(|(_, w)| Rc::ptr_eq(w, ws))
        .map(|(id, _)| *id)
}

fn text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .into()
}

fn area(content: &gtk::Box, label: &str, value: &str, height: i32) -> gtk::TextView {
    content.append(&gtk::Label::builder().label(label).xalign(0.0).build());
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .height_request(height)
        // Tab moves focus, so the whole form stays keyboard reachable.
        .accepts_tab(false)
        .top_margin(4)
        .bottom_margin(4)
        .left_margin(6)
        .right_margin(6)
        .build();
    view.buffer().set_text(value);
    content.append(&view);
    view
}

fn alert(window: &impl IsA<gtk::Window>, message: &str, detail: &str) {
    gtk::AlertDialog::builder()
        .message(message)
        .detail(detail)
        .build()
        .show(Some(window));
}

pub(super) fn open(state: &App) {
    let Some(ws) = active_workspace(state) else {
        return;
    };
    let Some(id) = workspace_id(state, &ws) else {
        return;
    };
    // One view per workspace: two editors on one plan would only conflict.
    let name = format!("task-view-{id}");
    if let Some(window) = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.widget_name() == name)
    {
        window.present();
        return;
    }
    let initial = ws
        .task
        .borrow()
        .clone()
        .unwrap_or_else(|| TaskContext::new(ws.name.borrow().clone(), ws.root_dir.clone()));
    let window = gtk::Window::builder()
        .transient_for(&state.window)
        .title("Task details")
        .default_width(780)
        .default_height(760)
        .build();
    window.set_widget_name(&name);
    window.add_css_class("soi-tools");
    tasks::close_on_escape(&window);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let git = note(&body, &tilde(&git_directory(state, &ws)));
    let (weak, ws_) = (Rc::downgrade(state), ws.clone());
    tasks::watch_git(&git, move || match weak.upgrade() {
        Some(state) => git_directory(&state, &ws_),
        None => ws_.root_dir.clone(),
    });
    let tabs = gtk::Stack::builder().vexpand(true).build();
    let switcher = gtk::StackSwitcher::builder()
        .stack(&tabs)
        .halign(gtk::Align::Start)
        .build();
    body.append(&switcher);
    body.append(&tabs);

    // Details
    let details = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let title = field(&details, "Task name", &initial.title);
    let status = gtk::DropDown::from_strings(&TaskStatus::ALL.map(TaskStatus::label));
    status.set_selected(
        TaskStatus::ALL
            .iter()
            .position(|s| *s == initial.status)
            .unwrap_or(0) as u32,
    );
    note(
        &details,
        "Status (set by you; agent activity never changes it)",
    );
    details.append(&status);
    details.append(
        &gtk::Label::builder()
            .label("Ticket number (optional)")
            .xalign(0.0)
            .build(),
    );
    let ticket = gtk::Entry::builder()
        .text(initial.ticket.as_deref().unwrap_or_default())
        .placeholder_text("PROJ-123")
        .build();
    details.append(&ticket);
    details.append(
        &gtk::Label::builder()
            .label("Issue link (optional; a reference only, nothing is imported)")
            .xalign(0.0)
            .build(),
    );
    let issue_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let issue = gtk::Entry::builder()
        .text(initial.issue.as_deref().unwrap_or_default())
        .placeholder_text("https://github.com/owner/repo/issues/1")
        .hexpand(true)
        .build();
    let open_issue = gtk::Button::with_label("Open");
    issue_row.append(&issue);
    issue_row.append(&open_issue);
    details.append(&issue_row);
    let description = area(&details, "Description", &initial.description, 100);
    let acceptance = area(&details, "Acceptance criteria", &initial.acceptance, 80);
    details.append(&gtk::Label::builder().label("Checklist").xalign(0.0).build());
    let checklist = gtk::Box::new(gtk::Orientation::Vertical, 4);
    details.append(&checklist);
    let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let new_item = gtk::Entry::builder()
        .placeholder_text("New checklist item, then Enter")
        .hexpand(true)
        .build();
    let add = gtk::Button::with_label("Add");
    add_row.append(&new_item);
    add_row.append(&add);
    details.append(&add_row);
    let files = area(
        &details,
        "Important files (one per line; relative to the workspace)",
        &initial
            .important_files
            .iter()
            .map(|p| path_text(p))
            .collect::<Vec<_>>()
            .join("\n"),
        60,
    );
    files.set_monospace(true);
    details.append(
        &gtk::Label::builder()
            .label("Skills and commands for the agent")
            .xalign(0.0)
            .build(),
    );
    let requests =
        requests::Picker::new(&ws.root_dir, &initial.skills, &initial.commands, true, None);
    details.append(&requests.widget);
    tabs.add_titled(
        &gtk::ScrolledWindow::builder()
            .child(&details)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some(TABS[0].0),
        TABS[0].1,
    );

    // Plan
    let plan_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    note(
        &plan_box,
        "Plan file (relative to the worktree, or absolute; Soi never overwrites an existing file)",
    );
    let path_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entry = gtk::Entry::builder()
        .text(
            initial
                .plan
                .as_deref()
                .map(path_text)
                .unwrap_or_else(|| path_text(&initial.folder_or_default().join("plan.md"))),
        )
        .hexpand(true)
        .build();
    let create = gtk::Button::with_mnemonic("_Create plan");
    let link = gtk::Button::with_mnemonic("_Link existing");
    let unlink = gtk::Button::with_label("Unlink");
    unlink.set_tooltip_text(Some("Forget the link; the file stays on disk"));
    for widget in [
        entry.upcast_ref::<gtk::Widget>(),
        create.upcast_ref(),
        link.upcast_ref(),
        unlink.upcast_ref(),
    ] {
        path_row.append(widget);
    }
    plan_box.append(&path_row);
    let location = note(&plan_box, "");
    let visibility = note(&plan_box, "");
    let conflict = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    conflict.add_css_class("soi-plan-conflict");
    let conflict_label = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let use_disk = gtk::Button::with_label("Load disk version");
    use_disk.set_tooltip_text(Some("Discard your unsaved edits"));
    let keep_mine = gtk::Button::with_label("Keep mine");
    keep_mine.set_tooltip_text(Some("Your version replaces the disk version when you save"));
    conflict.append(&conflict_label);
    conflict.append(&use_disk);
    conflict.append(&keep_mine);
    conflict.set_visible(false);
    plan_box.append(&conflict);
    let modes = gtk::Stack::builder().vexpand(true).build();
    let mode_switcher = gtk::StackSwitcher::builder()
        .stack(&modes)
        .halign(gtk::Align::Start)
        .build();
    plan_box.append(&mode_switcher);
    let editor = gtk::TextView::builder()
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(6)
        .bottom_margin(6)
        .left_margin(8)
        .right_margin(8)
        .sensitive(false)
        .build();
    modes.add_titled(
        &gtk::ScrolledWindow::builder()
            .child(&editor)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some("edit"),
        "Edit",
    );
    let preview = gtk::Label::builder()
        .selectable(true)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .xalign(0.0)
        .yalign(0.0)
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .build();
    modes.add_titled(
        &gtk::ScrolledWindow::builder()
            .child(&preview)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some("preview"),
        "Preview",
    );
    plan_box.append(&modes);
    let plan_status = note(&plan_box, "");
    tabs.add_titled(&plan_box, Some(TABS[1].0), TABS[1].1);

    // Agents
    let agents = gtk::Box::new(gtk::Orientation::Vertical, 8);
    note(
        &agents,
        "Linking a plan does not send anything to an agent. Agents see this task only when you hand it over: copy the brief and paste it, or start a new agent with it. Running terminals are never typed into.",
    );
    let handoff = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let copy = gtk::Button::with_mnemonic("Copy _brief");
    handoff.append(&copy);
    let presets = Rc::new(crate::presets::load());
    let names: Vec<&str> = presets.iter().map(|preset| preset.name.as_str()).collect();
    let program = gtk::DropDown::from_strings(&names);
    program.set_tooltip_text(Some("The agent to start; set them up in Settings ▸ Agents"));
    handoff.append(&program);
    let model = tasks::model_entry();
    model.set_text(presets.first().map_or("", |p| p.model.as_str()));
    handoff.append(&model);
    let (weak_model, choices) = (model.downgrade(), presets.clone());
    program.connect_selected_notify(move |program| {
        if let (Some(model), Some(preset)) = (
            weak_model.upgrade(),
            choices.get(program.selected() as usize),
        ) {
            model.set_text(&preset.model);
        }
    });
    let start = gtk::Button::with_label("Start with brief");
    start.set_tooltip_text(Some(
        "Open a new tab running the agent, with the brief as its first prompt",
    ));
    handoff.append(&start);
    agents.append(&handoff);
    let agent_message = note(&agents, "");
    let expander = gtk::Expander::new(Some("Brief preview"));
    let brief = gtk::Label::builder()
        .selectable(true)
        .wrap(true)
        .xalign(0.0)
        .margin_start(8)
        .build();
    expander.set_child(Some(&brief));
    agents.append(&expander);
    note(&agents, "Linked sessions");
    let linked = gtk::Box::new(gtk::Orientation::Vertical, 4);
    agents.append(&linked);
    let session_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let bookmark = gtk::Button::with_label("Bookmark session…");
    let history = gtk::Button::with_label("Session history…");
    session_actions.append(&bookmark);
    session_actions.append(&history);
    agents.append(&session_actions);
    tabs.add_titled(
        &gtk::ScrolledWindow::builder()
            .child(&agents)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some(TABS[2].0),
        TABS[2].1,
    );

    // Changes
    let changes_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    note(
        &changes_box,
        "Workspace-wide: these are all uncommitted changes in this worktree, made by you or any agent. Soi cannot tell which agent changed what.",
    );
    let summary = gtk::Label::builder()
        .selectable(true)
        .xalign(0.0)
        .yalign(0.0)
        .vexpand(true)
        .css_classes(["monospace"])
        .build();
    changes_box.append(
        &gtk::ScrolledWindow::builder()
            .child(&summary)
            .vexpand(true)
            .build(),
    );
    let change_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let refresh = gtk::Button::with_label("Refresh");
    let diff = gtk::Button::with_mnemonic("Inspect _diff…");
    change_actions.append(&refresh);
    change_actions.append(&diff);
    changes_box.append(&change_actions);
    tabs.add_titled(&changes_box, Some(TABS[3].0), TABS[3].1);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let message = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let side = gtk::Button::with_label("Preview beside terminal");
    let close = gtk::Button::with_label("Close");
    let save = gtk::Button::with_mnemonic("_Save");
    save.set_tooltip_text(Some("Save task details and the plan (Ctrl+S)"));
    footer.append(&message);
    footer.append(&side);
    footer.append(&close);
    footer.append(&save);
    body.append(&footer);
    window.set_child(Some(&body));

    let view = Rc::new(View {
        state: Rc::downgrade(state),
        ws,
        window: window.clone(),
        initial: initial.clone(),
        title,
        status,
        ticket,
        issue,
        description,
        acceptance,
        checklist,
        new_item: new_item.clone(),
        files,
        requests,
        tabs: tabs.clone(),
        message,
        agent_message,
        plan: Plan {
            path: RefCell::new(initial.plan.clone()),
            base: RefCell::default(),
            disk: RefCell::default(),
            loading: Cell::new(false),
            dirty: Cell::new(false),
            saving: Cell::new(false),
            polling: Cell::new(false),
            generation: Cell::new(0),
            entry,
            location,
            visibility,
            editor,
            preview,
            modes: modes.clone(),
            status: plan_status,
            conflict,
            conflict_label,
            use_disk: use_disk.clone(),
            create: create.clone(),
            link: link.clone(),
            unlink: unlink.clone(),
        },
        discard: Cell::new(false),
    });
    for item in &initial.checklist {
        add_item(&view, item);
    }

    let weak = Rc::downgrade(&view);
    let on = move |f: fn(&Rc<View>)| {
        let weak = weak.clone();
        move || {
            if let Some(view) = weak.upgrade() {
                f(&view);
            }
        }
    };
    let added = on(|view| {
        let text = view.new_item.text().trim().to_string();
        view.new_item.set_text("");
        if !text.is_empty() {
            add_item(view, &ChecklistItem { text, done: false });
        }
    });
    let added = Rc::new(added);
    let a = added.clone();
    add.connect_clicked(move |_| a());
    new_item.connect_activate(move |_| added());
    let f = on(open_issue_link);
    open_issue.connect_clicked(move |_| f());
    let f = on(create_plan);
    create.connect_clicked(move |_| f());
    let f = on(link_plan);
    link.connect_clicked(move |_| f());
    let f = on(unlink_plan);
    unlink.connect_clicked(move |_| f());
    let f = on(|view| {
        let disk = view.plan.disk.take().flatten();
        if let Some(text) = disk {
            set_editor(view, &text);
            view.plan.status.set_label("Loaded the version on disk.");
        }
        view.plan.conflict.set_visible(false);
    });
    use_disk.connect_clicked(move |_| f());
    let f = on(|view| {
        if let Some(disk) = view.plan.disk.take() {
            set_base(view, disk);
        }
        view.plan.conflict.set_visible(false);
        update_dirty(view);
        view.plan
            .status
            .set_label("Keeping your version: saving replaces the disk version.");
    });
    keep_mine.connect_clicked(move |_| f());
    let f = on(|view| {
        if view.plan.modes.visible_child_name().as_deref() == Some("preview") {
            render_preview(view);
        }
    });
    modes.connect_visible_child_name_notify(move |_| f());
    let f = on(|view| {
        if !view.plan.loading.get() {
            update_dirty(view);
        }
    });
    view.plan.editor.buffer().connect_changed(move |_| f());
    let f = on(copy_brief);
    copy.connect_clicked(move |_| f());
    let weak = Rc::downgrade(&view);
    start.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade()
            && let Some(preset) = presets.get(program.selected() as usize)
        {
            start_agent(&view, &tasks::with_model(preset, &model));
        }
    });
    let brief_weak = Rc::downgrade(&view);
    expander.connect_expanded_notify(move |expander| {
        if let Some(view) = brief_weak.upgrade()
            && expander.is_expanded()
        {
            brief.set_label(&match form(&view) {
                Ok(task) => task.brief(&view.ws.root_dir),
                Err(e) => e,
            });
        }
    });
    let f = on(|view| {
        if let Some(state) = view.state.upgrade() {
            tasks::bookmark(&state);
        }
    });
    bookmark.connect_clicked(move |_| f());
    let f = on(|view| {
        if let Some(state) = view.state.upgrade() {
            tasks::history(&state);
        }
    });
    history.connect_clicked(move |_| f());
    load_sessions(&view, &linked);

    let load_changes: Rc<dyn Fn()> = {
        let (summary, refresh, state, ws) = (
            summary.downgrade(),
            refresh.downgrade(),
            view.state.clone(),
            view.ws.clone(),
        );
        Rc::new(move || {
            let (Some(label), Some(button), Some(state)) =
                (summary.upgrade(), refresh.upgrade(), state.upgrade())
            else {
                return;
            };
            let directory = git_directory(&state, &ws);
            button.set_sensitive(false);
            label.set_label("Reading workspace changes…");
            let (summary, refresh) = (label.downgrade(), button.downgrade());
            background(
                move || crate::git::status(&directory),
                move |result| {
                    if let Some(button) = refresh.upgrade() {
                        button.set_sensitive(true);
                    }
                    if let Some(label) = summary.upgrade() {
                        label.set_label(&match result {
                            Ok(text) if text.lines().nth(1).is_none() => {
                                "No uncommitted changes.".into()
                            }
                            Ok(text) | Err(text) => text,
                        });
                    }
                },
            );
        })
    };
    let reload = load_changes.clone();
    refresh.connect_clicked(move |_| reload());
    tabs.connect_visible_child_name_notify(move |tabs| {
        if tabs.visible_child_name().as_deref() == Some("changes") {
            load_changes();
        }
    });
    let f = on(|view| {
        if let Some(state) = view.state.upgrade() {
            changes::open_for(&state, &view.ws);
        }
    });
    diff.connect_clicked(move |_| f());
    let f = on(|view| {
        if let Some(state) = view.state.upgrade() {
            preview::toggle_for(&state, &view.ws);
        }
    });
    side.connect_clicked(move |_| f());
    let f = on(|view| view.window.close());
    close.connect_clicked(move |_| f());
    let f = on(|view| save_view(view, false));
    save.connect_clicked(move |_| f());

    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_scope(gtk::ShortcutScope::Managed);
    let weak = Rc::downgrade(&view);
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>s"),
        Some(gtk::CallbackAction::new(move |_, _| {
            if let Some(view) = weak.upgrade() {
                save_view(&view, false);
            }
            glib::Propagation::Stop
        })),
    ));
    // Alt+1…4 switch tabs; the switcher buttons have no mnemonics.
    for (index, (name, _)) in TABS.iter().enumerate() {
        let tabs = tabs.downgrade();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(&format!("<Alt>{}", index + 1)),
            Some(gtk::CallbackAction::new(move |_, _| {
                if let Some(tabs) = tabs.upgrade() {
                    tabs.set_visible_child_name(name);
                }
                glib::Propagation::Stop
            })),
        ));
    }
    window.add_controller(shortcuts);

    // The close request owns the view: it lives exactly as long as the window.
    let owner = view.clone();
    window.connect_close_request(move |_| confirm_close(&owner));

    refresh_plan(&view);
    let weak = Rc::downgrade(&view);
    glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
        let Some(view) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        poll_plan(&view);
        glib::ControlFlow::Continue
    });
    window.present();
}

fn add_item(view: &Rc<View>, item: &ChecklistItem) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let check = gtk::CheckButton::builder().active(item.done).build();
    check.set_tooltip_text(Some("Done"));
    let entry = gtk::Entry::builder().text(&item.text).hexpand(true).build();
    let remove = gtk::Button::from_icon_name("list-remove-symbolic");
    remove.set_tooltip_text(Some("Remove item"));
    row.append(&check);
    row.append(&entry);
    row.append(&remove);
    view.checklist.append(&row);
    let weak = row.downgrade();
    remove.connect_clicked(move |_| {
        if let Some(row) = weak.upgrade()
            && let Some(parent) = row.parent().and_downcast::<gtk::Box>()
        {
            parent.remove(&row);
        }
    });
}

fn checklist(view: &View) -> Vec<ChecklistItem> {
    let mut items = Vec::new();
    let mut child = view.checklist.first_child();
    while let Some(row) = child {
        child = row.next_sibling();
        let check = row.first_child().and_downcast::<gtk::CheckButton>();
        let entry = check
            .as_ref()
            .and_then(|c| c.next_sibling())
            .and_downcast::<gtk::Entry>();
        if let (Some(check), Some(entry)) = (check, entry) {
            let text = entry.text().trim().to_string();
            if !text.is_empty() {
                items.push(ChecklistItem {
                    text,
                    done: check.is_active(),
                });
            }
        }
    }
    items
}

/// The task as the form currently describes it.
fn form(view: &View) -> Result<TaskContext, String> {
    let title = view.title.text().trim().to_string();
    if title.is_empty() {
        return Err("Enter a task name.".into());
    }
    // Saved, not initial: saving may have added what the form doesn't show.
    let mut task = saved(view);
    task.title = title;
    task.status = TaskStatus::ALL
        .get(view.status.selected() as usize)
        .copied()
        .unwrap_or_default();
    task.ticket = crate::tasks::validate_ticket(view.ticket.text().as_str())?;
    task.issue = crate::tasks::validate_issue(view.issue.text().as_str())?;
    task.description = text(&view.description).trim_end().into();
    task.acceptance = text(&view.acceptance).trim_end().into();
    task.checklist = checklist(view);
    task.important_files = text(&view.files)
        .lines()
        .filter_map(|p| optional_path(p, &view.ws.root_dir))
        .collect();
    task.skills = view.requests.skills();
    task.commands = view.requests.commands();
    task.plan = view.plan.path.borrow().clone();
    Ok(task)
}

fn saved(view: &View) -> TaskContext {
    view.ws
        .task
        .borrow()
        .clone()
        .unwrap_or_else(|| view.initial.clone())
}

fn store(view: &View, mut task: TaskContext) -> Result<(), String> {
    let state = view.state.upgrade().ok_or("Soi is closing.")?;
    if workspace_id(&state, &view.ws).is_none() {
        return Err(
            "This workspace was closed, so its task details can no longer be saved.".into(),
        );
    }
    task.adopt_folder();
    let changes = task.changes_since(&saved(view));
    if let Err(e) = task.record(&changes) {
        crate::log::write(&format!("failed to record task history: {e}"));
    }
    view.ws.task.replace(Some(task));
    save_all(&state.shared);
    panel::refresh(&state.shared);
    rebuild_sidebar(&state);
    Ok(())
}

/// Saves the details, then the plan if edited; `close` closes on success.
fn save_view(view: &Rc<View>, close: bool) {
    let task = match form(view) {
        Ok(task) => task,
        Err(e) => {
            view.tabs.set_visible_child_name("details");
            view.message.set_label(&e);
            return;
        }
    };
    if task != saved(view)
        && let Err(e) = store(view, task)
    {
        view.message.set_label(&e);
        return;
    }
    if !view.plan.dirty.get() {
        view.message.set_label("Saved.");
        if close {
            view.discard.set(true);
            view.window.close();
        }
        return;
    }
    let weak = Rc::downgrade(view);
    save_plan(view, move |ok| {
        let Some(view) = weak.upgrade() else { return };
        if ok {
            view.message.set_label("Saved.");
            if close {
                view.discard.set(true);
                view.window.close();
            }
        } else {
            view.tabs.set_visible_child_name("plan");
            view.message
                .set_label("Task details saved; the plan was not saved (see Plan).");
        }
    });
}

fn confirm_close(view: &Rc<View>) -> glib::Propagation {
    let details = form(view).map_or(true, |task| task != saved(view));
    if view.discard.get() || !(details || view.plan.dirty.get()) {
        return glib::Propagation::Proceed;
    }
    let dialog = gtk::AlertDialog::builder()
        .message("Save changes to this task?")
        .detail(if view.plan.dirty.get() {
            "The plan and possibly the task details have unsaved changes."
        } else {
            "The task details have unsaved changes."
        })
        .buttons(["Cancel", "Discard", "Save"])
        .cancel_button(0)
        .default_button(2)
        .modal(true)
        .build();
    let weak = Rc::downgrade(view);
    dialog.choose(
        Some(&view.window),
        None::<&gio::Cancellable>,
        move |choice| {
            let Some(view) = weak.upgrade() else { return };
            match choice {
                Ok(1) => {
                    view.discard.set(true);
                    view.window.close();
                }
                Ok(2) => save_view(&view, true),
                _ => {}
            }
        },
    );
    glib::Propagation::Stop
}

fn open_issue_link(view: &Rc<View>) {
    match crate::tasks::validate_issue(view.issue.text().as_str()) {
        Ok(Some(uri)) => gtk::UriLauncher::new(&uri).launch(
            Some(&view.window),
            None::<&gio::Cancellable>,
            |_| {},
        ),
        Ok(None) => view.message.set_label("Enter an issue link first."),
        Err(e) => view.message.set_label(&e),
    }
}

// Plan

fn entry_path(view: &View) -> PathBuf {
    let worktree = &view.ws.root_dir;
    optional_path(view.plan.entry.text().as_str(), worktree)
        .unwrap_or_else(|| saved(view).folder_or_default().join("plan.md"))
}

/// Updates the location, buttons and editor for the linked plan.
fn refresh_plan(view: &Rc<View>) {
    let plan = &view.plan;
    let path = plan.path.borrow().clone();
    plan.create.set_visible(path.is_none());
    plan.link.set_visible(path.is_none());
    plan.unlink.set_visible(path.is_some());
    plan.entry.set_editable(path.is_none());
    plan.editor.set_sensitive(false);
    plan.conflict.set_visible(false);
    plan.disk.replace(None);
    set_base(view, None);
    plan.dirty.set(false);
    set_editor_text(view, "");
    plan.visibility.set_label("");
    let Some(path) = path else {
        plan.location.set_label(
            "No plan linked. Create one (seeded from the task details) or link an existing Markdown file.",
        );
        plan.status.set_label("");
        return;
    };
    plan.entry.set_text(&path_text(&path));
    plan.location
        .set_label(&format!("Saved at {}", path.display()));
    plan.status.set_label("Loading plan…");
    let (weak, file, linked) = (Rc::downgrade(view), path.clone(), path.clone());
    background(
        move || crate::plan::read(&file),
        move |result| {
            let Some(view) = weak.upgrade() else { return };
            if view.plan.path.borrow().as_ref() != Some(&linked) {
                return;
            }
            match result {
                Ok(text) => {
                    set_editor(&view, &text);
                    view.plan.editor.set_sensitive(true);
                    view.plan.status.set_label("");
                }
                Err(e) => view.plan.status.set_label(&format!(
                    "{e}\nFix the file or unlink it; Soi will not replace it."
                )),
            }
        },
    );
    let (weak, worktree, file) = (Rc::downgrade(view), view.ws.root_dir.clone(), path.clone());
    background(
        move || crate::git::visibility(&worktree, &file),
        move |result| {
            if let Some(view) = weak.upgrade() {
                view.plan.visibility.set_label(&match result {
                    Ok(v) => v.describe().to_string(),
                    Err(e) => e,
                });
            }
        },
    );
}

fn set_editor_text(view: &View, text: &str) {
    view.plan.loading.set(true);
    view.plan.editor.buffer().set_text(text);
    view.plan.loading.set(false);
}

/// Shows `text` as the clean, on-disk version of the plan.
fn set_editor(view: &View, text: &str) {
    set_editor_text(view, text);
    view.plan.disk.replace(None);
    set_base(view, Some(text.into()));
    view.plan.dirty.set(false);
    render_preview(view);
}

fn set_base(view: &View, base: Option<String>) {
    view.plan.base.replace(base);
    view.plan.generation.set(view.plan.generation.get() + 1);
}

fn update_dirty(view: &View) {
    let dirty = view.plan.base.borrow().as_deref() != Some(text(&view.plan.editor).as_str());
    view.plan.dirty.set(dirty);
    view.plan
        .status
        .set_label(if dirty { "Unsaved changes" } else { "" });
}

fn render_preview(view: &View) {
    view.plan
        .preview
        .set_markup(&preview::markup(&text(&view.plan.editor)));
}

fn link_to(view: &Rc<View>, path: PathBuf, message: &str) {
    view.plan.path.replace(Some(path.clone()));
    // A link records a file that now exists, so keep it even if the rest of
    // the form is never saved.
    let mut task = saved(view);
    task.plan = Some(path);
    if let Err(e) = store(view, task) {
        view.message.set_label(&e);
    } else {
        view.message.set_label(message);
    }
    refresh_plan(view);
}

fn create_plan(view: &Rc<View>) {
    let path = entry_path(view);
    let template = match form(view) {
        Ok(task) => task.plan_template(),
        Err(_) => saved(view).plan_template(),
    };
    view.plan.create.set_sensitive(false);
    let (weak, file) = (Rc::downgrade(view), path.clone());
    background(
        move || crate::plan::create(&file, &template),
        move |result| {
            let Some(view) = weak.upgrade() else { return };
            view.plan.create.set_sensitive(true);
            match result {
                Ok(()) => link_to(&view, path, "Plan created."),
                Err(CreateError::Exists) => view.plan.location.set_label(&format!(
                    "{} already exists and was not changed. Use Link existing to edit it.",
                    path.display()
                )),
                Err(CreateError::Failed(e)) => view.plan.location.set_label(&e),
            }
        },
    );
}

fn link_plan(view: &Rc<View>) {
    let path = entry_path(view);
    let (weak, file) = (Rc::downgrade(view), path.clone());
    background(
        move || crate::plan::read(&file).map(|_| ()),
        move |result| {
            let Some(view) = weak.upgrade() else { return };
            match result {
                Ok(()) => link_to(&view, path, "Plan linked."),
                Err(e) => view.plan.location.set_label(&e),
            }
        },
    );
}

fn unlink_plan(view: &Rc<View>) {
    if view.plan.dirty.get() {
        view.plan
            .status
            .set_label("Save or undo your plan edits before unlinking.");
        return;
    }
    view.plan.path.replace(None);
    let mut task = saved(view);
    task.plan = None;
    if let Err(e) = store(view, task) {
        view.message.set_label(&e);
    } else {
        view.message
            .set_label("Plan unlinked; the file is still on disk.");
    }
    refresh_plan(view);
}

fn show_conflict(view: &View, disk: Option<String>) {
    let plan = &view.plan;
    plan.conflict_label.set_label(if disk.is_some() {
        "The plan changed on disk (perhaps an agent edited it) while you had unsaved edits."
    } else {
        "The plan file was deleted or replaced while you had unsaved edits."
    });
    plan.use_disk.set_visible(disk.is_some());
    plan.disk.replace(Some(disk));
    plan.conflict.set_visible(true);
}

fn save_plan(view: &Rc<View>, done: impl FnOnce(bool) + 'static) {
    let plan = &view.plan;
    let Some(path) = plan.path.borrow().clone() else {
        return done(true);
    };
    if plan.conflict.is_visible() {
        plan.status
            .set_label("Choose which version to keep before saving.");
        return done(false);
    }
    if plan.saving.replace(true) {
        plan.status.set_label("Still saving the plan…");
        return done(false);
    }
    let contents = text(&plan.editor);
    let base = plan.base.borrow().clone();
    let (weak, written) = (Rc::downgrade(view), contents.clone());
    background(
        move || crate::plan::save(&path, &contents, base.as_deref()),
        move |result| {
            let Some(view) = weak.upgrade() else { return };
            view.plan.saving.set(false);
            match result {
                Ok(()) => {
                    set_base(&view, Some(written));
                    update_dirty(&view);
                    view.plan.status.set_label("Plan saved.");
                    done(true);
                }
                Err(SaveError::Conflict(disk)) => {
                    show_conflict(&view, disk);
                    done(false);
                }
                Err(SaveError::Failed(e)) => {
                    view.plan
                        .status
                        .set_label(&format!("{e}\nYour edits are still here."));
                    done(false);
                }
            }
        },
    );
}

/// Follows external edits: a clean editor reloads, a dirty one asks.
fn poll_plan(view: &Rc<View>) {
    let plan = &view.plan;
    let Some(path) = plan.path.borrow().clone() else {
        return;
    };
    if plan.base.borrow().is_none() || plan.saving.get() || plan.polling.replace(true) {
        return;
    }
    let generation = plan.generation.get();
    let weak = Rc::downgrade(view);
    let file = path.clone();
    background(
        move || match crate::plan::read(&file) {
            Ok(text) => Ok(Some(text)),
            Err(_) if !file.exists() => Ok(None),
            Err(e) => Err(e),
        },
        move |result| {
            let Some(view) = weak.upgrade() else { return };
            let plan = &view.plan;
            plan.polling.set(false);
            if plan.generation.get() != generation || plan.path.borrow().as_ref() != Some(&path) {
                return;
            }
            let Ok(disk) = result else { return };
            if disk == *plan.base.borrow() || plan.disk.borrow().as_ref() == Some(&disk) {
                return;
            }
            match disk {
                Some(text) if !plan.dirty.get() => {
                    set_editor(&view, &text);
                    plan.status.set_label("Reloaded a change made outside Soi.");
                }
                None if !plan.dirty.get() => {
                    plan.disk.replace(Some(None));
                    plan.status.set_label(
                        "The plan file is missing on disk. Restore it, or unlink it; editing and saving recreates it.",
                    );
                }
                disk => show_conflict(&view, disk),
            }
        },
    );
}

// Agents

fn copy_brief(view: &Rc<View>) {
    match form(view) {
        Ok(task) => {
            view.window
                .clipboard()
                .set_text(&task.brief(&view.ws.root_dir));
            view.agent_message.set_label(if view.plan.dirty.get() {
                "Brief copied. The plan has unsaved edits: save it so the agent reads your latest version."
            } else {
                "Brief copied. Paste it into an agent; nothing was sent automatically."
            });
        }
        Err(e) => view.agent_message.set_label(&e),
    }
}

/// Opens a new tab running the agent with the brief as its initial prompt.
/// Arguments go straight to the PTY, never through a shell.
fn start_agent(view: &Rc<View>, preset: &crate::presets::Preset) {
    if view.plan.dirty.get() {
        view.agent_message
            .set_label("Save the plan first: the agent reads the file on disk.");
        return;
    }
    let task = match form(view) {
        Ok(task) => task,
        Err(e) => return view.agent_message.set_label(&e),
    };
    let Some(state) = view.state.upgrade() else {
        return;
    };
    let Some(id) = workspace_id(&state, &view.ws) else {
        return view.agent_message.set_label("This workspace was closed.");
    };
    if task != saved(view)
        && let Err(e) = store(view, task.clone())
    {
        return view.agent_message.set_label(&e);
    }
    select_workspace(&state, id);
    let result = current_pane(&state)
        .ok_or_else(|| "No terminal pane available.".to_string())
        .and_then(|pane| {
            tasks::launch(
                &state,
                &pane,
                &view.ws.root_dir,
                tasks::agent_command(
                    preset,
                    Some(task.brief(&view.ws.root_dir)),
                    &task,
                    &tasks::agent_path(&state),
                ),
            )
        });
    if result.is_ok()
        && let Err(e) = task.record(&[tasks::started(preset, true)])
    {
        crate::log::write(&format!("failed to record task history: {e}"));
    }
    view.agent_message.set_label(&match result {
        Ok(()) => format!(
            "Started {} in a new tab with the brief as its first prompt.",
            preset.name
        ),
        Err(e) => e,
    });
}

fn load_sessions(view: &Rc<View>, linked: &gtk::Box) {
    let (weak_linked, weak_state, task_id, directory) = (
        linked.downgrade(),
        view.state.clone(),
        view.initial.id,
        view.ws.root_dir.clone(),
    );
    background(
        || History::load(&crate::tasks::history_path()),
        move |result| {
            let Some(linked) = weak_linked.upgrade() else {
                return;
            };
            let history = match result {
                Ok(history) => history,
                Err(e) => {
                    note(&linked, &format!("Cannot load linked sessions: {e}"));
                    return;
                }
            };
            let mut count = 0;
            for session in history
                .sessions
                .into_iter()
                .filter(|s| s.task_id == Some(task_id) || s.directory == directory)
            {
                count += 1;
                let button = gtk::Button::with_label(&format!(
                    "Resume {} · {} · {}",
                    session.title, session.provider, session.session_id
                ));
                button
                    .set_sensitive(session.resume_command().is_ok() && session.directory.is_dir());
                let weak = weak_state.clone();
                button.connect_clicked(move |_| {
                    if let Some(state) = weak.upgrade()
                        && let Err(error) = tasks::resume_session(&state, &session)
                    {
                        alert(&state.window, "Cannot resume session", &error);
                    }
                });
                linked.append(&button);
            }
            if count == 0 {
                note(
                    &linked,
                    "No sessions linked yet. Claude Code sessions link themselves; bookmark others.",
                );
            }
        },
    );
}
