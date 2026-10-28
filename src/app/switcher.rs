//! The quick switcher (Ctrl+Shift+P): type part of a workspace's or agent's
//! name, in any window, and jump there with Enter.

use super::*;
use crate::agents::State as AgentState;

/// What a row in the switcher goes to.
#[derive(Clone)]
enum Target {
    Workspace {
        window: Uuid,
        workspace: Uuid,
    },
    Agent(Uuid),
    Session {
        window: Uuid,
        session: crate::tasks::AgentSession,
    },
}

struct Choice {
    title: String,
    detail: String,
    target: Target,
}

/// Opens the switcher over the window's workspaces.
pub(super) fn open(state: &App) {
    let choices = Rc::new(RefCell::new(choices(&state.shared, state)));
    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Go to a workspace, task, agent or saved session…")
        .build();
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(360)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_width_request(420);
    content.append(&entry);
    content.append(&scrolled);
    let popover = gtk::Popover::builder()
        .child(&content)
        .has_arrow(false)
        .position(gtk::PositionType::Bottom)
        .build();

    // Rows in the order shown, each with its choice's index.
    let shown: Rc<RefCell<Vec<usize>>> = Rc::default();
    let fill: Rc<dyn Fn(&str)> = {
        let (list, choices, shown) = (list.clone(), choices.clone(), shown.clone());
        Rc::new(move |query: &str| {
            list.remove_all();
            let choices = choices.borrow();
            let order = ranked(query, &choices);
            for &i in &order {
                list.append(&row(&choices[i]));
            }
            list.select_row(list.row_at_index(0).as_ref());
            shown.replace(order);
        })
    };
    fill("");
    let go = {
        let (shared, popover, choices, shown) = (
            Rc::downgrade(&state.shared),
            popover.clone(),
            choices.clone(),
            shown.clone(),
        );
        move |index: Option<i32>| {
            let choice = index
                .and_then(|i| shown.borrow().get(usize::try_from(i).ok()?).copied())
                .map(|i| choices.borrow()[i].target.clone());
            popover.popdown();
            if let (Some(shared), Some(target)) = (shared.upgrade(), choice) {
                go_to(&shared, target);
            }
        }
    };
    let changed = fill.clone();
    entry.connect_search_changed(move |entry| changed(&entry.text()));
    let activate = go.clone();
    let rows = list.clone();
    entry.connect_activate(move |_| activate(rows.selected_row().map(|row| row.index())));
    list.connect_row_activated(move |_, row| go(Some(row.index())));
    let popdown = popover.clone();
    entry.connect_stop_search(move |_| popdown.popdown());
    // Up and Down move through the list while typing.
    let keys = gtk::EventControllerKey::new();
    let (rows, view) = (list.clone(), scrolled.clone());
    keys.connect_key_pressed(move |_, key, _, _| {
        let delta = match key {
            gdk::Key::Down | gdk::Key::KP_Down => 1,
            gdk::Key::Up | gdk::Key::KP_Up => -1,
            _ => return glib::Propagation::Proceed,
        };
        let at = rows.selected_row().map_or(-1, |row| row.index());
        if let Some(row) = rows.row_at_index((at + delta).max(0)) {
            // Only the highlight moves; typing stays in the entry.
            rows.select_row(Some(&row));
            if let Some(bounds) = row.compute_bounds(&rows) {
                let (top, height) = (bounds.y() as f64, bounds.height() as f64);
                view.vadjustment().clamp_page(top, top + height);
            }
        }
        glib::Propagation::Stop
    });
    entry.add_controller(keys);

    let parent = &state.stack;
    let width = parent.width();
    show_popover(parent, &popover, gdk::Rectangle::new(width / 2, 0, 1, 1));
    entry.grab_focus();
    // Show live workspaces immediately. Disk history must not delay the
    // switcher or reopen a dismissed popover when a slow read completes.
    let (weak_popover, weak_entry, window) =
        (popover.downgrade(), entry.downgrade(), state.id.get());
    tasks::background(
        || crate::tasks::History::load(&crate::tasks::history_path()),
        move |result| {
            let (Some(popover), Some(entry)) = (weak_popover.upgrade(), weak_entry.upgrade())
            else {
                return;
            };
            if !popover.is_visible() {
                return;
            }
            if let Ok(history) = result {
                for session in history.sessions {
                    choices.borrow_mut().push(Choice {
                        title: session.title.clone(),
                        detail: format!(
                            "Saved session · {} · {} · {}",
                            session.provider,
                            session.session_id,
                            session.directory.display()
                        ),
                        target: Target::Session { window, session },
                    });
                }
                fill(entry.text().as_str());
            }
        },
    );
}

fn row(choice: &Choice) -> gtk::ListBoxRow {
    let title = gtk::Label::builder()
        .label(&choice.title)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["ws-name"])
        .build();
    let detail = gtk::Label::builder()
        .label(&choice.detail)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::Middle)
        .css_classes(["dim-label", "caption"])
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_margin_top(2);
    content.set_margin_bottom(2);
    content.append(&title);
    content.append(&detail);
    gtk::ListBoxRow::builder().child(&content).build()
}

/// Every workspace (this window's first) and every agent.
fn choices(shared: &Shared, current: &App) -> Vec<Choice> {
    let mut windows = shared.windows();
    windows.sort_by_key(|window| !Rc::ptr_eq(window, current));
    let several = windows.len() > 1;
    let mut choices = Vec::new();
    for window in &windows {
        let workspaces = window.workspaces.borrow();
        for id in workspace_order(window) {
            let Some(ws) = workspaces.get(&id) else {
                continue;
            };
            let mut detail = tilde(&ws.root_dir);
            if several {
                detail = format!("{} · {detail}", window.name.borrow());
            }
            if let Some(task) = ws.task.borrow().as_ref() {
                detail = format!("{} · {} · {detail}", task.title, task.status.label());
            }
            choices.push(Choice {
                title: ws.name.borrow().clone(),
                detail,
                target: Target::Workspace {
                    window: window.id.get(),
                    workspace: id,
                },
            });
        }
    }
    for (tab, agent) in panel::sorted_agents(shared) {
        let program = agent.program.clone().unwrap_or_default();
        let place = panel::workspace_name(shared, &tab).unwrap_or_default();
        let state = match agent.state {
            AgentState::Input => "needs input",
            AgentState::Done => "done",
            AgentState::Working => "working",
            AgentState::Idle => "idle",
            AgentState::Exited => "exited",
        };
        choices.push(Choice {
            title: format!("{program} in {place}"),
            detail: format!("Agent · {state}"),
            target: Target::Agent(tab.id),
        });
    }
    choices
}

fn go_to(shared: &Rc<Shared>, target: Target) {
    match target {
        Target::Workspace { window, workspace } => {
            if let Some(state) = shared.windows().into_iter().find(|w| w.id.get() == window) {
                select_workspace(&state, workspace);
                state.window.present();
            }
        }
        Target::Agent(tab) => panel::focus_tab(shared, tab),
        Target::Session { window, session } => {
            if let Some(state) = shared.windows().into_iter().find(|w| w.id.get() == window)
                && let Err(error) = tasks::resume_session(&state, &session)
            {
                gtk::AlertDialog::builder()
                    .message("Cannot resume session")
                    .detail(error)
                    .build()
                    .show(Some(&state.window));
            }
        }
    }
}

/// The choices that match `query`, best first; all of them for no query.
fn ranked(query: &str, choices: &[Choice]) -> Vec<usize> {
    let mut scored: Vec<(u32, usize)> = choices
        .iter()
        .enumerate()
        .filter_map(|(i, choice)| {
            let title = score(query, &choice.title);
            let detail = score(query, &choice.detail).map(|s| s / 2);
            Some((title.max(detail)?, i))
        })
        .collect();
    // Stable: equally good matches keep their order.
    scored.sort_by_key(|&(score, i)| (std::cmp::Reverse(score), i));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// How well `text` matches `query`, ignoring case: a prefix beats a word
/// start, which beats anywhere inside, which beats the letters in order
/// with gaps ("tf2" finds "Test Feature 2"). `None`: no match.
fn score(query: &str, text: &str) -> Option<u32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(1);
    }
    let text = text.to_lowercase();
    if text.starts_with(&query) {
        return Some(400);
    }
    if let Some(at) = text.find(&query) {
        let word_start = text[..at].ends_with(|c: char| !c.is_alphanumeric());
        return Some(if word_start { 300 } else { 200 });
    }
    let mut chars = text.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|q| chars.any(|c| c == q))
        .then_some(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_prefixes_words_substrings_and_letters_in_order() {
        assert_eq!(score("", "anything"), Some(1));
        assert_eq!(score("ter", "Terminal 2"), Some(400));
        assert_eq!(score("feat", "Claude Feature 1"), Some(300));
        assert_eq!(score("ature", "Claude Feature 1"), Some(200));
        assert_eq!(score("cf1", "Claude Feature 1"), Some(100));
        assert_eq!(score("CLAUDE", "claude"), Some(400));
        assert_eq!(score("xyz", "Claude Feature 1"), None);
        assert_eq!(score("1fc", "Claude Feature 1"), None, "in order only");
    }

    #[test]
    fn ranks_best_matches_first_and_keeps_order_otherwise() {
        let choice = |title: &str, detail: &str| Choice {
            title: title.into(),
            detail: detail.into(),
            target: Target::Agent(Uuid::nil()),
        };
        let choices = [
            choice("Renderer", "~/code/render"),
            choice("Terminal", "~"),
            choice("Opencode Feature 2", "~/code"),
            choice("Test 2", "~/test"),
        ];
        assert_eq!(ranked("", &choices), [0, 1, 2, 3]);
        // "Opencode Feature 2" has t…e in "Feature".
        assert_eq!(ranked("te", &choices), [1, 3, 2]);
        assert_eq!(ranked("code", &choices), [2, 0], "titles beat details");
        assert!(ranked("zzz", &choices).is_empty());
    }
}
