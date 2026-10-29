mod agents;
mod app;
mod collections;
mod dirs;
mod git;
mod keybindings;
mod log;
mod markdown;
mod mcp;
mod plan;
mod platform;
mod presets;
mod probe;
mod session;
mod settings;
mod shell_integration;
mod skills;
mod tasks;
mod term;
mod theme;

/// `SOI_DEV=1` (`make dev`): run as a separate app with its own session
/// file, next to an installed soi without touching its layout.
pub fn dev_mode() -> bool {
    std::env::var_os("SOI_DEV").is_some_and(|v| v != "0" && !v.is_empty())
}

/// Start the application or handle an agent command.
pub fn run() -> gtk::glib::ExitCode {
    // Commands for agents running inside soi; quick, without GTK.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("status") => std::process::exit(agents::status_command(&args[1..])),
        Some("claude-hook") => std::process::exit(agents::claude_hook_command()),
        Some("install-agents") => std::process::exit(agents::install_agents_command()),
        _ => {}
    }
    raise_open_file_limit();
    platform::set_gtk_env();
    adopt_old_files();
    log::install_panic_hook();
    app::run()
}

/// Run display-dependent checks on the process's main thread.
#[cfg(feature = "gui-tests")]
pub fn run_gui_tests() -> std::process::ExitCode {
    app::gui_tests::run()
}

/// Every terminal holds a few descriptors for its PTY, and macOS starts GUI
/// apps at 256 open files: a restored session with dozens of terminals would
/// fail to start the rest with "Too many open files". Raise the soft limit.
fn raise_open_file_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit is a valid rlimit for the call to fill in.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return;
    }
    // macOS refuses a soft limit above OPEN_MAX, even when the hard one is
    // higher. Shells inherit the limit too, and some programs slow down or
    // break with a huge one, so stop there on Linux as well.
    let wanted = limit.rlim_max.min(10240);
    if limit.rlim_cur < wanted {
        limit.rlim_cur = wanted;
        // SAFETY: limit is a valid rlimit; failure leaves the old limit.
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
    }
}

/// soi was called rim, and amux before that: takes over their settings and
/// layouts, unless soi already has its own.
fn adopt_old_files() {
    let state = dirs::state_home();
    for base in [dirs::config_home(), state.clone()] {
        let to = base.join("soi");
        for old in ["rim", "amux"] {
            let from = base.join(old);
            if from.is_dir() && !from.is_symlink() && !to.exists() {
                // Tasks remember their plans by full path, some of them in
                // the old state folder; a link keeps those paths working.
                if std::fs::rename(&from, &to).is_ok() && base == state {
                    let _ = std::os::unix::fs::symlink("soi", &from);
                }
            }
        }
    }
}
