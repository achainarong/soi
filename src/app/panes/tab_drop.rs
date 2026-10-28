//! Dragging a tab onto a pane: beside it, or in among its tabs.

use super::*;

/// What a dragged tab carries: this, then the tab's id.
pub(in crate::app) const TAB_DRAG: &str = "tab:";

/// Where a tab dropped on a pane goes: beside it, splitting the pane, or
/// into it as one more tab: last (dropped on the terminal) or at a place
/// among the tabs (dropped on the tab strip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum DropZone {
    Left,
    Right,
    Top,
    Bottom,
    Center,
    Tabs(usize),
}

impl DropZone {
    /// For a point in a pane of `width` × `height`: the outer quarter on
    /// each side splits there, the middle adds a tab.
    fn at(x: f64, y: f64, width: f64, height: f64) -> Self {
        if width <= 0.0 || height <= 0.0 {
            return DropZone::Center;
        }
        let (fx, fy) = (x / width, y / height);
        let (distance, zone) = [
            (fx, DropZone::Left),
            (1.0 - fx, DropZone::Right),
            (fy, DropZone::Top),
            (1.0 - fy, DropZone::Bottom),
        ]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("four sides");
        if distance < 0.25 {
            zone
        } else {
            DropZone::Center
        }
    }
}

/// Where on `pane` a tab dropped at `x`, `y` (pane coordinates) goes. On
/// the header, among the tabs, before the first whose middle is right of
/// the point; below it, by the terminal area (see `DropZone::at`).
pub(in crate::app) fn drop_zone(state: &App, pane: &gtk::Box, x: f64, y: f64) -> DropZone {
    let header = pane
        .first_child()
        .map_or(0.0, |header| header.height() as f64);
    if y < header {
        let panes = state.panes.borrow();
        let tabs = panes
            .get(pane)
            .map(|info| info.tabs.borrow().clone())
            .unwrap_or_default();
        let before = tabs
            .iter()
            .filter(|tab| {
                tab.button
                    .compute_bounds(pane)
                    .is_some_and(|b| (b.x() + b.width() / 2.0) as f64 <= x)
            })
            .count();
        return DropZone::Tabs(before);
    }
    let height = pane.height() as f64 - header;
    DropZone::at(x, y - header, pane.width() as f64, height)
}

/// Lets tabs be dropped on a pane, showing where they'd go meanwhile.
pub(in crate::app) fn install_tab_drop(state: &App, pane: &gtk::Box) {
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    target.set_preload(true);
    let dragged = |value: Option<&glib::Value>| {
        let text = value?.get::<String>().ok()?;
        Uuid::parse_str(text.strip_prefix(TAB_DRAG)?).ok()
    };
    let (shared, pane_ref) = (Rc::downgrade(&state.shared), pane.downgrade());
    let hint = move |zone: Option<DropZone>| {
        let (Some(shared), Some(pane)) = (shared.upgrade(), pane_ref.upgrade()) else {
            return;
        };
        let Some(state) = window_of(&shared, &pane) else {
            return;
        };
        if let Some(info) = state.panes.borrow().get(&pane) {
            show_drop_zone(&info.drop_hint, zone);
        }
    };
    let (show, shared, pane_ref) = (hint.clone(), Rc::downgrade(&state.shared), pane.downgrade());
    target.connect_motion(move |target, x, y| {
        let (Some(shared), Some(pane)) = (shared.upgrade(), pane_ref.upgrade()) else {
            return gdk::DragAction::empty();
        };
        let Some(state) = window_of(&shared, &pane) else {
            return gdk::DragAction::empty();
        };
        if dragged(target.value().as_ref()).is_none() {
            return gdk::DragAction::empty();
        }
        show(Some(drop_zone(&state, &pane, x, y)));
        gdk::DragAction::MOVE
    });
    let hide = hint.clone();
    target.connect_leave(move |_| hide(None));
    let (shared, pane_ref) = (Rc::downgrade(&state.shared), pane.downgrade());
    target.connect_drop(move |_, value, x, y| {
        hint(None);
        let (Some(shared), Some(pane), Some(id)) =
            (shared.upgrade(), pane_ref.upgrade(), dragged(Some(value)))
        else {
            return false;
        };
        let Some(state) = window_of(&shared, &pane) else {
            return false;
        };
        if !shared.tabs.borrow().contains_key(&id) {
            return false;
        }
        let zone = drop_zone(&state, &pane, x, y);
        shared.tab_drop.borrow_mut().target = Some((id, pane.downgrade(), zone));
        finish_tab_drop(&shared);
        true
    });
    pane.add_controller(target);
}

/// A tab drag onto a pane. The tab moves only once both the drop and the
/// end of the drag have arrived: moving its button while GTK still drags
/// it would leave the drag unfinished, and no tab could be dragged again.
#[derive(Default)]
pub(in crate::app) struct TabDrop {
    /// The tab, the pane it was dropped on, and where on it.
    pub(in crate::app) target: Option<(Uuid, glib::WeakRef<gtk::Box>, DropZone)>,
    /// The drag is over (dropped or cancelled).
    pub(in crate::app) ended: bool,
}

/// Moves the dropped tab once its drag is over (see `TabDrop`).
pub(in crate::app) fn finish_tab_drop(shared: &Rc<Shared>) {
    let target = {
        let mut drop = shared.tab_drop.borrow_mut();
        if !drop.ended || drop.target.is_none() {
            return;
        }
        std::mem::take(&mut *drop).target
    };
    let Some((id, pane, zone)) = target else {
        return;
    };
    let weak = Rc::downgrade(shared);
    glib::idle_add_local_once(move || {
        let Some(shared) = weak.upgrade() else { return };
        let tab = shared.tabs.borrow().get(&id).and_then(Weak::upgrade);
        if let (Some(tab), Some(pane)) = (tab, pane.upgrade())
            && let Some(state) = window_of(&shared, &pane)
        {
            move_tab(&state, &tab, &pane, zone);
        }
    });
}

/// Covers the part of a pane's terminal area where a drop would go.
pub(in crate::app) fn show_drop_zone(hint: &gtk::Box, zone: Option<DropZone>) {
    let Some(zone) = zone else {
        hint.set_visible(false);
        return;
    };
    let area = hint.parent().map_or((0, 0), |p| (p.width(), p.height()));
    let (halign, valign, width, height) = match zone {
        DropZone::Left => (gtk::Align::Start, gtk::Align::Fill, area.0 / 2, -1),
        DropZone::Right => (gtk::Align::End, gtk::Align::Fill, area.0 / 2, -1),
        DropZone::Top => (gtk::Align::Fill, gtk::Align::Start, -1, area.1 / 2),
        DropZone::Bottom => (gtk::Align::Fill, gtk::Align::End, -1, area.1 / 2),
        DropZone::Center | DropZone::Tabs(_) => (gtk::Align::Fill, gtk::Align::Fill, -1, -1),
    };
    hint.set_halign(halign);
    hint.set_valign(valign);
    hint.set_size_request(width, height);
    hint.set_visible(true);
}

/// Moves a tab, with its running terminal, to `target`: as one more tab
/// there, or as a new pane beside it. A pane left without tabs closes.
pub(in crate::app) fn move_tab(state: &App, tab: &Rc<Tab>, target: &gtk::Box, zone: DropZone) {
    let Some(source) = pane_of(state, tab.terminal.widget()) else {
        return;
    };
    let (from, to) = {
        let panes = state.panes.borrow();
        (panes.get(&source).cloned(), panes.get(target).cloned())
    };
    let (Some(from), Some(_)) = (from, to) else {
        return;
    };
    let alone = from.tabs.borrow().len() == 1;
    let Some(index) = tab_index(state, &source, tab) else {
        return;
    };
    // Onto its own pane: a lone tab stays; among the tabs, it moves there.
    if source == *target {
        match zone {
            DropZone::Center => return,
            _ if alone => return,
            DropZone::Tabs(at) => {
                reorder_tab(state, &source, index, at);
                return;
            }
            _ => {}
        }
    }

    unzoom_around(state, &source);
    unzoom_around(state, target);
    from.tabs.borrow_mut().remove(index);
    from.strip.remove(&tab.button);
    from.stack.remove(tab.terminal.widget());
    if alone {
        close_pane(state, &source);
    } else {
        let active = from.active.get();
        update_tab_strip(&from);
        activate_tab(
            state,
            &source,
            if index < active { active - 1 } else { active },
            false,
        );
    }

    let dest = if matches!(zone, DropZone::Center | DropZone::Tabs(_)) {
        target.clone()
    } else {
        let new = new_pane(state, &[], &[], 0);
        let Some(slot) = Slot::take(target.upcast_ref()) else {
            return;
        };
        let paned = match zone {
            DropZone::Left => make_paned(state, gtk::Orientation::Horizontal, 0.5, &new, target),
            DropZone::Right => make_paned(state, gtk::Orientation::Horizontal, 0.5, target, &new),
            DropZone::Top => make_paned(state, gtk::Orientation::Vertical, 0.5, &new, target),
            _ => make_paned(state, gtk::Orientation::Vertical, 0.5, target, &new),
        };
        slot.fill(&paned);
        new
    };
    let Some(info) = state.panes.borrow().get(&dest).cloned() else {
        return;
    };
    let count = info.tabs.borrow().len();
    let index = match zone {
        DropZone::Tabs(at) => at.min(count),
        _ => count,
    };
    let before = index
        .checked_sub(1)
        .map(|i| info.tabs.borrow()[i].button.clone());
    info.tabs.borrow_mut().insert(index, tab.clone());
    info.strip.insert_child_after(&tab.button, before.as_ref());
    info.stack.add_child(tab.terminal.widget());
    update_tab_strip(&info);
    set_focused(state, &dest);
    activate_tab(state, &dest, index, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_near_an_edge_split_and_in_the_middle_add_a_tab() {
        let at = |x, y| DropZone::at(x, y, 400.0, 200.0);
        assert_eq!(at(20.0, 100.0), DropZone::Left);
        assert_eq!(at(390.0, 100.0), DropZone::Right);
        assert_eq!(at(200.0, 10.0), DropZone::Top);
        assert_eq!(at(200.0, 190.0), DropZone::Bottom);
        assert_eq!(at(200.0, 100.0), DropZone::Center);
        // In a corner, the nearer edge wins (relative to the pane's size).
        assert_eq!(at(10.0, 40.0), DropZone::Left);
        assert_eq!(at(60.0, 5.0), DropZone::Top);
        assert_eq!(DropZone::at(5.0, 5.0, 0.0, 0.0), DropZone::Center);
    }
}
