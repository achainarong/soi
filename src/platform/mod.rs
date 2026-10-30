//! What differs between the systems soi runs on, behind one trait. The rest
//! of soi asks `platform::current()` and stays free of `/proc` paths and
//! `cfg(target_os)`.
//!
//! - `linux`: Linux
//! - `macos`: macOS

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("soi runs on Linux and macOS");

pub trait Platform: Sync {
    /// The program in the foreground of a shell's terminal, or `None` when
    /// the shell itself is in the foreground (waiting at its prompt).
    fn foreground_program(&self, shell: u32) -> Option<String>;

    /// Every program in the foreground of a shell's terminal: the one
    /// leading it first, then the ones it started, such as the agent a
    /// wrapper runs. Empty when the shell is in the foreground.
    fn foreground_programs(&self, shell: u32) -> Vec<String>;

    /// A process's current directory.
    fn process_cwd(&self, pid: u32) -> Option<PathBuf>;

    /// A directory only this user can read, for the socket agents report
    /// to (see `crate::agents`).
    fn runtime_dir(&self) -> Option<PathBuf>;

    /// The font family used when neither the user nor their terminal's
    /// settings name one.
    fn monospace_family(&self) -> &'static str;

    /// Pseudo file systems holding no images, only kernel or device state:
    /// program output may not have soi read files there.
    fn kernel_filesystems(&self) -> &'static [&'static str];

    /// Where to look for shared data such as Ghostty themes and Konsole
    /// profiles: the user's own directory first, then the system's.
    fn data_dirs(&self) -> Vec<PathBuf>;

    /// Ghostty's config directories, read in this order (later ones win).
    fn ghostty_config_dirs(&self) -> Vec<PathBuf> {
        vec![crate::dirs::config_home().join("ghostty")]
    }

    /// Whether soi's default shortcuts use Cmd (GTK's `<Meta>`), as Mac
    /// apps do, instead of Ctrl.
    fn cmd_shortcuts(&self) -> bool {
        false
    }

    /// The host's deliberate gesture for opening terminal links.
    fn link_gesture(&self) -> LinkGesture {
        if self.cmd_shortcuts() {
            LinkGesture::Command
        } else {
            LinkGesture::Control
        }
    }

    /// Whether $SHELL starts as a login shell, as terminals do on a Mac:
    /// apps started from the Dock or Finder get only the system's PATH,
    /// and `~/.zprofile` (Homebrew, …) adds the rest.
    fn login_shell(&self) -> bool {
        false
    }

    /// Settings for GTK's renderer, as environment variables, that work
    /// around this system's drawing bugs.
    fn gtk_env(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Moves a window onto part of its screen. Returns false where soi
    /// can't, as under Wayland and X11 with GTK 4, whose window managers
    /// snap windows themselves (Super+arrows).
    fn snap_window(&self, _window: &gtk::Window, _snap: Snap) -> bool {
        false
    }

    /// Whether a window's GDK surface is just the window, so its size is
    /// the window's: not with client-side shadows around it, as on Linux.
    fn surface_is_window(&self) -> bool {
        false
    }

    /// This computer's name, to tell local `file://` links from others.
    fn hostname(&self) -> Option<String> {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
            return None;
        }
        let name = std::ffi::CStr::from_bytes_until_nul(&buf).ok()?;
        Some(name.to_string_lossy().trim().to_string())
    }
}

/// A part of the screen to put a window on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    Left,
    Right,
    Top,
    Bottom,
}

/// A terminal link's activation predicate and the guidance shown beside it.
/// This is action-specific: terminal keyboard input keeps its own modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkGesture {
    Command,
    Control,
}

impl LinkGesture {
    pub fn matches(self, button: u32, state: gtk::gdk::ModifierType) -> bool {
        use gtk::gdk::ModifierType as M;
        let keyboard = M::SHIFT_MASK
            | M::CONTROL_MASK
            | M::ALT_MASK
            | M::SUPER_MASK
            | M::HYPER_MASK
            | M::META_MASK;
        let required = match self {
            Self::Command => M::META_MASK,
            Self::Control => M::CONTROL_MASK,
        };
        button == 1 && state & keyboard == required
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Command => "Command+click to open",
            Self::Control => "Ctrl+click to open",
        }
    }
}

/// The platform soi was built for.
pub fn current() -> &'static dyn Platform {
    #[cfg(target_os = "linux")]
    {
        &linux::Linux
    }
    #[cfg(target_os = "macos")]
    {
        &macos::MacOs
    }
}

/// Variables set only for soi's own GTK, with the value programs soi starts
/// get instead (`None`: none).
static GTK_ONLY_ENV: OnceLock<Vec<(String, Option<OsString>)>> = OnceLock::new();

/// Sets the platform's `gtk_env` before GTK starts, leaving any the user
/// set alone. Call it while soi still has only its main thread.
///
/// The AppImage and the macOS bundle point GTK at the libraries inside
/// them before soi starts: their launcher lists those variables in
/// `SOI_BUNDLE_ENV`, with the values they had before in `SOI_OUTER_<name>`.
pub fn set_gtk_env() {
    GTK_ONLY_ENV.get_or_init(|| {
        let mut set = Vec::new();
        for &(key, value) in current().gtk_env() {
            if std::env::var_os(key).is_none() {
                // SAFETY: no other threads are running yet.
                unsafe { std::env::set_var(key, value) };
                set.push((key.to_string(), None));
            }
        }
        if let Ok(bundle) = std::env::var("SOI_BUNDLE_ENV") {
            for key in bundle.split_whitespace() {
                let outer = format!("SOI_OUTER_{key}");
                set.push((key.to_string(), std::env::var_os(&outer)));
                set.push((outer, None));
            }
            set.push(("SOI_BUNDLE_ENV".into(), None));
        }
        set
    });
}

/// Variables soi set only for its own GTK, with what programs it starts
/// get instead: the value from before, or none.
pub fn gtk_only_env() -> &'static [(String, Option<OsString>)] {
    GTK_ONLY_ENV.get().map_or(&[], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::gdk::ModifierType as M;

    #[test]
    fn link_gestures_require_exact_keyboard_modifiers_and_left_button() {
        let modifiers = [
            M::SHIFT_MASK,
            M::CONTROL_MASK,
            M::ALT_MASK,
            M::SUPER_MASK,
            M::HYPER_MASK,
            M::META_MASK,
        ];
        for (gesture, required) in [
            (LinkGesture::Command, M::META_MASK),
            (LinkGesture::Control, M::CONTROL_MASK),
        ] {
            for bits in 0..(1 << modifiers.len()) {
                let state = modifiers
                    .iter()
                    .enumerate()
                    .fold(M::empty(), |state, (i, m)| {
                        if bits & (1 << i) != 0 {
                            state | *m
                        } else {
                            state
                        }
                    });
                for button in 0..=5 {
                    assert_eq!(
                        gesture.matches(button, state),
                        button == 1 && state == required,
                        "{gesture:?}, button {button}, {state:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn link_gestures_ignore_lock_and_button_state_flags() {
        let incidental = [
            M::LOCK_MASK,
            M::BUTTON1_MASK,
            M::BUTTON2_MASK,
            M::BUTTON3_MASK,
            M::BUTTON4_MASK,
            M::BUTTON5_MASK,
        ];
        for (gesture, required) in [
            (LinkGesture::Command, M::META_MASK),
            (LinkGesture::Control, M::CONTROL_MASK),
        ] {
            for bits in 0..(1 << incidental.len()) {
                let state = incidental
                    .iter()
                    .enumerate()
                    .fold(required, |state, (i, m)| {
                        if bits & (1 << i) != 0 {
                            state | *m
                        } else {
                            state
                        }
                    });
                assert!(gesture.matches(1, state), "{gesture:?}, {state:?}");
                assert!(!gesture.matches(1, state | M::SHIFT_MASK));
            }
        }
    }

    #[test]
    fn link_hints_agree_with_policy() {
        assert_eq!(LinkGesture::Command.hint(), "Command+click to open");
        assert_eq!(LinkGesture::Control.hint(), "Ctrl+click to open");
        assert_eq!(
            current().link_gesture(),
            if current().cmd_shortcuts() {
                LinkGesture::Command
            } else {
                LinkGesture::Control
            }
        );
    }
}
