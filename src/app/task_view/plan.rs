//! The task window's plan tab: a Markdown editor for the plan file, saved
//! only over the version it was read from.

use super::*;

fn entry_path(view: &View) -> PathBuf {
    let worktree = &view.ws.root_dir;
    optional_path(view.plan.entry.text().as_str(), worktree)
        .unwrap_or_else(|| saved(view).folder_or_default().join("plan.md"))
}

/// Updates the location, buttons and editor for the linked plan.
pub(super) fn refresh_plan(view: &Rc<View>) {
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
pub(super) fn set_editor(view: &View, text: &str) {
    set_editor_text(view, text);
    view.plan.disk.replace(None);
    set_base(view, Some(text.into()));
    view.plan.dirty.set(false);
    render_preview(view);
}

pub(super) fn set_base(view: &View, base: Option<String>) {
    view.plan.base.replace(base);
    view.plan.generation.set(view.plan.generation.get() + 1);
}

pub(super) fn update_dirty(view: &View) {
    let dirty = view.plan.base.borrow().as_deref() != Some(text(&view.plan.editor).as_str());
    view.plan.dirty.set(dirty);
    view.plan
        .status
        .set_label(if dirty { "Unsaved changes" } else { "" });
}

pub(super) fn render_preview(view: &View) {
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

pub(super) fn create_plan(view: &Rc<View>) {
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

pub(super) fn link_plan(view: &Rc<View>) {
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

pub(super) fn unlink_plan(view: &Rc<View>) {
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

pub(super) fn save_plan(view: &Rc<View>, done: impl FnOnce(bool) + 'static) {
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
pub(super) fn poll_plan(view: &Rc<View>) {
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
