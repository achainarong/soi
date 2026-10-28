//! Rearranging workspaces and groups by dragging them in the sidebar.

use super::*;

/// A sidebar entry; dragged as the string `ws:<uuid>` or `group:<uuid>`.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::app) enum Item {
    Workspace(Uuid),
    Group(Uuid),
}

impl Item {
    fn encode(self) -> String {
        match self {
            Item::Workspace(id) => format!("ws:{id}"),
            Item::Group(id) => format!("group:{id}"),
        }
    }

    fn decode(s: &str) -> Option<Item> {
        match s.strip_prefix("ws:") {
            Some(id) => Uuid::parse_str(id).ok().map(Item::Workspace),
            None => Uuid::parse_str(s.strip_prefix("group:")?)
                .ok()
                .map(Item::Group),
        }
    }

    pub(in crate::app) fn of_row(row: &gtk::ListBoxRow) -> Option<Item> {
        group_id_of_row(row).map(Item::Group).or_else(|| {
            Uuid::parse_str(&row.widget_name())
                .ok()
                .map(Item::Workspace)
        })
    }

    fn row_name(self) -> String {
        match self {
            Item::Workspace(id) => id.to_string(),
            Item::Group(id) => format!("group:{id}"),
        }
    }
}

/// Where a dropped entry lands.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::app) enum Anchor {
    Before(Item),
    After(Item),
    /// At the end of a group.
    Into(Uuid),
    /// At the end of the sidebar.
    End,
}

/// Position of an entry in the sidebar tree.
pub(in crate::app) enum Loc {
    Top(usize),
    InGroup(usize, usize),
}

pub(in crate::app) fn locate(tree: &[Node], item: Item) -> Option<Loc> {
    for (i, node) in tree.iter().enumerate() {
        match (node, item) {
            (Node::Workspace(w), Item::Workspace(id)) if *w == id => return Some(Loc::Top(i)),
            (Node::Group { id: g, .. }, Item::Group(id)) if *g == id => return Some(Loc::Top(i)),
            (Node::Group { workspaces, .. }, Item::Workspace(id)) => {
                if let Some(j) = workspaces.iter().position(|w| *w == id) {
                    return Some(Loc::InGroup(i, j));
                }
            }
            _ => {}
        }
    }
    None
}

pub(in crate::app) fn group_of(tree: &[Node], ws: Uuid) -> Option<Uuid> {
    match locate(tree, Item::Workspace(ws))? {
        Loc::InGroup(i, _) => match &tree[i] {
            Node::Group { id, .. } => Some(*id),
            Node::Workspace(_) => None,
        },
        Loc::Top(_) => None,
    }
}

pub(in crate::app) fn attach_drag(row: &gtk::ListBoxRow, item: Item) {
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .content(&gdk::ContentProvider::for_value(&item.encode().to_value()))
        .build();
    source.connect_drag_begin(|source, _| {
        if let Some(row) = source.widget() {
            source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
        }
    });
    row.add_controller(source);
}

/// One drop target on the whole list, so the empty space below the last row
/// also accepts drops (moving an entry to the end).
pub(in crate::app) fn install_drop_target(state: &App) {
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    target.set_preload(true);
    let weak = Rc::downgrade(state);
    target.connect_motion(move |target, _, y| {
        let item = target
            .value()
            .and_then(|v| v.get::<String>().ok())
            .and_then(|s| Item::decode(&s));
        if let (Some(state), Some(item)) = (weak.upgrade(), item) {
            show_drop_hint(&state, drop_anchor(&state, item, y));
        }
        gdk::DragAction::MOVE
    });
    let weak = Rc::downgrade(state);
    target.connect_leave(move |_| {
        if let Some(state) = weak.upgrade() {
            clear_drop_hint(&state);
        }
    });
    let weak = Rc::downgrade(state);
    target.connect_drop(move |_, value, _, y| {
        let item = value.get::<String>().ok().and_then(|s| Item::decode(&s));
        let (Some(state), Some(item)) = (weak.upgrade(), item) else {
            return false;
        };
        let anchor = drop_anchor(&state, item, y);
        clear_drop_hint(&state);
        // The rows are rebuilt by the move; don't do that while GTK is
        // still delivering the drop to them.
        glib::idle_add_local_once(move || move_item(&state, item, anchor));
        true
    });
    state.list.add_controller(target);
}

/// Upper/lower half of a row means before/after it. Over a group header a
/// workspace goes into the group, except near its top edge.
pub(in crate::app) fn drop_anchor(state: &App, dragged: Item, y: f64) -> Anchor {
    let Some(row) = state.list.row_at_y(y as i32) else {
        return Anchor::End;
    };
    let (Some(target), Some(bounds)) = (Item::of_row(&row), row.compute_bounds(&state.list)) else {
        return Anchor::End;
    };
    let rel = (y - bounds.y() as f64) / bounds.height().max(1.0) as f64;
    let before_or_after = |item| {
        if rel < 0.5 {
            Anchor::Before(item)
        } else {
            Anchor::After(item)
        }
    };
    match (dragged, target) {
        (Item::Workspace(_), Item::Group(group)) if rel >= 0.3 => Anchor::Into(group),
        (Item::Workspace(_), Item::Group(_)) => Anchor::Before(target),
        // Groups don't nest: over a grouped workspace, use its group.
        (Item::Group(_), Item::Workspace(ws)) => match group_of(&state.tree.borrow(), ws) {
            Some(group) => before_or_after(Item::Group(group)),
            None => before_or_after(target),
        },
        _ => before_or_after(target),
    }
}

pub(in crate::app) fn move_item(state: &App, item: Item, anchor: Anchor) {
    move_node(&mut state.tree.borrow_mut(), item, anchor);
    rebuild_sidebar(state);
    save_all(&state.shared);
}

pub(in crate::app) fn move_node(tree: &mut Vec<Node>, item: Item, anchor: Anchor) {
    let pointless = match anchor {
        Anchor::Before(target) | Anchor::After(target) => target == item,
        Anchor::Into(group) => item == Item::Group(group),
        Anchor::End => false,
    };
    if pointless {
        return;
    }
    {
        let node = match item {
            Item::Workspace(id) => {
                detach(tree, id);
                Node::Workspace(id)
            }
            Item::Group(_) => match locate(tree, item) {
                Some(Loc::Top(i)) => tree.remove(i),
                _ => return,
            },
        };
        let is_group = matches!(node, Node::Group { .. });
        let end = tree.len();
        let dest = match anchor {
            Anchor::End => Loc::Top(end),
            Anchor::Into(group) => match locate(tree, Item::Group(group)) {
                Some(Loc::Top(i)) if !is_group => match &tree[i] {
                    Node::Group { workspaces, .. } => Loc::InGroup(i, workspaces.len()),
                    Node::Workspace(_) => Loc::Top(end),
                },
                _ => Loc::Top(end),
            },
            Anchor::Before(target) | Anchor::After(target) => {
                let after = matches!(anchor, Anchor::After(_)) as usize;
                match locate(tree, target) {
                    Some(Loc::Top(i)) => Loc::Top(i + after),
                    Some(Loc::InGroup(i, _)) if is_group => Loc::Top(i + after),
                    Some(Loc::InGroup(i, j)) => Loc::InGroup(i, j + after),
                    None => Loc::Top(end),
                }
            }
        };
        match (dest, node) {
            (Loc::InGroup(i, j), Node::Workspace(id)) => {
                if let Node::Group { workspaces, .. } = &mut tree[i] {
                    workspaces.insert(j.min(workspaces.len()), id);
                }
            }
            (Loc::Top(i) | Loc::InGroup(i, _), node) => {
                let i = i.min(tree.len());
                tree.insert(i, node);
            }
        }
    }
}

pub(in crate::app) fn show_drop_hint(state: &App, anchor: Anchor) {
    clear_drop_hint(state);
    let hint = match anchor {
        Anchor::Before(item) => find_row(state, &item.row_name()).map(|r| (r, "drop-before")),
        Anchor::After(item) => last_row_of(state, item).map(|r| (r, "drop-after")),
        Anchor::Into(group) => {
            find_row(state, &Item::Group(group).row_name()).map(|r| (r, "drop-into"))
        }
        Anchor::End => (0..)
            .map_while(|i| state.list.row_at_index(i))
            .last()
            .map(|r| (r, "drop-after")),
    };
    if let Some((row, class)) = hint {
        row.add_css_class(class);
        state.drop_hint.replace(Some((row, class)));
    }
}

pub(in crate::app) fn clear_drop_hint(state: &App) {
    if let Some((row, class)) = state.drop_hint.take() {
        row.remove_css_class(class);
    }
}

/// The last visible row belonging to an entry (a group's last workspace).
pub(in crate::app) fn last_row_of(state: &App, item: Item) -> Option<gtk::ListBoxRow> {
    let last_child = match item {
        Item::Group(group) => state.tree.borrow().iter().find_map(|node| match node {
            Node::Group {
                id,
                collapsed: false,
                workspaces,
                ..
            } if *id == group => workspaces.last().copied(),
            _ => None,
        }),
        Item::Workspace(_) => None,
    };
    last_child
        .and_then(|ws| find_row(state, &ws.to_string()))
        .or_else(|| find_row(state, &item.row_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Workspaces 1..=3 at top level, then group 10 holding 4 and 5.
    fn tree() -> Vec<Node> {
        vec![
            Node::Workspace(id(1)),
            Node::Workspace(id(2)),
            Node::Workspace(id(3)),
            Node::Group {
                id: id(10),
                name: "g".into(),
                collapsed: false,
                workspaces: vec![id(4), id(5)],
            },
        ]
    }

    /// Compact form: `1 2 [10: 4 5]`.
    fn show(tree: &[Node]) -> String {
        let n = |u: &Uuid| u.as_u128().to_string();
        tree.iter()
            .map(|node| match node {
                Node::Workspace(w) => n(w),
                Node::Group { id, workspaces, .. } => {
                    let inner: Vec<String> = workspaces.iter().map(n).collect();
                    format!("[{}: {}]", n(id), inner.join(" "))
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn moved(item: Item, anchor: Anchor) -> String {
        let mut t = tree();
        move_node(&mut t, item, anchor);
        show(&t)
    }

    #[test]
    fn reorders_top_level() {
        let ws = |n| Item::Workspace(id(n));
        assert_eq!(moved(ws(1), Anchor::After(ws(3))), "2 3 1 [10: 4 5]");
        assert_eq!(moved(ws(3), Anchor::Before(ws(1))), "3 1 2 [10: 4 5]");
        assert_eq!(moved(ws(1), Anchor::End), "2 3 [10: 4 5] 1");
        assert_eq!(moved(ws(2), Anchor::Before(ws(2))), "1 2 3 [10: 4 5]");
    }

    #[test]
    fn moves_into_and_out_of_groups() {
        let ws = |n| Item::Workspace(id(n));
        assert_eq!(moved(ws(1), Anchor::Into(id(10))), "2 3 [10: 4 5 1]");
        assert_eq!(moved(ws(2), Anchor::Before(ws(5))), "1 3 [10: 4 2 5]");
        assert_eq!(moved(ws(4), Anchor::After(ws(1))), "1 4 2 3 [10: 5]");
        assert_eq!(moved(ws(5), Anchor::Before(ws(4))), "1 2 3 [10: 5 4]");
        assert_eq!(moved(ws(4), Anchor::End), "1 2 3 [10: 5] 4");
    }

    #[test]
    fn moves_groups() {
        let group = Item::Group(id(10));
        assert_eq!(
            moved(group, Anchor::Before(Item::Workspace(id(1)))),
            "[10: 4 5] 1 2 3"
        );
        assert_eq!(
            moved(group, Anchor::After(Item::Workspace(id(2)))),
            "1 2 [10: 4 5] 3"
        );
        assert_eq!(moved(group, Anchor::Before(group)), "1 2 3 [10: 4 5]");
    }
}
