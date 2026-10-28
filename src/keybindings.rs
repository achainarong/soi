//! Keyboard shortcuts: the actions that can be bound, their default keys,
//! and the user's changes in `~/.config/soi/keybindings.conf`, as lines
//! like `new-tab = <Shift><Control>t` (GTK accelerator syntax). Several
//! keys for one action are separated by `|`. An empty value turns a
//! shortcut off; actions not listed use their default.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::theme::key_values;

pub struct Binding {
    pub id: &'static str,
    pub label: &'static str,
    pub section: &'static str,
    linux: &'static str,
    mac: &'static str,
}

impl Binding {
    /// The default accelerator ("" = off): with Cmd on a Mac, which
    /// leaves Ctrl to the terminal.
    pub fn default_accel(&self) -> &'static str {
        if crate::platform::current().cmd_shortcuts() {
            self.mac
        } else {
            self.linux
        }
    }
}

const fn bind(
    section: &'static str,
    id: &'static str,
    label: &'static str,
    linux: &'static str,
    mac: &'static str,
) -> Binding {
    Binding {
        id,
        label,
        section,
        linux,
        mac,
    }
}

/// Every bindable action, in the order the settings page shows them, with
/// its default keys elsewhere and on a Mac.
pub const BINDINGS: &[Binding] = &[
    bind("Tabs", "tab-1", "Go to main tab 1", "<Alt>1", "<Meta>1"),
    bind("Tabs", "tab-2", "Go to main tab 2", "<Alt>2", "<Meta>2"),
    bind("Tabs", "tab-3", "Go to main tab 3", "<Alt>3", "<Meta>3"),
    bind("Tabs", "tab-4", "Go to main tab 4", "<Alt>4", "<Meta>4"),
    bind("Tabs", "tab-5", "Go to main tab 5", "<Alt>5", "<Meta>5"),
    bind("Tabs", "tab-6", "Go to main tab 6", "<Alt>6", "<Meta>6"),
    bind("Tabs", "tab-7", "Go to main tab 7", "<Alt>7", "<Meta>7"),
    bind("Tabs", "tab-8", "Go to main tab 8", "<Alt>8", "<Meta>8"),
    bind("Tabs", "tab-9", "Go to main tab 9", "<Alt>9", "<Meta>9"),
    bind(
        "Panes",
        "next-pane",
        "Next split pane",
        "<Shift><Control>braceright",
        "<Alt><Meta>bracketright",
    ),
    bind(
        "Panes",
        "previous-pane",
        "Previous split pane",
        "<Shift><Control>braceleft",
        "<Alt><Meta>bracketleft",
    ),
    bind(
        "Panes",
        "close-pane",
        "Close split pane",
        "<Control><Alt>w",
        "<Shift><Meta>w",
    ),
    bind(
        "Tabs",
        "new-tab",
        "New main tab",
        "<Shift><Control>t",
        "<Meta>t",
    ),
    bind(
        "Tabs",
        "close-tab",
        "Close main tab and all its panes",
        "<Shift><Control>w",
        "<Meta>w",
    ),
    bind(
        "Tabs",
        "reopen-tab",
        "Reopen the tab or workspace closed last",
        "<Shift><Control>z",
        "<Shift><Meta>t",
    ),
    bind(
        "Tabs",
        "next-tab",
        "Next main tab",
        "<Control>Tab",
        "<Shift><Meta>braceright",
    ),
    bind(
        "Tabs",
        "previous-tab",
        "Previous main tab",
        "<Shift><Control>Tab",
        "<Shift><Meta>braceleft",
    ),
    bind(
        "Panes",
        "split-right",
        "Split right",
        "<Shift><Control>e",
        "<Meta>d",
    ),
    bind(
        "Panes",
        "split-down",
        "Split down",
        "<Shift><Control>o",
        "<Shift><Meta>d",
    ),
    bind(
        "Panes",
        "focus-left",
        "Focus pane on the left",
        "<Shift><Control>Left",
        "<Alt><Meta>Left",
    ),
    bind(
        "Panes",
        "focus-right",
        "Focus pane on the right",
        "<Shift><Control>Right",
        "<Alt><Meta>Right",
    ),
    bind(
        "Panes",
        "focus-up",
        "Focus pane above",
        "<Shift><Control>Up",
        "<Alt><Meta>Up",
    ),
    bind(
        "Panes",
        "focus-down",
        "Focus pane below",
        "<Shift><Control>Down",
        "<Alt><Meta>Down",
    ),
    bind(
        "Panes",
        "zoom-pane",
        "Zoom pane (fill the workspace) or back",
        "<Shift><Control>Return",
        "<Shift><Meta>Return",
    ),
    bind(
        "Panes",
        "broadcast-input",
        "Type into all panes of the current main tab at once, or not",
        "<Shift><Control>b",
        "<Shift><Meta>b",
    ),
    bind(
        "Panes",
        "grow-left",
        "Move the divider left",
        "<Shift><Control><Alt>Left",
        "<Shift><Alt><Meta>Left",
    ),
    bind(
        "Panes",
        "grow-right",
        "Move the divider right",
        "<Shift><Control><Alt>Right",
        "<Shift><Alt><Meta>Right",
    ),
    bind(
        "Panes",
        "grow-up",
        "Move the divider up",
        "<Shift><Control><Alt>Up",
        "<Shift><Alt><Meta>Up",
    ),
    bind(
        "Panes",
        "grow-down",
        "Move the divider down",
        "<Shift><Control><Alt>Down",
        "<Shift><Alt><Meta>Down",
    ),
    bind(
        "Workspaces",
        "new-workspace",
        "New workspace",
        "<Shift><Control>a",
        "<Shift><Meta>a",
    ),
    bind(
        "Workspaces",
        "rename-workspace",
        "Rename workspace",
        "F2",
        "F2",
    ),
    bind(
        "Workspaces",
        "quick-switch",
        "Go to a workspace or agent by name",
        "<Shift><Control>p",
        "<Meta>p",
    ),
    bind(
        "Workspaces",
        "next-workspace",
        "Next workspace in this group",
        "<Control>Page_Down",
        "<Control><Meta>Down",
    ),
    bind(
        "Workspaces",
        "previous-workspace",
        "Previous workspace in this group",
        "<Control>Page_Up",
        "<Control><Meta>Up",
    ),
    bind(
        "Workspaces",
        "next-group",
        "Next group",
        "<Shift><Control>Page_Down",
        "<Shift><Control><Meta>Down",
    ),
    bind(
        "Workspaces",
        "previous-group",
        "Previous group",
        "<Shift><Control>Page_Up",
        "<Shift><Control><Meta>Up",
    ),
    bind(
        "Workspaces",
        "workspace-1",
        "Go to workspace 1 of this group",
        "<Control><Alt>1",
        "<Alt><Meta>1",
    ),
    bind(
        "Workspaces",
        "workspace-2",
        "Go to workspace 2 of this group",
        "<Control><Alt>2",
        "<Alt><Meta>2",
    ),
    bind(
        "Workspaces",
        "workspace-3",
        "Go to workspace 3 of this group",
        "<Control><Alt>3",
        "<Alt><Meta>3",
    ),
    bind(
        "Workspaces",
        "workspace-4",
        "Go to workspace 4 of this group",
        "<Control><Alt>4",
        "<Alt><Meta>4",
    ),
    bind(
        "Workspaces",
        "workspace-5",
        "Go to workspace 5 of this group",
        "<Control><Alt>5",
        "<Alt><Meta>5",
    ),
    bind(
        "Workspaces",
        "workspace-6",
        "Go to workspace 6 of this group",
        "<Control><Alt>6",
        "<Alt><Meta>6",
    ),
    bind(
        "Workspaces",
        "workspace-7",
        "Go to workspace 7 of this group",
        "<Control><Alt>7",
        "<Alt><Meta>7",
    ),
    bind(
        "Workspaces",
        "workspace-8",
        "Go to workspace 8 of this group",
        "<Control><Alt>8",
        "<Alt><Meta>8",
    ),
    bind(
        "Workspaces",
        "workspace-9",
        "Go to workspace 9 of this group",
        "<Control><Alt>9",
        "<Alt><Meta>9",
    ),
    bind(
        "Windows",
        "new-window",
        "New window",
        "<Shift><Control>n",
        "<Meta>n",
    ),
    bind("Windows", "next-window", "Next window", "", ""),
    bind("Windows", "previous-window", "Previous window", "", ""),
    bind(
        "Windows",
        "maximize-window",
        "Fill the screen (not fullscreen) or back",
        "<Alt>F10",
        "<Meta>Return|<Control><Alt>Escape",
    ),
    bind(
        "Windows",
        "snap-left",
        "Move the window to the left half of the screen (macOS)",
        "",
        "<Control><Alt>Left",
    ),
    bind(
        "Windows",
        "snap-right",
        "Move the window to the right half of the screen (macOS)",
        "",
        "<Control><Alt>Right",
    ),
    bind(
        "Windows",
        "snap-top",
        "Move the window to the top half of the screen (macOS)",
        "",
        "<Control><Alt>Up",
    ),
    bind(
        "Windows",
        "snap-bottom",
        "Move the window to the bottom half of the screen (macOS)",
        "",
        "<Control><Alt>Down",
    ),
    bind("Terminal", "copy", "Copy", "<Shift><Control>c", "<Meta>c"),
    bind("Terminal", "cut", "Cut", "<Shift><Control>x", "<Meta>x"),
    bind("Terminal", "paste", "Paste", "<Shift><Control>v", "<Meta>v"),
    bind(
        "Terminal",
        "copy-last-output",
        "Copy the last command's output",
        "<Shift><Control>y",
        "<Shift><Meta>y",
    ),
    bind(
        "Terminal",
        "search",
        "Search",
        "<Shift><Control>f",
        "<Meta>f",
    ),
    bind(
        "Terminal",
        "browse",
        "Browse mode (vim keys)",
        "<Shift><Control>space",
        "<Shift><Meta>space",
    ),
    bind(
        "Terminal",
        "zoom-in",
        "Bigger text",
        "<Control>plus",
        "<Meta>plus",
    ),
    bind(
        "Terminal",
        "zoom-out",
        "Smaller text",
        "<Control>minus",
        "<Meta>minus",
    ),
    bind(
        "Terminal",
        "zoom-reset",
        "Normal text size",
        "<Control>0",
        "<Meta>0",
    ),
    bind(
        "Agents",
        "toggle-agents",
        "Show or hide the agents panel",
        "<Shift><Control>g",
        "<Shift><Meta>g",
    ),
    bind(
        "Agents",
        "next-agent",
        "Go to the next agent that needs you",
        "<Shift><Control>j",
        "<Shift><Meta>j",
    ),
    bind(
        "Tasks",
        "task-details",
        "Open the current task's details",
        "<Shift><Control>i",
        "<Shift><Meta>i",
    ),
    bind(
        "Tasks",
        "toggle-plan",
        "Show or hide the plan beside the terminal",
        "<Shift><Control>m",
        "<Shift><Meta>m",
    ),
    bind(
        "App",
        "settings",
        "Settings",
        "<Control>comma",
        "<Meta>comma",
    ),
    bind("App", "quit", "Quit", "<Shift><Control>q", "<Meta>q"),
];

pub fn binding(id: &str) -> Option<&'static Binding> {
    BINDINGS.iter().find(|b| b.id == id)
}

/// The keys of a shortcut, which may list several separated by `|`.
pub fn accels(accel: &str) -> impl Iterator<Item = &str> {
    accel.split('|').map(str::trim).filter(|a| !a.is_empty())
}

fn path() -> Option<PathBuf> {
    Some(crate::dirs::config_home().join("soi/keybindings.conf"))
}

/// The user's shortcuts: defaults plus their changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keybindings {
    /// Action id → accelerator ("" = off), only where it differs from the
    /// default.
    changes: BTreeMap<String, String>,
}

impl Keybindings {
    pub fn load() -> Self {
        let text = path()
            .and_then(|path| fs::read_to_string(path).ok())
            .unwrap_or_default();
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let mut keybindings = Self::default();
        for (id, accel) in key_values(text) {
            if binding(&id).is_some() {
                keybindings.set(&id, &accel);
            }
        }
        keybindings
    }

    pub fn save(&self) -> io::Result<()> {
        let path =
            path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
        crate::dirs::write_atomic(&path, self.render().as_bytes(), 0o666)
    }

    fn render(&self) -> String {
        let mut text =
            String::from("# soi shortcuts that differ from the defaults; edit them in Settings.\n");
        for (id, accel) in &self.changes {
            text.push_str(&format!("{id} = {accel}\n"));
        }
        text
    }

    /// The accelerator for an action ("" when turned off).
    pub fn get(&self, id: &str) -> &str {
        match self.changes.get(id) {
            Some(accel) => accel,
            None => binding(id).map_or("", Binding::default_accel),
        }
    }

    pub fn set(&mut self, id: &str, accel: &str) {
        let accel = accel.trim();
        if binding(id).is_some_and(|b| b.default_accel() == accel) {
            self.changes.remove(id);
        } else {
            self.changes.insert(id.to_string(), accel.to_string());
        }
    }

    pub fn is_changed(&self, id: &str) -> bool {
        self.changes.contains_key(id)
    }

    pub fn reset_all(&mut self) {
        self.changes.clear();
    }

    /// Another action already using `accel`, as judged by `same`.
    pub fn conflict(
        &self,
        id: &str,
        accel: &str,
        same: impl Fn(&str, &str) -> bool,
    ) -> Option<&'static Binding> {
        BINDINGS
            .iter()
            .filter(|b| b.id != id)
            .find(|b| !self.get(b.id).is_empty() && same(self.get(b.id), accel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_changes() {
        let mut keys = Keybindings::default();
        let default = |id| binding(id).unwrap().default_accel();
        assert_eq!(keys.get("new-tab"), default("new-tab"));
        keys.set("new-tab", "<Alt>t");
        keys.set("close-tab", "");
        keys.set("split-right", default("split-right")); // no change
        let text = keys.render();
        assert!(text.contains("new-tab = <Alt>t") && text.contains("close-tab = \n"));
        assert!(!text.contains("split-right"));

        let loaded = Keybindings::parse(&format!("{text}\nunknown-action = <Alt>x\n"));
        assert_eq!(loaded, keys);
        assert_eq!(loaded.get("close-tab"), "", "turned off");
        assert!(loaded.is_changed("new-tab") && !loaded.is_changed("copy"));
    }

    #[test]
    fn finds_conflicts() {
        let keys = Keybindings::default();
        let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        let close = binding("close-tab").unwrap().default_accel();
        let other = keys
            .conflict("new-tab", &close.to_uppercase(), same)
            .unwrap();
        assert_eq!(other.id, "close-tab");
        assert!(keys.conflict("close-tab", close, same).is_none());
    }

    #[test]
    fn defaults_differ_on_a_mac() {
        for defaults in [
            BINDINGS.iter().map(|b| b.linux).collect::<Vec<_>>(),
            BINDINGS.iter().map(|b| b.mac).collect(),
        ] {
            let mut used: Vec<_> = defaults.into_iter().flat_map(accels).collect();
            let count = used.len();
            used.sort();
            used.dedup();
            assert_eq!(used.len(), count, "two actions share a default");
        }
        // Ctrl is the terminal's on a Mac, unless Cmd is held too. Except
        // Ctrl+Option to move the window, as with Rectangle.
        let window_keys = |id: &str| id.starts_with("snap-") || id == "maximize-window";
        assert!(BINDINGS.iter().all(|b| {
            accels(b.mac).all(|a| !a.contains("<Control>") || a.contains("<Meta>"))
                || window_keys(b.id)
        }));
    }

    #[test]
    fn splits_several_keys() {
        assert_eq!(
            accels(" <Meta>Return | <Control><Alt>Escape ").collect::<Vec<_>>(),
            ["<Meta>Return", "<Control><Alt>Escape"]
        );
        assert_eq!(accels("").count(), 0);
        assert_eq!(accels("<Meta>q").collect::<Vec<_>>(), ["<Meta>q"]);
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = BINDINGS.iter().map(|b| b.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), BINDINGS.len());
    }
}
