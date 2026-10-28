//! Drag live main-tab layouts and panes without restarting their terminals.
//! Source widgets must stay put until GTK has finished the drag.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DragItem {
    MainTab(Uuid),
    Pane(Uuid),
}

impl DragItem {
    fn encode(self) -> String {
        match self {
            Self::MainTab(id) => format!("soi-main-tab:{id}"),
            Self::Pane(id) => format!("soi-pane:{id}"),
        }
    }

    fn decode(value: &glib::Value) -> Option<Self> {
        let text = value.get::<String>().ok()?;
        let (kind, id) = text.split_once(':')?;
        let id = Uuid::parse_str(id).ok()?;
        match kind {
            "soi-main-tab" => Some(Self::MainTab(id)),
            "soi-pane" => Some(Self::Pane(id)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

impl Edge {
    fn at(x: f64, y: f64, width: f64, height: f64) -> Self {
        if width <= 0.0 || height <= 0.0 {
            return Self::Center;
        }
        let (x, y) = (x / width, y / height);
        let (distance, edge) = [
            (x, Self::Left),
            (1.0 - x, Self::Right),
            (y, Self::Top),
            (1.0 - y, Self::Bottom),
        ]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap();
        if distance < 0.25 { edge } else { Self::Center }
    }
}

#[derive(Clone)]
pub(super) enum DropPlace {
    Tabs {
        workspace: Uuid,
        before: Option<Uuid>,
    },
    Pane {
        pane: glib::WeakRef<gtk::Box>,
        edge: Edge,
    },
}

#[derive(Default)]
pub(super) struct LayoutDrag {
    pub(super) pending: Option<(DragItem, Uuid, DropPlace)>,
    pub(super) ended: bool,
    pub(super) active: Option<DragItem>,
    hover: Option<(Uuid, Uuid, Uuid)>,
}

fn install_source(
    shared: &Rc<Shared>,
    widget: &impl IsA<gtk::Widget>,
    item: impl Fn() -> Option<DragItem> + 'static,
) {
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    let weak = Rc::downgrade(shared);
    source.connect_prepare(move |source, x, y| {
        // Header buttons still click; only the header/title starts a drag.
        if source
            .widget()
            .and_then(|w| w.pick(x, y, gtk::PickFlags::DEFAULT))
            .is_some_and(|w| {
                w.is::<gtk::Button>() || w.ancestor(gtk::Button::static_type()).is_some()
            })
        {
            return None;
        }
        let item = item()?;
        let shared = weak.upgrade()?;
        shared.layout_drag.replace(LayoutDrag {
            active: Some(item),
            ..Default::default()
        });
        Some(gdk::ContentProvider::for_value(&item.encode().to_value()))
    });
    let weak = Rc::downgrade(shared);
    source.connect_drag_begin(move |source, _| {
        if let Some(shared) = weak.upgrade() {
            let active = shared.layout_drag.borrow().active;
            shared.layout_drag.replace(LayoutDrag {
                active,
                ..Default::default()
            });
        }
        if let Some(widget) = source.widget() {
            source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&widget))), 0, 0);
        }
    });
    let weak = Rc::downgrade(shared);
    source.connect_drag_cancel(move |_, _, _| {
        if let Some(shared) = weak.upgrade() {
            shared.layout_drag.replace(LayoutDrag::default());
        }
        false
    });
    let weak = Rc::downgrade(shared);
    source.connect_drag_end(move |_, _, _| {
        if let Some(shared) = weak.upgrade() {
            {
                let mut drag = shared.layout_drag.borrow_mut();
                drag.ended = true;
                drag.active = None;
                drag.hover = None;
            }
            finish_layout_drop(&shared);
        }
    });
    widget.add_controller(source);
}

fn drop_target(shared: &Rc<Shared>) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    target.set_preload(true);
    let weak = Rc::downgrade(shared);
    target.connect_accept(move |_, _| {
        weak.upgrade()
            .is_some_and(|shared| shared.layout_drag.borrow().active.is_some())
    });
    target
}

pub(super) fn install_main_tab_drag(state: &App, tab: &MainTabUi) {
    let id = tab.id;
    tab.button.set_tooltip_text(Some(
        "Drag to reorder tabs or drop at a pane edge to move this layout",
    ));
    install_source(&state.shared, &tab.button, move || {
        Some(DragItem::MainTab(id))
    });
}

pub(super) fn install_pane_drag(state: &App, pane: &gtk::Box, header: &gtk::Box) {
    header.set_tooltip_text(Some(
        "Drag to move this pane; drop on the main tab bar to make it a tab",
    ));
    let (weak, pane_ref) = (Rc::downgrade(&state.shared), pane.downgrade());
    install_source(&state.shared, header, move || {
        let (shared, pane) = (weak.upgrade()?, pane_ref.upgrade()?);
        let state = window_of(&shared, &pane)?;
        let id = state.panes.borrow().get(&pane)?.current()?.id;
        Some(DragItem::Pane(id))
    });
    let target = drop_target(&state.shared);
    let (weak, pane_ref) = (Rc::downgrade(&state.shared), pane.downgrade());
    target.connect_motion(move |target, x, y| {
        let (Some(shared), Some(pane)) = (weak.upgrade(), pane_ref.upgrade()) else {
            return gdk::DragAction::empty();
        };
        let Some(state) = window_of(&shared, &pane) else {
            return gdk::DragAction::empty();
        };
        let Some(item) = target.value().as_ref().and_then(DragItem::decode) else {
            return gdk::DragAction::empty();
        };
        if !can_drop_on_pane(&shared, item, &pane) {
            return gdk::DragAction::empty();
        }
        if let Some(info) = state.panes.borrow().get(&pane) {
            show_pane_hint(
                &info.drop_hint,
                Some(Edge::at(x, y, pane.width() as f64, pane.height() as f64)),
            );
        }
        gdk::DragAction::MOVE
    });
    let (weak, pane_ref) = (Rc::downgrade(&state.shared), pane.downgrade());
    target.connect_leave(move |_| {
        if let (Some(shared), Some(pane)) = (weak.upgrade(), pane_ref.upgrade())
            && let Some(state) = window_of(&shared, &pane)
            && let Some(info) = state.panes.borrow().get(&pane)
        {
            show_pane_hint(&info.drop_hint, None);
        }
    });
    let (weak, pane_ref) = (Rc::downgrade(&state.shared), pane.downgrade());
    target.connect_drop(move |_, value, x, y| {
        let (Some(shared), Some(pane), Some(item)) =
            (weak.upgrade(), pane_ref.upgrade(), DragItem::decode(value))
        else {
            return false;
        };
        let Some(state) = window_of(&shared, &pane) else {
            return false;
        };
        if let Some(info) = state.panes.borrow().get(&pane) {
            show_pane_hint(&info.drop_hint, None);
        }
        if !can_drop_on_pane(&shared, item, &pane) {
            return false;
        }
        let edge = Edge::at(x, y, pane.width() as f64, pane.height() as f64);
        queue_drop(
            &shared,
            item,
            state.id.get(),
            DropPlace::Pane {
                pane: pane.downgrade(),
                edge,
            },
        );
        true
    });
    pane.add_controller(target);
}

fn show_pane_hint(hint: &gtk::Box, edge: Option<Edge>) {
    let Some(edge) = edge else {
        hint.set_visible(false);
        return;
    };
    let (width, height) = hint.parent().map_or((0, 0), |p| (p.width(), p.height()));
    let (horizontal, vertical, width, height) = match edge {
        Edge::Left => (gtk::Align::Start, gtk::Align::Fill, width / 2, -1),
        Edge::Right => (gtk::Align::End, gtk::Align::Fill, width / 2, -1),
        Edge::Top => (gtk::Align::Fill, gtk::Align::Start, -1, height / 2),
        Edge::Bottom => (gtk::Align::Fill, gtk::Align::End, -1, height / 2),
        Edge::Center => (gtk::Align::Fill, gtk::Align::Fill, -1, -1),
    };
    hint.set_halign(horizontal);
    hint.set_valign(vertical);
    hint.set_size_request(width, height);
    hint.set_visible(true);
}

fn strip_anchor(ws: &WorkspaceUi, x: f64) -> Option<Uuid> {
    ws.tabs
        .borrow()
        .iter()
        .find(|tab| {
            tab.button
                .compute_bounds(&ws.tab_strip)
                .is_some_and(|b| x < (b.x() + b.width() / 2.0) as f64)
        })
        .map(|tab| tab.id)
}

fn strip_hint(ws: &WorkspaceUi, before: Option<Uuid>, visible: bool) {
    ws.tab_strip.remove_css_class("drop-end");
    for tab in ws.tabs.borrow().iter() {
        tab.button.remove_css_class("drop-before");
        if visible && Some(tab.id) == before {
            tab.button.add_css_class("drop-before");
        }
    }
    if visible && before.is_none() {
        ws.tab_strip.add_css_class("drop-end");
    }
}

pub(super) fn install_tab_strip_drop(state: &App, workspace: Uuid) {
    let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
        return;
    };
    let target = drop_target(&state.shared);
    let (weak, holder) = (Rc::downgrade(&state.shared), ws.holder.downgrade());
    target.connect_motion(move |target, x, _| {
        let (Some(shared), Some(holder)) = (weak.upgrade(), holder.upgrade()) else {
            return gdk::DragAction::empty();
        };
        let Some(state) = window_of(&shared, &holder) else {
            return gdk::DragAction::empty();
        };
        let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
            return gdk::DragAction::empty();
        };
        let Some(item) = target.value().as_ref().and_then(DragItem::decode) else {
            return gdk::DragAction::empty();
        };
        if find_source(&shared, item).is_none() {
            return gdk::DragAction::empty();
        }
        strip_hint(&ws, strip_anchor(&ws, x), true);
        let hovered = ws
            .tabs
            .borrow()
            .iter()
            .find(|tab| {
                tab.button
                    .compute_bounds(&ws.tab_strip)
                    .is_some_and(|bounds| {
                        x >= bounds.x() as f64 && x < (bounds.x() + bounds.width()) as f64
                    })
            })
            .map(|tab| tab.id);
        hover_tab(&shared, &state, workspace, hovered);
        gdk::DragAction::MOVE
    });
    let (weak, holder) = (Rc::downgrade(&state.shared), ws.holder.downgrade());
    target.connect_leave(move |_| {
        if let (Some(shared), Some(holder)) = (weak.upgrade(), holder.upgrade())
            && let Some(state) = window_of(&shared, &holder)
            && let Some(ws) = state.workspaces.borrow().get(&workspace)
        {
            strip_hint(ws, None, false);
            shared.layout_drag.borrow_mut().hover = None;
        }
    });
    let (weak, holder) = (Rc::downgrade(&state.shared), ws.holder.downgrade());
    target.connect_drop(move |_, value, x, _| {
        let (Some(shared), Some(holder), Some(item)) =
            (weak.upgrade(), holder.upgrade(), DragItem::decode(value))
        else {
            return false;
        };
        let Some(state) = window_of(&shared, &holder) else {
            return false;
        };
        let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
            return false;
        };
        strip_hint(&ws, None, false);
        if find_source(&shared, item).is_none() {
            return false;
        }
        queue_drop(
            &shared,
            item,
            state.id.get(),
            DropPlace::Tabs {
                workspace,
                before: strip_anchor(&ws, x),
            },
        );
        true
    });
    ws.tab_strip.add_controller(target);
}

/// Sidebar workspace rows accept live panes/layouts as new main tabs.
pub(super) fn install_workspace_drop(state: &App, row: &gtk::ListBoxRow, workspace: Uuid) {
    let target = drop_target(&state.shared);
    let (weak, row_ref) = (Rc::downgrade(&state.shared), row.downgrade());
    target.connect_motion(move |target, _, _| {
        let (Some(shared), Some(row)) = (weak.upgrade(), row_ref.upgrade()) else {
            return gdk::DragAction::empty();
        };
        let Some(state) = window_of(&shared, &row) else {
            return gdk::DragAction::empty();
        };
        let Some(item) = target.value().as_ref().and_then(DragItem::decode) else {
            return gdk::DragAction::empty();
        };
        if find_source(&shared, item).is_none() {
            return gdk::DragAction::empty();
        }
        let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
            return gdk::DragAction::empty();
        };
        row.add_css_class("drop-into");
        hover_tab(&shared, &state, workspace, Some(ws.current_tab().id));
        gdk::DragAction::MOVE
    });
    let (weak, row_ref) = (Rc::downgrade(&state.shared), row.downgrade());
    target.connect_leave(move |_| {
        if let Some(row) = row_ref.upgrade() {
            row.remove_css_class("drop-into");
        }
        if let Some(shared) = weak.upgrade() {
            shared.layout_drag.borrow_mut().hover = None;
        }
    });
    let (weak, row_ref) = (Rc::downgrade(&state.shared), row.downgrade());
    target.connect_drop(move |_, value, _, _| {
        let (Some(shared), Some(row), Some(item)) =
            (weak.upgrade(), row_ref.upgrade(), DragItem::decode(value))
        else {
            return false;
        };
        row.remove_css_class("drop-into");
        let Some(state) = window_of(&shared, &row) else {
            return false;
        };
        if find_source(&shared, item).is_none() {
            return false;
        }
        queue_drop(
            &shared,
            item,
            state.id.get(),
            DropPlace::Tabs {
                workspace,
                before: None,
            },
        );
        true
    });
    row.add_controller(target);
}

fn hover_tab(shared: &Rc<Shared>, state: &App, workspace: Uuid, tab: Option<Uuid>) {
    let hover = tab.map(|tab| (state.id.get(), workspace, tab));
    if shared.layout_drag.borrow().hover == hover {
        return;
    }
    shared.layout_drag.borrow_mut().hover = hover;
    let Some((window, workspace, tab)) = hover else {
        return;
    };
    let weak = Rc::downgrade(shared);
    glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
        let Some(shared) = weak.upgrade() else { return };
        if shared.layout_drag.borrow().hover != Some((window, workspace, tab)) {
            return;
        }
        let Some(state) = shared
            .windows()
            .into_iter()
            .find(|state| state.id.get() == window)
        else {
            return;
        };
        let Some(ws) = state.workspaces.borrow().get(&workspace).cloned() else {
            return;
        };
        let Some(index) = ws.tabs.borrow().iter().position(|main| main.id == tab) else {
            return;
        };
        ws.active_tab.set(index);
        ws.tab_stack.set_visible_child(&ws.current_tab().layout);
        if state.active.get() != Some(workspace) {
            state.active.set(Some(workspace));
            state.stack.set_visible_child(&ws.holder);
            let rebuilding = state.rebuilding.replace(true);
            state
                .list
                .select_row(find_row(&state, &workspace.to_string()).as_ref());
            state.rebuilding.set(rebuilding);
            let group = group_of(&state.tree.borrow(), workspace);
            state.last_in_group.borrow_mut().insert(group, workspace);
            update_title(&state);
        }
        refresh_main_tabs(&ws);
        // Do not grab keyboard focus while GTK owns the drag.
    });
}

fn queue_drop(shared: &Rc<Shared>, item: DragItem, window: Uuid, place: DropPlace) {
    shared.layout_drag.borrow_mut().pending = Some((item, window, place));
    finish_layout_drop(shared);
}

pub(super) fn finish_layout_drop(shared: &Rc<Shared>) {
    let pending = {
        let mut drag = shared.layout_drag.borrow_mut();
        if !drag.ended || drag.pending.is_none() {
            return;
        }
        std::mem::take(&mut *drag).pending
    };
    let Some((item, window, place)) = pending else {
        return;
    };
    let weak = Rc::downgrade(shared);
    glib::idle_add_local_once(move || {
        if let Some(shared) = weak.upgrade()
            && let Some(state) = shared.windows().into_iter().find(|w| w.id.get() == window)
        {
            move_layout(&shared, item, &state, place);
        }
    });
}

struct Source {
    state: App,
    workspace: Uuid,
    tab: Rc<MainTabUi>,
    pane: Option<gtk::Box>,
}

fn find_source(shared: &Rc<Shared>, item: DragItem) -> Option<Source> {
    match item {
        DragItem::MainTab(id) => {
            for state in shared.windows() {
                for (&workspace, ws) in state.workspaces.borrow().iter() {
                    if let Some(tab) = ws.tabs.borrow().iter().find(|tab| tab.id == id).cloned() {
                        return Some(Source {
                            state: state.clone(),
                            workspace,
                            tab,
                            pane: None,
                        });
                    }
                }
            }
            None
        }
        DragItem::Pane(id) => {
            let terminal = shared.tabs.borrow().get(&id)?.upgrade()?;
            let state = window_of(shared, terminal.terminal.widget())?;
            let pane = pane_of(&state, terminal.terminal.widget())?;
            let (workspace, ws) = workspace_of(&state, &pane)?;
            let tab = main_tab_of(&ws, &pane)?;
            Some(Source {
                state,
                workspace,
                tab,
                pane: Some(pane),
            })
        }
    }
}

fn can_drop_on_pane(shared: &Rc<Shared>, item: DragItem, pane: &gtk::Box) -> bool {
    let Some(source) = find_source(shared, item) else {
        return false;
    };
    match source.pane {
        Some(from) => from != *pane,
        None => !pane.is_ancestor(&source.tab.layout),
    }
}

/// Validates first, then reparents existing GTK widgets. No terminal is rebuilt.
pub(super) fn move_layout(shared: &Rc<Shared>, item: DragItem, to: &App, mut place: DropPlace) {
    let Some(source) = find_source(shared, item) else {
        return;
    };
    if let DropPlace::Pane {
        pane,
        edge: Edge::Center,
    } = &place
    {
        let Some(pane) = pane.upgrade() else { return };
        if !can_drop_on_pane(shared, item, &pane) {
            return;
        }
        let Some((workspace, _)) = workspace_of(to, &pane) else {
            return;
        };
        place = DropPlace::Tabs {
            workspace,
            before: None,
        };
    }
    let (workspace, destination_tab) = match &place {
        DropPlace::Tabs { workspace, before } => {
            let Some(ws) = to.workspaces.borrow().get(workspace).cloned() else {
                return;
            };
            if before.is_some_and(|id| !ws.tabs.borrow().iter().any(|tab| tab.id == id)) {
                return;
            }
            if Rc::ptr_eq(&source.state, to)
                && source.workspace == *workspace
                && (source.pane.is_none() || tab_panes(&source.tab).len() == 1)
            {
                reorder_main_tab(to, *workspace, source.tab.id, *before);
                save_all(shared);
                return;
            }
            (*workspace, None)
        }
        DropPlace::Pane { pane, .. } => {
            let Some(pane) = pane.upgrade() else { return };
            if !can_drop_on_pane(shared, item, &pane) || !to.panes.borrow().contains_key(&pane) {
                return;
            }
            let Some((workspace, ws)) = workspace_of(to, &pane) else {
                return;
            };
            let Some(tab) = main_tab_of(&ws, &pane) else {
                return;
            };
            unzoom_around(to, &pane);
            (workspace, Some((pane, tab.id)))
        }
    };
    let Some((root, saved)) = take_source(&source) else {
        return;
    };
    if !Rc::ptr_eq(&source.state, to) {
        hand_over(&source.state, to, &root);
    }
    match place {
        DropPlace::Tabs { before, .. } => {
            add_main_tab_layout(to, workspace, &saved, &root);
            reorder_main_tab(to, workspace, saved.id, before);
        }
        DropPlace::Pane { edge, .. } => {
            let Some((pane, tab_id)) = destination_tab else {
                return;
            };
            let Some(slot) = Slot::take(pane.upcast_ref()) else {
                return;
            };
            let orientation = if matches!(edge, Edge::Left | Edge::Right) {
                gtk::Orientation::Horizontal
            } else {
                gtk::Orientation::Vertical
            };
            let split = if matches!(edge, Edge::Left | Edge::Top) {
                make_paned(to, orientation, 0.5, &root, &pane)
            } else {
                make_paned(to, orientation, 0.5, &pane, &root)
            };
            slot.fill(&split);
            select_main_tab_id(to, workspace, tab_id);
            let mut panes = Vec::new();
            collect_panes(&root, &mut panes);
            if let Some(pane) = panes.get(saved.focused_pane).or(panes.first()) {
                focus_pane(to, pane);
            }
        }
    }
    save_all(shared);
    panel::refresh(shared);
}

fn take_source(source: &Source) -> Option<(gtk::Widget, crate::session::WorkspaceTab)> {
    if source.pane.is_none() || tab_panes(&source.tab).len() == 1 {
        return take_main_tab(&source.state, source.workspace, source.tab.id);
    }
    let pane = source.pane.as_ref()?;
    unzoom_around(&source.state, pane);
    let parent = pane.parent()?.downcast::<gtk::Paned>().ok()?;
    let sibling = if parent.start_child().as_ref() == Some(pane.upcast_ref()) {
        parent.end_child()?
    } else {
        parent.start_child()?
    };
    let slot = Slot::take(parent.upcast_ref())?;
    parent.set_start_child(gtk::Widget::NONE);
    parent.set_end_child(gtk::Widget::NONE);
    source.state.ratios.borrow_mut().remove(&parent);
    slot.fill(&sibling);
    if source.tab.last_focus.upgrade().as_ref() == Some(pane) {
        source.tab.last_focus.set(first_pane(&sibling).as_ref());
    }
    let name = source
        .state
        .panes
        .borrow()
        .get(pane)?
        .current()?
        .title
        .text()
        .to_string();
    Some((
        pane.clone().upcast(),
        crate::session::WorkspaceTab {
            id: Uuid::new_v4(),
            name,
            layout: Layout::pane(home_dir()),
            focused_pane: 0,
        },
    ))
}
