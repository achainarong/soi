//! Groups of workspaces in the sidebar.

use super::*;

/// The sidebar's groups in order, each with its workspaces. The ungrouped
/// workspaces count as one group (`None`), where the first of them is.
pub(in crate::app) fn groups(state: &App) -> Vec<(Option<Uuid>, Vec<Uuid>)> {
    let mut groups: Vec<(Option<Uuid>, Vec<Uuid>)> = Vec::new();
    for node in state.tree.borrow().iter() {
        match node {
            Node::Workspace(id) => match groups.iter_mut().find(|(g, _)| g.is_none()) {
                Some((_, ungrouped)) => ungrouped.push(*id),
                None => groups.push((None, vec![*id])),
            },
            Node::Group { id, workspaces, .. } if !workspaces.is_empty() => {
                groups.push((Some(*id), workspaces.clone()));
            }
            Node::Group { .. } => {}
        }
    }
    groups
}

/// Workspace `n` (from 1) of the active workspace's group.
pub(in crate::app) fn select_in_group(state: &App, n: usize) {
    let active = state.active.get();
    let groups = groups(state);
    let workspaces = groups
        .iter()
        .find(|(_, workspaces)| active.is_some_and(|id| workspaces.contains(&id)))
        .or(groups.first())
        .map(|(_, workspaces)| workspaces);
    if let Some(&ws) = workspaces.and_then(|w| w.get(n.wrapping_sub(1))) {
        select_workspace(state, ws);
    }
}

/// Goes to the next (or previous) group, to the workspace last shown there.
pub(in crate::app) fn cycle_group(state: &App, delta: isize) {
    let groups = groups(state);
    if groups.is_empty() {
        return;
    }
    let active = state.active.get();
    let current = groups
        .iter()
        .position(|(_, workspaces)| active.is_some_and(|id| workspaces.contains(&id)))
        .unwrap_or(0);
    let (group, workspaces) =
        &groups[(current as isize + delta).rem_euclid(groups.len() as isize) as usize];
    let last = state.last_in_group.borrow().get(group).copied();
    let target = last
        .filter(|id| workspaces.contains(id))
        .unwrap_or(workspaces[0]);
    select_workspace(state, target);
}

pub(in crate::app) fn new_group(state: &App, workspaces: Vec<Uuid>, at: Option<usize>) {
    let id = Uuid::new_v4();
    {
        let mut tree = state.tree.borrow_mut();
        let at = at.unwrap_or(tree.len()).min(tree.len());
        tree.insert(
            at,
            Node::Group {
                id,
                name: "New group".into(),
                collapsed: false,
                workspaces,
            },
        );
    }
    rebuild_sidebar(state);
    rename_group(state, id);
}

pub(in crate::app) fn move_to_group(state: &App, ws: Uuid, group: Option<Uuid>) {
    {
        let mut tree = state.tree.borrow_mut();
        let Some(at) = detach(&mut tree, ws) else {
            return;
        };
        let target = tree.iter_mut().find_map(|node| match node {
            Node::Group {
                id,
                collapsed,
                workspaces,
                ..
            } if Some(*id) == group => Some((collapsed, workspaces)),
            _ => None,
        });
        match target {
            Some((collapsed, workspaces)) => {
                *collapsed = false;
                workspaces.push(ws);
            }
            None => {
                let at = at.min(tree.len());
                tree.insert(at, Node::Workspace(ws));
            }
        }
    }
    rebuild_sidebar(state);
}

pub(in crate::app) fn toggle_group(state: &App, id: Uuid) {
    for node in state.tree.borrow_mut().iter_mut() {
        if let Node::Group {
            id: gid, collapsed, ..
        } = node
            && *gid == id
        {
            *collapsed = !*collapsed;
        }
    }
    rebuild_sidebar(state);
}

/// Dissolves a group, keeping its workspaces in place.
pub(in crate::app) fn ungroup(state: &App, id: Uuid) {
    {
        let mut tree = state.tree.borrow_mut();
        let Some(i) = tree
            .iter()
            .position(|n| matches!(n, Node::Group { id: gid, .. } if *gid == id))
        else {
            return;
        };
        if let Node::Group { workspaces, .. } = tree.remove(i) {
            for (offset, ws) in workspaces.into_iter().enumerate() {
                tree.insert(i + offset, Node::Workspace(ws));
            }
        }
    }
    rebuild_sidebar(state);
}

/// Removes a group together with its workspaces (and their terminals).
pub(in crate::app) fn delete_group(state: &App, id: Uuid) {
    let members = state.tree.borrow().iter().find_map(|node| match node {
        Node::Group {
            id: gid,
            workspaces,
            ..
        } if *gid == id => Some(workspaces.clone()),
        _ => None,
    });
    for ws in members.unwrap_or_default() {
        close_workspace(state, ws);
    }
    state
        .tree
        .borrow_mut()
        .retain(|node| !matches!(node, Node::Group { id: gid, .. } if *gid == id));
    rebuild_sidebar(state);
}

pub(in crate::app) fn group_name(state: &App, id: Uuid) -> Option<String> {
    state.tree.borrow().iter().find_map(|node| match node {
        Node::Group { id: gid, name, .. } if *gid == id => Some(name.clone()),
        _ => None,
    })
}

pub(in crate::app) fn rename_group(state: &App, id: Uuid) {
    start_inline_rename(state, &format!("group:{id}"));
}
