//! The PATH agent tabs run with, read from the user's login shell without
//! letting its startup scripts block an agent launch.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::home;

/// The PATH agent tabs run with: the configured shell's, then soi's own, then
/// ~/.local/bin. Zsh is interactive too, so .zshrc can add NVM and other tools.
/// Each shell is asked once; changing profiles must not reuse another shell's PATH.
pub fn agent_path(shell: &str) -> String {
    static PATHS: OnceLock<ShellPaths> = OnceLock::new();
    cached_path(PATHS.get_or_init(ShellPaths::default), shell, || {
        let login = login_path(shell).unwrap_or_default();
        let own = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = Vec::new();
        for dir in std::env::split_paths(&login)
            .chain(std::env::split_paths(&own))
            .chain([home().join(".local/bin")])
        {
            if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        std::env::join_paths(dirs)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

type ShellPaths = Mutex<HashMap<String, Arc<OnceLock<String>>>>;

fn cached_path(paths: &ShellPaths, shell: &str, load: impl FnOnce() -> String) -> String {
    #[expect(
        clippy::unwrap_used,
        reason = "nothing panics while this lock is held, so it is never poisoned"
    )]
    let path = paths
        .lock()
        .unwrap()
        .entry(shell.to_string())
        .or_default()
        .clone();
    // Do not hold the map lock while a shell runs its startup scripts.
    path.get_or_init(load).clone()
}

const SHELL_PATH_MARKER: &[u8] = b"\0SOI_AGENT_PATH\0";
const MAX_SHELL_OUTPUT: usize = 64 * 1024;

fn is_zsh(shell: &str) -> bool {
    Path::new(shell)
        .file_name()
        .is_some_and(|name| name == "zsh")
}

fn path_command(shell: &str) -> Command {
    let mut command = Command::new(shell);
    if is_zsh(shell) {
        // Let zsh locate .zshrc (including ZDOTDIR); do not source it ourselves.
        // NUL framing excludes startup greetings and logout messages from PATH.
        command.args([
            "-i",
            "-l",
            "-c",
            "builtin printf '\\0SOI_AGENT_PATH\\0%s\\0' \"$PATH\"",
        ]);
    } else {
        command.args(["-l", "-c", "printenv PATH"]);
    }
    command
}

/// Read the shell's PATH without letting startup scripts block agent launch.
fn login_path(shell: &str) -> Option<String> {
    probe_path(
        &mut path_command(shell),
        is_zsh(shell),
        Duration::from_secs(3),
    )
}

fn probe_path(command: &mut Command, framed: bool, timeout: Duration) -> Option<String> {
    let mut output = Vec::new();
    let mut found = None;
    let status = crate::probe::run(command, timeout, |chunk| {
        output.extend_from_slice(chunk);
        if output.len() > MAX_SHELL_OUTPUT {
            output.drain(..output.len() - MAX_SHELL_OUTPUT);
        }
        if framed && let Some(path) = framed_path(&output) {
            found = Some(path);
        }
        ControlFlow::Continue(())
    })?;
    if !status.success() {
        return None;
    }
    if framed {
        found
    } else {
        // Keep the existing login-shell behavior for bash and fish.
        std::str::from_utf8(&output)
            .ok()?
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(String::from)
    }
}

fn framed_path(output: &[u8]) -> Option<String> {
    let start = output
        .windows(SHELL_PATH_MARKER.len())
        .rposition(|bytes| bytes == SHELL_PATH_MARKER)?
        + SHELL_PATH_MARKER.len();
    let path = &output[start..];
    let end = path.iter().position(|byte| *byte == 0)?;
    std::str::from_utf8(&path[..end]).ok().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::find_program;
    use std::fs;
    use std::io;
    use std::time::Instant;
    use uuid::Uuid;

    struct ShellHome(PathBuf);

    impl ShellHome {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("soi-shell-path-{}", Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for ShellHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_zsh_path_discovery_is_interactive() {
        for shell in ["zsh", "/bin/zsh", "/opt/homebrew/bin/zsh"] {
            let command = path_command(shell);
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args[..3], ["-i", "-l", "-c"]);
            assert!(args[3].to_str().unwrap().contains("SOI_AGENT_PATH"));
        }
        for shell in ["/bin/bash", "fish", "/bin/sh"] {
            let command = path_command(shell);
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args, ["-l", "-c", "printenv PATH"]);
        }
    }

    #[test]
    fn zsh_path_includes_zshrc_tools_with_an_isolated_home() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::var("PATH").unwrap_or_default();
        let Some(zsh) = find_program("zsh", &path) else {
            eprintln!("zsh is not installed; real-zsh startup check skipped");
            return;
        };
        let home = ShellHome::new();
        let tools = home.0.join("nvm tools");
        fs::create_dir(&tools).unwrap();
        for name in ["mate", "node"] {
            let file = tools.join(name);
            fs::write(&file, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
        }
        for custom_zdotdir in [false, true] {
            let dotdir = if custom_zdotdir {
                fs::write(home.0.join(".zshrc"), "exit 17\n").unwrap();
                let dir = home.0.join("dotfiles");
                fs::create_dir(&dir).unwrap();
                dir
            } else {
                home.0.clone()
            };
            fs::write(dotdir.join(".zprofile"), "print -r -- 'login greeting'\n").unwrap();
            fs::write(
                dotdir.join(".zshrc"),
                "[[ -o interactive ]] || return\nexport PATH=\"$HOME/nvm tools:$PATH\"\nprint -r -- 'interactive greeting'\nalias printf='print -r -- not-a-path'\n",
            )
            .unwrap();
            fs::write(dotdir.join(".zlogout"), "print -r -- 'logout greeting'\n").unwrap();
            let mut command = path_command(zsh.to_str().unwrap());
            command
                .env_clear()
                .env("HOME", &home.0)
                .env("TERM", "dumb")
                .env("PATH", "/usr/bin:/bin");
            if custom_zdotdir {
                command.env("ZDOTDIR", &dotdir);
            }
            let discovered = probe_path(&mut command, true, Duration::from_secs(3)).unwrap();
            assert_eq!(find_program("mate", &discovered), Some(tools.join("mate")));
            assert_eq!(find_program("node", &discovered), Some(tools.join("node")));
            assert!(!discovered.contains("greeting"));
        }
    }

    #[test]
    fn framed_shell_path_ignores_noise_and_rejects_incomplete_output() {
        assert_eq!(
            framed_path(b"hello\n\0SOI_AGENT_PATH\0/tools with spaces:/bin\0goodbye\n"),
            Some("/tools with spaces:/bin".into())
        );
        assert_eq!(framed_path(b"/bin\n"), None);
        assert_eq!(framed_path(b"\0SOI_AGENT_PATH\0/bin"), None);
        assert_eq!(framed_path(b"\0SOI_AGENT_PATH\0\xff\0"), None);
    }

    #[test]
    fn shell_path_drains_large_startup_and_logout_output() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"
            i=0
            while [ "$i" -lt 10000 ]; do printf 'startup greeting\n'; i=$((i+1)); done
            printf '\0SOI_AGENT_PATH\0/tools:/bin\0'
            i=0
            while [ "$i" -lt 10000 ]; do printf 'logout greeting\n'; i=$((i+1)); done
        "#,
        ]);
        assert_eq!(
            probe_path(&mut command, true, Duration::from_secs(3)),
            Some("/tools:/bin".into())
        );
    }

    #[test]
    fn failed_shell_path_probes_allow_the_existing_fallback() {
        for script in [
            "exit 1",
            "printf '\\0SOI_AGENT_PATH\\0/bin\\0'; exit 1",
            "printf greeting",
        ] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            assert_eq!(probe_path(&mut command, true, Duration::from_secs(1)), None);
        }
        let mut command = Command::new("/nonexistent/soi-shell");
        assert_eq!(probe_path(&mut command, true, Duration::from_secs(1)), None);
    }

    #[test]
    fn shell_path_probe_times_out_and_reaps_the_child() {
        let home = ShellHome::new();
        let pid_file = home.0.join("pid");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf '%s' \"$$\" > \"$1\"; while :; do :; done",
                "soi-test",
            ])
            .arg(&pid_file);
        let started = Instant::now();
        assert_eq!(
            probe_path(&mut command, true, Duration::from_millis(200)),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: libc::pid_t = fs::read_to_string(pid_file).unwrap().parse().unwrap();
        // SAFETY: signal 0 checks existence without sending a signal.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "the timed-out shell was reaped"
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn non_zsh_path_output_keeps_login_shell_parsing() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'greeting\\n/tools:/bin\\n\\n'"]);
        assert_eq!(
            probe_path(&mut command, false, Duration::from_secs(1)),
            Some("/tools:/bin".into())
        );
    }

    #[test]
    fn shell_path_cache_is_separate_for_each_configured_shell() {
        let paths = ShellPaths::default();
        assert_eq!(
            cached_path(&paths, "/bin/zsh", || "/nvm/bin".into()),
            "/nvm/bin"
        );
        assert_eq!(
            cached_path(&paths, "/bin/bash", || "/bash/bin".into()),
            "/bash/bin"
        );
        assert_eq!(
            cached_path(&paths, "/bin/zsh", || panic!("already cached")),
            "/nvm/bin"
        );
    }

    #[test]
    fn concurrent_shell_path_queries_only_probe_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let paths = ShellPaths::default();
        let calls = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    assert_eq!(
                        cached_path(&paths, "/bin/zsh", || {
                            calls.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(20));
                            "/nvm/bin".into()
                        }),
                        "/nvm/bin"
                    );
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
