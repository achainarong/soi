//! Where soi keeps its files: the XDG base directories, on macOS too (like
//! Ghostty, Kitty or Neovim there), so settings stay in `~/.config`.

use std::path::PathBuf;

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_STATE_HOME`, else `~/.local/state`.
pub fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| crate::session::home_dir().join(fallback))
}
