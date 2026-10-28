//! A log file for problems: `~/.local/state/soi/soi.log`. Crashes are
//! written there (message, location and backtrace), so they can be looked
//! at even when soi was started from the app menu and stderr went nowhere
//! visible.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Keeps the log from growing without bound: it starts over beyond this.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn path() -> Option<PathBuf> {
    Some(crate::dirs::state_home().join("soi/soi.log"))
}

/// Appends a line to the log file and prints it to stderr.
pub fn write(message: &str) {
    eprintln!("soi: {message}");
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = fs::remove_file(&path);
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "[{seconds}] {message}");
    }
}

/// Records crashes in the log file, in addition to the usual output.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        write(&format!(
            "crashed (version {}): {info}\n{backtrace}",
            env!("CARGO_PKG_VERSION")
        ));
        default(info);
    }));
}
