//! User settings, organized in profiles.
//!
//! - `~/.config/soi/config` names the active profile (`profile = Work`).
//! - `~/.config/soi/profiles/<name>.conf` holds one profile's settings.
//!
//! Both are `key = value` files like Ghostty's. A missing key means "use the
//! default", which is usually detected from the system.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::theme::key_values;

const PROFILE: &str = "profile";
const THEME: &str = "theme";
const FONT: &str = "font";
const OPACITY: &str = "background-opacity";
const SHELL: &str = "shell";
const SCROLLBACK: &str = "scrollback-lines";
const START_DIR: &str = "start-directory";
const AGENTS: &str = "agents";
const NOTIFY: &str = "desktop-notifications";
const UI_SCALE: &str = "window-scale";
const CURSOR_BLINK: &str = "cursor-blink";
const SHELL_INTEGRATION: &str = "shell-integration";
const LONG_COMMAND: &str = "long-command-seconds";

pub const DEFAULT_PROFILE: &str = "Default";
pub const DEFAULT_SCROLLBACK: u32 = 10_000;
pub const DEFAULT_LONG_COMMAND: u32 = 10;
pub const MIN_UI_SCALE: f64 = 0.75;
pub const MAX_UI_SCALE: f64 = 1.5;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Settings {
    /// Color scheme id, see `theme::SchemeChoice`.
    pub theme: Option<String>,
    /// Pango font description, e.g. `JetBrainsMono Nerd Font Bold 12`.
    pub font: Option<String>,
    pub background_opacity: Option<f64>,
    /// Command for new terminals, split on whitespace.
    pub shell: Option<String>,
    pub scrollback: Option<u32>,
    /// Where new workspaces open; `~` is expanded.
    pub start_dir: Option<String>,
    /// Programs shown as agents, separated by commas or spaces.
    pub agents: Option<String>,
    /// Tell the desktop when an agent needs you while soi isn't in front.
    pub desktop_notifications: Option<bool>,
    /// Size of the whole window, text and terminals, e.g. 1.25 for 125%.
    pub ui_scale: Option<f64>,
    /// Blink the cursor, unless the program says otherwise.
    pub cursor_blink: Option<bool>,
    /// Start bash and zsh with soi's command marks (see `shell_integration`).
    pub shell_integration: Option<bool>,
    /// Commands that ran at least this many seconds say when they end
    /// while you look elsewhere (0: never).
    pub long_command: Option<u32>,
}

fn config_dir() -> Option<PathBuf> {
    Some(crate::dirs::config_home().join("soi"))
}

pub fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config"))
}

fn profiles_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("profiles"))
}

fn profile_path(name: &str) -> io::Result<PathBuf> {
    if !valid_profile_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid profile name {name:?}"),
        ));
    }
    profiles_dir()
        .map(|dir| dir.join(format!("{name}.conf")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))
}

pub fn valid_profile_name(name: &str) -> bool {
    !name.trim().is_empty() && name == name.trim() && !name.contains(['/', '\0']) && name != "."
}

fn write_file(path: &PathBuf, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text)
}

/// `existing` with the given keys set (or removed, for `None`); comments and
/// unknown keys are kept as they are.
fn update_lines(existing: &str, values: &[(&str, Option<String>)]) -> String {
    let ours = |line: &str| {
        line.split_once('=')
            .is_some_and(|(k, _)| values.iter().any(|(key, _)| *key == k.trim()))
    };
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| !ours(line))
        .map(String::from)
        .collect();
    lines.extend(
        values
            .iter()
            .filter_map(|(key, value)| Some(format!("{key} = {}", value.as_ref()?))),
    );
    lines.join("\n") + "\n"
}

// ---------------------------------------------------------------------------
// Profiles

/// Moves settings from older versions (kept directly in `config`) into the
/// Default profile, the first time profiles are used.
fn migrate() -> io::Result<()> {
    let (Some(config), Some(dir)) = (config_path(), profiles_dir()) else {
        return Ok(());
    };
    if dir.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&config).unwrap_or_default();
    Settings::parse(&text).save_profile(DEFAULT_PROFILE)?;
    let cleaned = update_lines(&text, &Settings::default().values());
    write_file(
        &config,
        &update_lines(&cleaned, &[(PROFILE, Some(DEFAULT_PROFILE.into()))]),
    )
}

/// All profile names, sorted; Default is always among them.
pub fn list_profiles() -> Vec<String> {
    if let Err(err) = migrate() {
        crate::log::write(&format!("failed to set up profiles: {err}"));
    }
    let mut names: Vec<String> = profiles_dir()
        .and_then(|dir| fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "conf" {
                return None;
            }
            Some(path.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    if !names.iter().any(|n| n == DEFAULT_PROFILE) {
        names.push(DEFAULT_PROFILE.into());
    }
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// The active profile, falling back to Default.
pub fn active_profile() -> String {
    let profiles = list_profiles();
    let text = config_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
    key_values(&text)
        .into_iter()
        .rev()
        .find(|(k, _)| k == PROFILE)
        .map(|(_, v)| v)
        .filter(|name| profiles.contains(name))
        .unwrap_or_else(|| DEFAULT_PROFILE.into())
}

pub fn set_active_profile(name: &str) -> io::Result<()> {
    let path = config_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    let text = fs::read_to_string(&path).unwrap_or_default();
    write_file(&path, &update_lines(&text, &[(PROFILE, Some(name.into()))]))
}

/// Fails for Default, which always stays.
fn not_default(name: &str) -> io::Result<()> {
    if name == DEFAULT_PROFILE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the Default profile can't be renamed or deleted",
        ));
    }
    Ok(())
}

pub fn rename_profile(old: &str, new: &str) -> io::Result<()> {
    not_default(old)?;
    let (from, to) = (profile_path(old)?, profile_path(new)?);
    if to.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("a profile named {new:?} already exists"),
        ));
    }
    let was_active = active_profile() == old;
    if from.exists() {
        fs::rename(from, to)?;
    } else {
        Settings::default().save_profile(new)?;
    }
    if was_active {
        set_active_profile(new)?;
    }
    Ok(())
}

pub fn delete_profile(name: &str) -> io::Result<()> {
    not_default(name)?;
    match fs::remove_file(profile_path(name)?) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// `base`, or `base 2`, `base 3`, … — whichever doesn't exist yet.
pub fn unique_profile_name(base: &str) -> String {
    let existing = list_profiles();
    (1..)
        .map(|n| {
            if n == 1 {
                base.to_string()
            } else {
                format!("{base} {n}")
            }
        })
        .find(|name| !existing.contains(name))
        .expect("unbounded")
}

/// The shell used when none is set: what the user's terminal (Konsole or
/// Ghostty) starts, else $SHELL (as a login shell on a Mac).
pub fn default_shell() -> String {
    crate::theme::terminal_command().unwrap_or_else(|| {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        if crate::platform::current().login_shell() {
            format!("{shell} -l")
        } else {
            shell
        }
    })
}

// ---------------------------------------------------------------------------
// Settings of one profile

impl Settings {
    pub fn load_profile(name: &str) -> Self {
        profile_path(name)
            .ok()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn save_profile(&self, name: &str) -> io::Result<()> {
        let path = profile_path(name)?;
        let existing = fs::read_to_string(&path).unwrap_or_default();
        write_file(&path, &update_lines(&existing, &self.values()))
    }

    fn parse(text: &str) -> Self {
        let pairs = key_values(text);
        let get = |key: &str| {
            pairs
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        Self {
            theme: get(THEME),
            font: get(FONT),
            background_opacity: get(OPACITY).and_then(|v| v.parse().ok()),
            shell: get(SHELL),
            scrollback: get(SCROLLBACK).and_then(|v| v.parse().ok()),
            start_dir: get(START_DIR),
            agents: get(AGENTS),
            desktop_notifications: get(NOTIFY).and_then(|v| v.parse().ok()),
            ui_scale: get(UI_SCALE).and_then(|v| v.parse().ok()),
            cursor_blink: get(CURSOR_BLINK).and_then(|v| v.parse().ok()),
            shell_integration: get(SHELL_INTEGRATION).and_then(|v| v.parse().ok()),
            long_command: get(LONG_COMMAND).and_then(|v| v.parse().ok()),
        }
    }

    fn values(&self) -> [(&'static str, Option<String>); 12] {
        [
            (THEME, self.theme.clone()),
            (FONT, self.font.clone()),
            (OPACITY, self.background_opacity.map(|o| format!("{o:.2}"))),
            (SHELL, self.shell.clone()),
            (SCROLLBACK, self.scrollback.map(|s| s.to_string())),
            (START_DIR, self.start_dir.clone()),
            (AGENTS, self.agents.clone()),
            (NOTIFY, self.desktop_notifications.map(|on| on.to_string())),
            (UI_SCALE, self.ui_scale.map(|s| format!("{s:.2}"))),
            (CURSOR_BLINK, self.cursor_blink.map(|on| on.to_string())),
            (
                SHELL_INTEGRATION,
                self.shell_integration.map(|on| on.to_string()),
            ),
            (LONG_COMMAND, self.long_command.map(|s| s.to_string())),
        ]
    }

    /// See `long_command`; `None` when that's off.
    pub fn long_command(&self) -> Option<std::time::Duration> {
        match self.long_command.unwrap_or(DEFAULT_LONG_COMMAND) {
            0 => None,
            seconds => Some(std::time::Duration::from_secs(seconds.into())),
        }
    }

    pub fn shell_integration(&self) -> bool {
        self.shell_integration.unwrap_or(true)
    }

    pub fn cursor_blink(&self) -> bool {
        self.cursor_blink.unwrap_or(false)
    }

    pub fn desktop_notifications(&self) -> bool {
        self.desktop_notifications.unwrap_or(true)
    }

    pub fn shell_command(&self) -> Vec<String> {
        let command = self.shell.clone().unwrap_or_else(default_shell);
        command.split_whitespace().map(String::from).collect()
    }

    /// The start directory if it exists, otherwise the home directory.
    pub fn start_dir(&self) -> PathBuf {
        let home = crate::session::home_dir();
        self.start_dir
            .as_deref()
            .map(|dir| match dir.strip_prefix('~') {
                Some(rest) => home.join(rest.trim_start_matches('/')),
                None => PathBuf::from(dir),
            })
            .filter(|dir| dir.is_dir())
            .unwrap_or(home)
    }

    pub fn ui_scale(&self) -> f64 {
        self.ui_scale
            .unwrap_or(1.0)
            .clamp(MIN_UI_SCALE, MAX_UI_SCALE)
    }

    pub fn scrollback_lines(&self) -> u32 {
        self.scrollback.unwrap_or(DEFAULT_SCROLLBACK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_keeps_other_lines() {
        let settings = Settings {
            theme: Some("builtin:Nord".into()),
            background_opacity: Some(0.8),
            shell: Some("fish -l".into()),
            ..Default::default()
        };
        let text = update_lines(
            "# my notes\nfont = Old 10\nunknown = 1\n",
            &settings.values(),
        );
        assert!(text.contains("# my notes") && text.contains("unknown = 1"));
        assert!(!text.contains("Old 10"), "unset keys are removed");
        assert_eq!(Settings::parse(&text), settings);
        assert_eq!(settings.shell_command(), ["fish", "-l"]);
    }

    /// The only test touching the file system; it points the config
    /// directory at a temporary one.
    #[test]
    fn manages_profile_files() {
        let dir = std::env::temp_dir().join(format!("soi-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("soi")).unwrap();
        // SAFETY: no other test reads the environment.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        fs::write(
            dir.join("soi/config"),
            "# mine\nbackground-opacity = 0.60\ntheme = builtin:Nord\n",
        )
        .unwrap();

        // Settings from before profiles existed become the Default profile.
        assert_eq!(list_profiles(), [DEFAULT_PROFILE]);
        assert_eq!(active_profile(), DEFAULT_PROFILE);
        let default = Settings::load_profile(DEFAULT_PROFILE);
        assert_eq!(default.background_opacity, Some(0.6));
        assert_eq!(default.theme.as_deref(), Some("builtin:Nord"));
        let config = fs::read_to_string(dir.join("soi/config")).unwrap();
        assert!(config.contains("# mine") && !config.contains("opacity"));

        let copy = unique_profile_name("Default copy");
        default.save_profile(&copy).unwrap();
        set_active_profile(&copy).unwrap();
        rename_profile(&copy, "Work").unwrap();
        assert_eq!(list_profiles(), [DEFAULT_PROFILE, "Work"]);
        assert_eq!(active_profile(), "Work", "renaming keeps it active");
        assert_eq!(Settings::load_profile("Work"), default);
        assert!(rename_profile("Work", DEFAULT_PROFILE).is_err());
        assert!(rename_profile(DEFAULT_PROFILE, "Home").is_err());
        assert!(delete_profile(DEFAULT_PROFILE).is_err());

        delete_profile("Work").unwrap();
        assert_eq!(active_profile(), DEFAULT_PROFILE, "falls back to Default");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_profile_names() {
        assert!(valid_profile_name("Work"));
        assert!(valid_profile_name("Late night"));
        for bad in ["", " ", " padded", "a/b", "."] {
            assert!(!valid_profile_name(bad), "{bad:?}");
        }
    }
}
