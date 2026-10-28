//! The agents soi can start (Task details, New task worktree), as set up in
//! Settings ▸ Agents: which tool, and its model and variant, so one tool can
//! be there several times, say OpenCode with Qwen and with GPT. Kept in
//! `~/.config/soi/agent-presets.json`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The tools a preset can start, and their programs.
pub const TOOLS: [(&str, &str); 2] = [("claude", "Claude Code"), ("opencode", "OpenCode")];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// What the dropdowns show.
    pub name: String,
    /// One of `TOOLS`' programs.
    pub program: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Claude Code's effort, e.g. `high`.
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
        program == "claude"
    }

    /// The command that starts it, with `brief` as its first prompt and
    /// `tasks` (the project's task folders, outside the worktree) open to it:
    /// Claude Code may read and edit there without asking (see `env` for
    /// OpenCode).
    pub fn command(&self, brief: Option<String>, tasks: Option<&Path>) -> Vec<String> {
        self.build(brief, tasks, true)
    }

    /// `command`, for the tool as installed on `path`: without the options
    /// its version doesn't take (see `opencode_takes_model`).
    pub fn launch_command(
        &self,
        brief: Option<String>,
        tasks: Option<&Path>,
        path: &str,
    ) -> Vec<String> {
        let model = self.program != "opencode" || opencode_takes_model(path);
        self.build(brief, tasks, model)
    }

    /// With `model` false: no `--model`, and no `--agent` in the arguments
    /// before a `--` (after one, they're the wrapper's).
    fn build(&self, brief: Option<String>, tasks: Option<&Path>, model: bool) -> Vec<String> {
        let program = self.program.as_str();
        let mut command: Vec<String> = self.wrapper.split_whitespace().map(String::from).collect();
        command.push(program.to_string());
        let takes_model = model;
        let model = self.model.trim();
        let variant = self.variant.trim();
        if !model.is_empty() && takes_model {
            command.extend(["--model".to_string(), model.to_string()]);
        }
        if !variant.is_empty() && Self::takes_variant(program) {
            command.extend(["--effort".to_string(), variant.to_string()]);
        }
        if let Some(brief) = brief {
            // `brief` starts with "# Task", so it can never be read as a flag.
            debug_assert!(!brief.starts_with('-'));
            match program {
                "claude" => command.push(brief),
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

/// Whether OpenCode's terminal app, as installed on `path`, takes
/// `--model` and `--agent`. 1.x does; 2.x so far doesn't and quits on them
/// (anomalyco/opencode#56218), so it starts with the model used last.
/// Asked once per installed binary, so an update counts without a restart;
/// when it can't tell, the options stay.
fn opencode_takes_model(path: &str) -> bool {
    static ASKED: OnceLock<Mutex<HashMap<(PathBuf, SystemTime), bool>>> = OnceLock::new();
    let Some(program) = crate::agents::find_program("opencode", path) else {
        return true;
    };
    let Some(modified) = fs::metadata(&program).and_then(|m| m.modified()).ok() else {
        return true;
    };
    let mut asked = ASKED.get_or_init(Mutex::default).lock().unwrap();
    *asked.entry((program.clone(), modified)).or_insert_with(|| {
        std::process::Command::new(&program)
            .arg("--version")
            .env("PATH", path)
            .output()
            .ok()
            .and_then(|out| major_version(&String::from_utf8_lossy(&out.stdout)))
            .is_none_or(|major| major < 2)
    })
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

/// The presets set up, or the defaults. Unknown tools are left out.
pub fn load() -> Vec<Preset> {
    let presets: Vec<Preset> = fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(defaults);
    presets
        .into_iter()
        .filter(|preset| TOOLS.iter().any(|(program, _)| *program == preset.program))
        .collect()
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

    fn brief() -> Option<String> {
        Some("# Task: x".into())
    }

    #[test]
    fn plain_tools_get_the_brief_as_their_first_prompt() {
        let command = |program| Preset::new(program).command(brief(), None);
        assert_eq!(command("claude"), ["claude", "# Task: x"]);
        assert_eq!(command("opencode"), ["opencode", "--prompt", "# Task: x"]);
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
    fn opencode_2_gets_no_model_or_agent() {
        let preset = Preset {
            model: "iq-router/qwen-3.8".into(),
            args: "--agent sol --auto --agent=plan -- --agent mate".into(),
            wrapper: "mate".into(),
            ..Preset::new("opencode")
        };
        assert_eq!(
            preset.build(brief(), None, false),
            [
                "mate",
                "opencode",
                "--prompt",
                "# Task: x",
                "--auto",
                "--",
                "--agent",
                "mate"
            ]
        );
        assert_eq!(
            preset.build(None, None, true)[..4],
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
