//! The agents soi can start (Task details, New task worktree), as set up in
//! Settings ▸ Agents: which tool, and its model and variant, so one tool can
//! be there several times, say OpenCode with Qwen and with GPT. Kept in
//! `~/.config/soi/agent-presets.json`.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

/// The tools a preset can start, and their programs.
pub const TOOLS: [(&str, &str); 3] = [
    ("claude", "Claude Code"),
    ("opencode", "OpenCode"),
    ("pi", "Pi"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// What the dropdowns show.
    pub name: String,
    /// One of `TOOLS`' programs.
    pub program: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Claude Code's effort or Pi's thinking level, e.g. `high`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub variant: String,
    /// More arguments, split on whitespace, e.g. `--agent plan`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// A command that starts the tool, put in front of it, e.g. `mate` for
    /// `mate claude …`. Split on whitespace.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wrapper: String,
}

impl Preset {
    pub fn new(program: &str) -> Self {
        Self {
            name: crate::agents::display_name(program).into(),
            program: program.into(),
            model: String::new(),
            variant: String::new(),
            args: String::new(),
            wrapper: String::new(),
        }
    }

    /// Whether the tool takes a variant from its command line. OpenCode's
    /// terminal app doesn't (only `opencode run`): there, an OpenCode agent
    /// with that variant does it (`--agent` in the arguments).
    pub fn takes_variant(program: &str) -> bool {
        Self::variant_option(program).is_some()
    }

    fn variant_option(program: &str) -> Option<&'static str> {
        match program {
            "claude" => Some("--effort"),
            "pi" => Some("--thinking"),
            _ => None,
        }
    }

    /// The command that starts it, with `brief` as its first prompt and
    /// `tasks` (the project's task folders, outside the worktree) open to it:
    /// Claude Code may read and edit there without asking (see `env` for
    /// OpenCode).
    pub fn command(&self, brief: Option<String>, tasks: Option<&Path>) -> Vec<String> {
        self.build(brief, tasks, Compat::default())
    }

    /// `command`, for the tool as installed on `path` (see `Compat`).
    pub fn launch_command(
        &self,
        brief: Option<String>,
        tasks: Option<&Path>,
        path: &str,
    ) -> Vec<String> {
        self.build(brief, tasks, Compat::of(&self.program, path))
    }

    /// Without `compat.model`: no `--model`, and no `--agent` in the
    /// arguments before a `--` (after one, they're the wrapper's).
    fn build(&self, brief: Option<String>, tasks: Option<&Path>, compat: Compat) -> Vec<String> {
        let program = self.program.as_str();
        let mut command: Vec<String> = self.wrapper.split_whitespace().map(String::from).collect();
        command.push(program.to_string());
        if compat.standalone {
            command.push("--standalone".to_string());
        }
        let takes_model = compat.model;
        let model = self.model.trim();
        let variant = self.variant.trim();
        if !model.is_empty() && takes_model {
            command.extend(["--model".to_string(), model.to_string()]);
        }
        if !variant.is_empty()
            && let Some(option) = Self::variant_option(program)
        {
            command.extend([option.to_string(), variant.to_string()]);
        }
        if let Some(brief) = brief {
            // `brief` starts with "# Task", so it can never be read as a flag.
            debug_assert!(!brief.starts_with('-'));
            match program {
                "claude" | "pi" => command.push(brief),
                // OpenCode's TUI takes its first prompt as an option.
                "opencode" => command.extend(["--prompt".to_string(), brief]),
                _ => {}
            }
        }
        // After the prompt, so an option that takes several values can't
        // take it too (like Claude Code's `--add-dir`).
        let mut args = self.args.split_whitespace();
        while let Some(arg) = args.next() {
            if arg == "--" {
                command.push(arg.to_string());
                command.extend(args.by_ref().map(String::from));
            } else if !takes_model && arg == "--agent" {
                args.next();
            } else if takes_model || !arg.starts_with("--agent=") {
                command.push(arg.to_string());
            }
        }
        if program == "claude"
            && let Some(tasks) = tasks
        {
            // `//` starts an absolute path in a permission rule; Edit rules
            // cover writing files too.
            command.extend([
                "--add-dir".to_string(),
                tasks.to_string_lossy().into_owned(),
                "--allowedTools".to_string(),
                format!("Edit(/{}/**)", tasks.display()),
            ]);
        }
        command
    }

    /// What its environment adds: OpenCode may use `tasks`, outside the
    /// worktree, without asking (it edits files without asking anyway).
    pub fn env(&self, tasks: Option<&Path>) -> Vec<(String, String)> {
        match (self.program.as_str(), tasks) {
            ("opencode", Some(tasks)) => {
                let pattern = format!("{}/**", tasks.display());
                let config = serde_json::json!({
                    "permission": { "external_directory": { pattern: "allow" } }
                });
                vec![("OPENCODE_CONFIG_CONTENT".into(), config.to_string())]
            }
            _ => Vec::new(),
        }
    }
}

impl Preset {
    /// The command as you'd type it, with `<brief>` for the brief.
    pub fn preview(&self) -> String {
        self.command(Some("<brief>".into()), None)
            .iter()
            .map(|arg| {
                if !arg.is_empty()
                    && arg
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+".contains(c))
                {
                    arg.clone()
                } else {
                    format!("'{}'", arg.replace('\'', "'\\''"))
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How a tool's command line changes with its version, as installed. The
/// default is what versions soi can't tell apart take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compat {
    /// Takes `--model` and `--agent`. OpenCode 2.x so far doesn't and quits
    /// on them (anomalyco/opencode#56218), so it starts with its default
    /// model, else the one used last.
    pub model: bool,
    /// Needs `--standalone` for a server of its own. OpenCode 2.x otherwise
    /// uses a background server shared by all its terminals, and its plugins
    /// and config there don't see the tab's environment: soi's plugin can't
    /// tell which tab it reports for, and `env`'s task folders don't count.
    pub standalone: bool,
}

impl Default for Compat {
    fn default() -> Self {
        Self {
            model: true,
            standalone: false,
        }
    }
}

impl Compat {
    /// For `program` as installed on `path`.
    pub fn of(program: &str, path: &str) -> Self {
        match (program, major_version_of(program, path)) {
            ("opencode", Some(major)) if major >= 2 => Self {
                model: false,
                standalone: true,
            },
            _ => Self::default(),
        }
    }
}

/// Major versions found, by binary and its modification time.
type Versions = HashMap<(PathBuf, SystemTime), Option<u32>>;

/// The major version of `program` as installed on `path`, from its
/// `--version`. Asked once per installed binary, so an update counts without
/// a restart. Only for tools whose versions differ (see `Compat`): others
/// aren't run for it.
fn major_version_of(program: &str, path: &str) -> Option<u32> {
    static ASKED: OnceLock<Mutex<Versions>> = OnceLock::new();
    if program != "opencode" {
        return None;
    }
    let binary = crate::agents::find_program(program, path)?;
    let modified = fs::metadata(&binary).and_then(|m| m.modified()).ok()?;
    #[expect(
        clippy::unwrap_used,
        reason = "nothing panics while this lock is held, so it is never poisoned"
    )]
    let mut asked = ASKED.get_or_init(Mutex::default).lock().unwrap();
    *asked.entry((binary.clone(), modified)).or_insert_with(|| {
        probe_major_version(
            Command::new(&binary).arg("--version").env("PATH", path),
            Duration::from_secs(3),
        )
    })
}

/// Version detection must not inherit a terminal, allocate unlimited output,
/// or wait for pipe EOF held by a background descendant of the helper.
fn probe_major_version(command: &mut Command, timeout: Duration) -> Option<u32> {
    const LIMIT: usize = 64 * 1024;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let result = (|| {
        let mut stdout = child.stdout.take()?;
        let fd = stdout.as_raw_fd();
        // SAFETY: stdout owns the live descriptor throughout both calls.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        let started = Instant::now();
        let mut output = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let status = child.try_wait().ok()?;
            loop {
                if started.elapsed() >= timeout {
                    return None;
                }
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if n > LIMIT - output.len() {
                            return None;
                        }
                        output.extend_from_slice(&buffer[..n]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => return None,
                }
            }
            if let Some(status) = status {
                return status
                    .success()
                    .then(|| major_version(&String::from_utf8_lossy(&output)))
                    .flatten();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// `2` in `opencode v2.0.28` or `2.0.28`.
fn major_version(version: &str) -> Option<u32> {
    let version = version.split_whitespace().last()?;
    let version = version.strip_prefix('v').unwrap_or(version);
    version.split('.').next()?.parse().ok()
}

/// One of each tool, as they come.
pub fn defaults() -> Vec<Preset> {
    TOOLS
        .iter()
        .map(|(program, _)| Preset::new(program))
        .collect()
}

fn path() -> PathBuf {
    crate::dirs::config_home().join("soi/agent-presets.json")
}

/// The presets set up, or the defaults. Unknown tools are left out, and a
/// tool with no preset gets its default one, so a tool added to soi shows up
/// in a list saved before it.
pub fn load() -> Vec<Preset> {
    let presets: Vec<Preset> = fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(defaults);
    complete(presets)
}

fn complete(presets: Vec<Preset>) -> Vec<Preset> {
    let mut presets: Vec<Preset> = presets
        .into_iter()
        .filter(|preset| TOOLS.iter().any(|(program, _)| *program == preset.program))
        .collect();
    for (program, _) in TOOLS {
        if !presets.iter().any(|preset| preset.program == program) {
            presets.push(Preset::new(program));
        }
    }
    presets
}

pub fn save(presets: &[Preset]) -> io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(presets).map_err(io::Error::other)?;
    fs::write(path, json + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_probe_bounds_output_and_rejects_failed_helpers() {
        for script in [
            "printf '2.0.0'; exit 1",
            "i=0; while [ $i -lt 17000 ]; do printf abcd; i=$((i+1)); done; printf ' 2.0.0'",
        ] {
            assert_eq!(
                probe_major_version(
                    Command::new("/bin/sh").args(["-c", script]),
                    Duration::from_secs(2)
                ),
                None
            );
        }
        assert_eq!(
            probe_major_version(
                Command::new("/bin/sh").args(["-c", "printf 'opencode 2.0.0'"]),
                Duration::from_secs(2)
            ),
            Some(2)
        );
    }

    #[test]
    fn version_probe_does_not_wait_for_a_stalled_program() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("soi-version-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let binary = root.join("opencode");
        fs::write(&binary, "#!/bin/sh\nexec /bin/sleep 4\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(major_version_of("opencode", root.to_str().unwrap()), None);
        fs::remove_dir_all(root).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(3500),
            "unbounded version probe"
        );
    }

    fn brief() -> Option<String> {
        Some("# Task: x".into())
    }

    #[test]
    fn a_saved_list_gets_a_default_preset_for_a_tool_it_lacks() {
        let sol = Preset {
            name: "OpenCode 6.1 Sol".into(),
            model: "gpt-6.1-sol".into(),
            ..Preset::new("opencode")
        };
        let saved = vec![Preset::new("claude"), sol.clone(), Preset::new("gone")];
        assert_eq!(
            complete(saved),
            [Preset::new("claude"), sol, Preset::new("pi")]
        );
        assert_eq!(complete(Vec::new()), defaults());
    }

    #[test]
    fn plain_tools_get_the_brief_as_their_first_prompt() {
        let command = |program| Preset::new(program).command(brief(), None);
        assert_eq!(command("claude"), ["claude", "# Task: x"]);
        assert_eq!(command("opencode"), ["opencode", "--prompt", "# Task: x"]);
        assert_eq!(command("pi"), ["pi", "# Task: x"]);
        assert_eq!(Preset::new("opencode").command(None, None), ["opencode"]);
    }

    #[test]
    fn models_and_variants_go_the_way_each_tool_takes_them() {
        let preset = |program: &str, model: &str, variant: &str| Preset {
            model: model.into(),
            variant: variant.into(),
            ..Preset::new(program)
        };
        assert_eq!(
            preset("claude", "opus", "high").command(None, None),
            ["claude", "--model", "opus", "--effort", "high"]
        );
        assert_eq!(
            preset("pi", "anthropic/claude-opus-5-5", "high").command(None, None),
            [
                "pi",
                "--model",
                "anthropic/claude-opus-5-5",
                "--thinking",
                "high"
            ]
        );
        // OpenCode's TUI has no variant option.
        assert_eq!(
            preset("opencode", "qwen/qwen3-coder", "high").command(brief(), None),
            [
                "opencode",
                "--model",
                "qwen/qwen3-coder",
                "--prompt",
                "# Task: x"
            ]
        );
    }

    #[test]
    fn opencode_2_gets_its_own_server_and_no_model_or_agent() {
        let preset = Preset {
            model: "iq-router/qwen-3.8".into(),
            args: "--agent sol --auto --agent=plan -- --agent mate".into(),
            wrapper: "mate".into(),
            ..Preset::new("opencode")
        };
        let opencode_2 = Compat {
            model: false,
            standalone: true,
        };
        assert_eq!(
            preset.build(brief(), None, opencode_2),
            [
                "mate",
                "opencode",
                "--standalone",
                "--prompt",
                "# Task: x",
                "--auto",
                "--",
                "--agent",
                "mate"
            ]
        );
        assert_eq!(
            preset.build(None, None, Compat::default())[..4],
            ["mate", "opencode", "--model", "iq-router/qwen-3.8"]
        );
        // Claude Code keeps its options whatever OpenCode is installed.
        let claude = Preset {
            model: "opus".into(),
            ..Preset::new("claude")
        };
        assert_eq!(
            claude.launch_command(None, None, ""),
            ["claude", "--model", "opus"]
        );
    }

    #[test]
    fn versions_are_read_with_or_without_name_and_v() {
        assert_eq!(major_version("opencode v2.0.28\n"), Some(2));
        assert_eq!(major_version("1.19.5"), Some(1));
        assert_eq!(major_version(""), None);
        assert_eq!(major_version("unknown"), None);
    }

    #[test]
    fn wrappers_go_in_front() {
        let preset = Preset {
            model: "opus".into(),
            args: "-- --no-git".into(),
            wrapper: " mate ".into(),
            ..Preset::new("claude")
        };
        assert_eq!(
            preset.command(brief(), Some(Path::new("/plans/soi"))),
            [
                "mate",
                "claude",
                "--model",
                "opus",
                "# Task: x",
                "--",
                "--no-git",
                "--add-dir",
                "/plans/soi",
                "--allowedTools",
                "Edit(//plans/soi/**)"
            ]
        );
        assert_eq!(
            preset.preview(),
            "mate claude --model opus '<brief>' -- --no-git"
        );
    }

    #[test]
    fn previews_read_like_a_shell_command() {
        let preset = Preset {
            model: "openrouter/qwen/qwen3-coder".into(),
            args: "--agent it's".into(),
            ..Preset::new("opencode")
        };
        assert_eq!(
            preset.preview(),
            "opencode --model openrouter/qwen/qwen3-coder --prompt '<brief>' --agent 'it'\\''s'"
        );
    }

    #[test]
    fn arguments_and_task_folders_come_after_the_prompt() {
        let preset = Preset {
            args: " --agent  plan ".into(),
            ..Preset::new("claude")
        };
        assert_eq!(
            preset.command(brief(), Some(Path::new("/plans/soi"))),
            [
                "claude",
                "# Task: x",
                "--agent",
                "plan",
                "--add-dir",
                "/plans/soi",
                "--allowedTools",
                "Edit(//plans/soi/**)"
            ]
        );
        assert!(preset.env(Some(Path::new("/plans/soi"))).is_empty());
        // OpenCode is told in its environment instead.
        let opencode = Preset::new("opencode");
        assert_eq!(
            opencode.command(brief(), Some(Path::new("/plans/soi"))),
            ["opencode", "--prompt", "# Task: x"]
        );
        assert_eq!(
            opencode.env(Some(Path::new("/plans/soi"))),
            [(
                "OPENCODE_CONFIG_CONTENT".to_string(),
                r#"{"permission":{"external_directory":{"/plans/soi/**":"allow"}}}"#.to_string()
            )]
        );
        assert!(opencode.env(None).is_empty());
    }
}
