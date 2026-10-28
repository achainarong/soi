//! The persisted session: open and closed window contexts, each with its sidebar
//! (workspaces, optionally grouped), and each workspace's ordered main tabs,
//! each with an independent split layout of terminal panes.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub windows: Vec<WindowSession>,
    /// Before soi had several windows, the one window was stored here.
    #[serde(default, skip_serializing)]
    sidebar: Vec<Entry>,
    #[serde(default, skip_serializing)]
    active: Option<Uuid>,
    /// Tabs and workspaces closed lately, the last one last, to reopen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<Closed>,
    /// Workspaces put away when done with them, the last one last: gone
    /// from the sidebar, their folders (e.g. a task's worktree) kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived: Vec<Workspace>,
    /// Text size from zooming in or out, on top of the window scale;
    /// `None` is normal size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f64>,
}

/// Something closed that can be opened again, with its saved output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Closed {
    Tab { id: Uuid, cwd: PathBuf },
    MainTab(WorkspaceTab),
    Workspace(Workspace),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSession {
    /// Stable identity, independent of the window's name or open-list index.
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    /// Closed contexts remain saved and can be reopened from another window.
    #[serde(default)]
    pub closed: bool,
    /// The window's name, e.g. "Work".
    #[serde(default)]
    pub name: Option<String>,
    /// The agents panel on the right is shown.
    #[serde(default)]
    pub agents_panel: bool,
    /// Top-level sidebar entries, in display order.
    pub sidebar: Vec<Entry>,
    pub active: Option<Uuid>,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    /// The sidebar's width (0: the default).
    #[serde(default)]
    pub sidebar_width: i32,
    /// The agents panel's width, kept while it's hidden (0: its natural width).
    #[serde(default)]
    pub agents_width: i32,
    /// When it was last typed in (seconds since 1970; 0: unknown).
    #[serde(default)]
    pub last_used: u64,
}

impl Default for WindowSession {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            closed: false,
            name: None,
            agents_panel: false,
            sidebar: Vec::new(),
            active: None,
            width: 0,
            height: 0,
            sidebar_width: 0,
            agents_width: 0,
            last_used: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    Workspace(Workspace),
    Group(Group),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
    /// The collection of skills and commands its new tasks start with (see
    /// `crate::collections`), by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    /// Project directory; shown in the sidebar and used as fallback cwd.
    pub root_dir: PathBuf,
    pub tabs: Vec<WorkspaceTab>,
    pub active_tab: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Box<crate::tasks::TaskContext>>,
}

/// A main workspace tab and its independent split layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceTab {
    pub id: Uuid,
    pub name: String,
    pub layout: Layout,
    #[serde(default)]
    pub focused_pane: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layout {
    Pane {
        /// The shown tab's directory (all a pane had before it had tabs).
        cwd: PathBuf,
        /// Every tab's directory, when there are several.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tabs: Vec<PathBuf>,
        /// Index of the shown tab.
        #[serde(default, skip_serializing_if = "is_zero")]
        active: usize,
        /// Each tab's identity, to find its saved output (see `scrollback`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ids: Vec<Uuid>,
        /// Fills the workspace, the other panes hidden.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        zoomed: bool,
    },
    Split {
        orientation: Orientation,
        /// Position of the divider, 0.0..=1.0.
        ratio: f64,
        start: Box<Layout>,
        end: Box<Layout>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    /// Panes side by side.
    Horizontal,
    /// Panes stacked on top of each other.
    Vertical,
}

pub fn home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Layout {
    /// A pane with one tab.
    pub fn pane(cwd: PathBuf) -> Self {
        Layout::Pane {
            cwd,
            tabs: Vec::new(),
            active: 0,
            ids: Vec::new(),
            zoomed: false,
        }
    }

    fn tab_ids(&self, out: &mut HashSet<Uuid>) {
        match self {
            Layout::Pane { ids, .. } => out.extend(ids),
            Layout::Split { start, end, .. } => {
                start.tab_ids(out);
                end.tab_ids(out);
            }
        }
    }
}

impl Workspace {
    fn tab_ids(&self, out: &mut HashSet<Uuid>) {
        for tab in &self.tabs {
            tab.layout.tab_ids(out);
        }
    }

    pub fn new(root_dir: PathBuf) -> Self {
        let name = root_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root_dir.display().to_string());
        Self {
            id: Uuid::new_v4(),
            name,
            tabs: vec![WorkspaceTab {
                id: Uuid::new_v4(),
                name: "Terminal".into(),
                layout: Layout::pane(root_dir.clone()),
                focused_pane: 0,
            }],
            active_tab: 0,
            root_dir,
            task: None,
        }
    }
}

impl Session {
    pub fn new(windows: Vec<WindowSession>) -> Self {
        Self {
            windows,
            ..Default::default()
        }
    }

    /// The saved session, legacy ones converted.
    pub fn restored() -> Self {
        let session = Self::load();
        let closed = session.closed.clone();
        let archived = session.archived.clone();
        let zoom = session.zoom;
        Self {
            closed,
            archived,
            zoom,
            ..Self::new(session.into_windows())
        }
    }

    /// Every tab of every context, open, closed or archived.
    pub fn tab_ids(&self) -> HashSet<Uuid> {
        let mut ids = HashSet::new();
        for window in &self.windows {
            for entry in &window.sidebar {
                match entry {
                    Entry::Workspace(ws) => ws.tab_ids(&mut ids),
                    Entry::Group(group) => {
                        for ws in &group.workspaces {
                            ws.tab_ids(&mut ids);
                        }
                    }
                }
            }
        }
        for closed in &self.closed {
            match closed {
                Closed::Tab { id, .. } => {
                    ids.insert(*id);
                }
                Closed::MainTab(tab) => tab.layout.tab_ids(&mut ids),
                Closed::Workspace(ws) => ws.tab_ids(&mut ids),
            }
        }
        for ws in &self.archived {
            ws.tab_ids(&mut ids);
        }
        ids
    }

    /// Update one context in place without dropping closed windows or changing
    /// its position in the window list. Returns whether persistence is needed.
    pub fn update_window(&mut self, window: WindowSession) -> bool {
        if let Some(saved) = self.windows.iter_mut().find(|saved| saved.id == window.id) {
            if *saved == window {
                return false;
            }
            *saved = window;
        } else {
            self.windows.push(window);
        }
        true
    }

    /// Deletes closed contexts: the one with `id`, or all of them. Open
    /// windows stay. Returns whether anything was deleted.
    pub fn remove_closed(&mut self, id: Option<Uuid>) -> bool {
        let before = self.windows.len();
        self.windows
            .retain(|window| !window.closed || id.is_some_and(|id| id != window.id));
        self.windows.len() != before
    }

    /// The windows to open on start: those that were open, and the one used
    /// last even if it was closed just before quitting. That one comes last,
    /// so it ends up in front. With nothing open, a saved context comes back
    /// rather than none (see `last_closed`).
    pub fn windows_to_open(&self) -> Vec<WindowSession> {
        let last = self
            .windows
            .iter()
            .filter(|window| window.last_used > 0)
            .max_by_key(|window| window.last_used)
            .map(|window| window.id);
        let (mut open, used_last): (Vec<_>, Vec<_>) = self
            .windows
            .iter()
            .filter(|window| !window.closed || Some(window.id) == last)
            .cloned()
            .partition(|window| Some(window.id) != last);
        open.extend(used_last);
        if open.is_empty() {
            open.extend(self.last_closed().cloned());
        }
        open
    }

    /// The closed context used last; without times, the last one saved.
    pub fn last_closed(&self) -> Option<&WindowSession> {
        // `max_by_key` takes the last of equals.
        self.windows
            .iter()
            .filter(|window| window.closed)
            .max_by_key(|window| window.last_used)
    }

    /// The windows to restore, including a window saved by older versions.
    pub fn into_windows(mut self) -> Vec<WindowSession> {
        if self.windows.is_empty() && !self.sidebar.is_empty() {
            self.windows.push(WindowSession {
                sidebar: std::mem::take(&mut self.sidebar),
                active: self.active,
                ..Default::default()
            });
        }
        self.windows
    }
}

fn state_dir() -> Option<PathBuf> {
    Some(crate::dirs::state_home().join("soi"))
}

fn session_path() -> Option<PathBuf> {
    // A dev build (see `crate::dev_mode`) keeps its own layout.
    let name = if crate::dev_mode() {
        "session-dev.json"
    } else {
        "session.json"
    };
    Some(state_dir()?.join(name))
}

/// Terminal output saved across restarts: one file per tab, named by the
/// tab's id, holding `Term::export` text.
pub mod scrollback {
    use super::*;

    fn dir() -> Option<PathBuf> {
        let name = if crate::dev_mode() {
            "scrollback-dev"
        } else {
            "scrollback"
        };
        Some(state_dir()?.join(name))
    }

    fn path(id: Uuid) -> Option<PathBuf> {
        Some(dir()?.join(format!("{id}.txt")))
    }

    pub fn load(id: Uuid) -> Option<String> {
        fs::read_to_string(path(id)?).ok()
    }

    pub fn save(id: Uuid, text: &str) -> io::Result<()> {
        let path = path(id).ok_or_else(|| io::Error::other("no state directory"))?;
        if text.is_empty() {
            return match fs::remove_file(&path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }

    /// Deletes saved output of tabs that are gone.
    pub fn prune(keep: &HashSet<Uuid>) {
        let Some(entries) = dir().and_then(|dir| fs::read_dir(dir).ok()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let id = path
                .file_stem()
                .and_then(|stem| Uuid::parse_str(&stem.to_string_lossy()).ok());
            if id.is_none_or(|id| !keep.contains(&id)) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

impl Session {
    /// Loads the saved session, falling back to a default one.
    pub fn load() -> Self {
        let Some(path) = session_path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<Session>(&json) {
                Ok(session) => session,
                Err(err) => {
                    crate::log::write(&format!(
                        "ignoring corrupt session {}: {err}",
                        path.display()
                    ));
                    Self::default()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                crate::log::write(&format!("cannot read session {}: {err}", path.display()));
                Self::default()
            }
        }
    }

    /// Writes the session atomically (temp file + rename).
    pub fn save(&self) -> io::Result<()> {
        let path = session_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_context_round_trips_and_old_workspaces_still_load() {
        let mut workspace = Workspace::new("/project".into());
        let old = serde_json::to_string(&workspace).unwrap();
        assert!(
            serde_json::from_str::<Workspace>(&old)
                .unwrap()
                .task
                .is_none()
        );
        let mut task = crate::tasks::TaskContext::new("Fix login".into(), "/repository".into());
        task.status = crate::tasks::TaskStatus::Review;
        task.plan = Some("/project/plan.md".into());
        task.important_files.push("/project/src/auth.rs".into());
        // A task saved before ticket fields existed still loads, empty.
        let before = serde_json::to_value(&task).unwrap();
        for field in ["issue", "description", "acceptance", "checklist"] {
            assert!(before.get(field).is_none(), "{field} is omitted when empty");
        }
        let loaded: crate::tasks::TaskContext = serde_json::from_value(before).unwrap();
        assert_eq!(loaded, task);
        task.issue = Some("https://example.com/issues/1".into());
        task.description = "Users get logged out.".into();
        task.acceptance = "Sessions survive a restart.".into();
        task.checklist = crate::tasks::parse_checklist("- [x] Reproduce\n- [ ] Fix");
        workspace.task = Some(Box::new(task));
        let json = serde_json::to_string(&workspace).unwrap();
        assert_eq!(serde_json::from_str::<Workspace>(&json).unwrap(), workspace);
    }

    #[test]
    fn updates_keep_closed_contexts_and_round_trip_their_full_layouts() {
        let mut workspace = Workspace::new(PathBuf::from("/project"));
        workspace.tabs[0].layout = Layout::Split {
            orientation: Orientation::Horizontal,
            ratio: 0.37,
            start: Box::new(Layout::Pane {
                cwd: PathBuf::from("/project/tests"),
                tabs: vec![PathBuf::from("/project"), PathBuf::from("/project/tests")],
                active: 1,
                ids: vec![Uuid::new_v4(), Uuid::new_v4()],
                zoomed: true,
            }),
            end: Box::new(Layout::pane(PathBuf::from("/project/src"))),
        };
        let first = WindowSession {
            name: Some("First".into()),
            ..Default::default()
        };
        let mut second = WindowSession {
            name: Some("Second".into()),
            active: Some(workspace.id),
            width: 1200,
            height: 800,
            sidebar: vec![Entry::Group(Group {
                id: Uuid::new_v4(),
                name: "Project".into(),
                collapsed: true,
                collection: Some("C++".into()),
                workspaces: vec![workspace],
            })],
            ..Default::default()
        };
        let mut session = Session::new(vec![first.clone(), second.clone()]);
        second.closed = true;
        assert!(session.update_window(second.clone()));
        assert!(!session.update_window(first.clone()));
        assert_eq!(session.windows, [first, second.clone()]);
        let encoded = serde_json::to_string(&session).unwrap();
        let windows = serde_json::from_str::<Session>(&encoded)
            .unwrap()
            .into_windows();
        assert_eq!(windows[1], second);
        assert!(windows[1].closed);

        second.closed = false;
        assert!(session.update_window(second.clone()));
        assert_eq!(
            session.windows.len(),
            2,
            "reopening updates the same context"
        );
        assert_eq!(session.windows[1], second);
        assert!(
            !session.update_window(session.windows[1].clone()),
            "skip unchanged saves"
        );
    }

    #[test]
    fn opens_the_window_used_last_even_when_closed() {
        let window = |closed, last_used| WindowSession {
            closed,
            last_used,
            ..Default::default()
        };
        let (main, empty, old) = (window(true, 200), window(false, 100), window(true, 50));
        let session = Session::new(vec![main.clone(), empty.clone(), old]);
        assert_eq!(session.windows_to_open(), [empty.clone(), main.clone()]);
        let unknown = Session::new(vec![window(true, 0), empty.clone()]);
        assert_eq!(unknown.windows_to_open(), std::slice::from_ref(&empty));
        let open = Session::new(vec![window(false, 300), empty.clone()]);
        assert_eq!(open.windows_to_open(), [empty, open.windows[0].clone()]);
    }

    #[test]
    fn opens_a_saved_window_when_none_was_open() {
        let window = |last_used| WindowSession {
            closed: true,
            last_used,
            ..Default::default()
        };
        let (first, second) = (window(0), window(0));
        let unknown = Session::new(vec![first, second.clone()]);
        assert_eq!(unknown.windows_to_open(), [second]);
        let (used, old) = (window(100), window(0));
        let session = Session::new(vec![used.clone(), old]);
        assert_eq!(session.windows_to_open(), [used]);
        assert!(Session::default().windows_to_open().is_empty());
    }

    #[test]
    fn deletes_only_closed_contexts() {
        let open = WindowSession::default();
        let closed = |name: &str| WindowSession {
            name: Some(name.into()),
            closed: true,
            ..Default::default()
        };
        let (first, second) = (closed("First"), closed("Second"));
        let mut session = Session::new(vec![open.clone(), first.clone(), second.clone()]);
        assert!(!session.remove_closed(Some(open.id)), "open windows stay");
        assert!(session.remove_closed(Some(first.id)));
        assert_eq!(session.windows, [open.clone(), second]);
        session.windows.push(first);
        assert!(session.remove_closed(None));
        assert_eq!(session.windows, [open]);
        assert!(!session.remove_closed(None));
    }

    #[test]
    fn migrates_windows_without_ids_and_preserves_their_new_identities() {
        let old = r#"{"windows":[
            {"name":"First","sidebar":[],"active":null},
            {"name":"Second","sidebar":[],"active":null}
        ]}"#;
        let windows = serde_json::from_str::<Session>(old).unwrap().into_windows();
        assert!(!windows[0].id.is_nil());
        assert_ne!(windows[0].id, windows[1].id);
        assert!(windows.iter().all(|w| !w.closed));
        let session = Session::new(windows.clone());
        let encoded = serde_json::to_string(&session).unwrap();
        assert_eq!(
            serde_json::from_str::<Session>(&encoded)
                .unwrap()
                .into_windows(),
            windows
        );
    }

    #[test]
    fn rejects_sessions_from_before_workspace_tabs() {
        let old = r#"{"sidebar":[{"kind":"workspace","id":"22222222-2222-2222-2222-222222222222",
            "name":"soi","root_dir":"/tmp","layout":{"type":"pane","cwd":"/tmp"}}],
            "active":"22222222-2222-2222-2222-222222222222"}"#;
        assert!(
            serde_json::from_str::<Session>(old).is_err(),
            "workspace tabs start a new session format"
        );
    }
}
