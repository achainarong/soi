//! Coding agents running in terminals (Claude Code, OpenCode, …): what each
//! one is doing, and how agents tell soi about it.
//!
//! soi guesses an agent's state from its terminal (busy output, bells,
//! notifications). Agents with an integration report it exactly instead:
//! Claude Code through hooks that run `soi claude-hook`, OpenCode through a
//! plugin, Pi through an extension. Reports arrive as JSON lines on a Unix socket
//! (`$XDG_RUNTIME_DIR/soi.sock`), whose path each tab gets in
//! `SOI_SOCKET`, along with its own id in `SOI_TAB_ID`.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Programs recognized as agents when no list is set.
pub const DEFAULT_AGENTS: &[&str] = &[
    "claude",
    "opencode",
    "pi",
    "codex",
    "aider",
    "gemini",
    "amp",
    "goose",
    "crush",
    "cursor-agent",
    "qwen",
];

/// Output within this long means the agent is working (when guessing).
pub const BUSY_WINDOW: Duration = Duration::from_millis(1500);

/// The largest report accepted on the socket.
pub(crate) const MAX_REPORT_BYTES: usize = 64 * 1024;

/// Marks the hook commands and the plugin file as soi's own.
const MARKER: &str = "soi-agent-integration";
/// The same, from when soi was called rim; those get replaced.
const RIM_MARKER: &str = "rim-agent-integration";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Working,
    /// Waits for an answer or an approval.
    Input,
    /// Finished its turn.
    Done,
    /// Quiet; without a report, waiting and done look the same.
    Idle,
    Exited,
}

impl State {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "working" => Self::Working,
            "input" => Self::Input,
            "done" => Self::Done,
            "idle" => Self::Idle,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Input => "Needs input",
            Self::Done => "Done",
            Self::Idle => "Idle",
            Self::Exited => "Exited",
        }
    }

    /// CSS class for the state's color.
    pub fn class(self) -> &'static str {
        match self {
            Self::Working => "agent-working",
            Self::Input => "agent-input",
            Self::Done => "agent-done",
            Self::Idle => "agent-idle",
            Self::Exited => "agent-exited",
        }
    }

    /// Order in the panel: what needs you first.
    pub fn rank(self) -> u8 {
        match self {
            Self::Input => 0,
            Self::Done => 1,
            Self::Idle => 2,
            Self::Working => 3,
            Self::Exited => 4,
        }
    }
}

/// The agent in one tab, if any.
#[derive(Clone, Debug)]
pub struct Agent {
    /// The agent's program; `None` while no agent runs in the tab.
    pub program: Option<String>,
    pub state: State,
    /// The agent reports its state, so soi doesn't guess.
    pub reported: bool,
    /// It stopped working (or exited) while you weren't looking.
    pub unseen: bool,
    /// What the agent last said (a question, an error, a notification).
    pub message: String,
    /// When the state began.
    pub since: Instant,
    /// Its model and context size, when it reports them.
    pub usage: Option<Usage>,
    /// Time spent working and waiting for you, before the current state.
    worked: Duration,
    waited: Duration,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            program: None,
            state: State::Idle,
            reported: false,
            unseen: false,
            message: String::new(),
            since: Instant::now(),
            usage: None,
            worked: Duration::ZERO,
            waited: Duration::ZERO,
        }
    }
}

impl Agent {
    pub fn is_agent(&self) -> bool {
        self.program.is_some()
    }

    /// Applies what the terminal shows: the foreground program (whether it's
    /// a known agent) and whether it printed output lately. Returns whether
    /// anything changed.
    pub fn observe(
        &mut self,
        foreground: Option<&str>,
        known: bool,
        busy: bool,
        visible: bool,
        now: Instant,
    ) -> bool {
        let before = self.snapshot();
        match foreground {
            // A reporting agent may have any name (e.g. started via node).
            Some(program) if known || self.reported => {
                if self.program.is_none() && self.reported {
                    // It reported before soi noticed it.
                    self.program = Some(program.to_string());
                } else if self.program.as_deref() != Some(program) || self.state == State::Exited {
                    // A new agent in this tab: start fresh, without an alert.
                    *self = Self {
                        program: Some(program.to_string()),
                        since: now,
                        ..Self::default()
                    };
                }
                if !self.reported {
                    if busy {
                        self.set(State::Working, None, visible, now);
                    } else if self.state == State::Working {
                        self.set(State::Idle, None, visible, now);
                    }
                }
            }
            _ if self.program.is_some() && self.state != State::Exited => {
                self.reported = false;
                self.set(State::Exited, None, visible, now);
            }
            _ => {}
        }
        if self.state == State::Exited && visible {
            // Seen it go: the tab is a plain terminal again.
            *self = Self::default();
        }
        before != self.snapshot()
    }

    /// A state the agent reported itself.
    pub fn report(&mut self, state: State, message: &str, visible: bool, now: Instant) -> bool {
        let before = self.snapshot();
        self.reported = true;
        self.set(state, Some(message), visible, now);
        before != self.snapshot()
    }

    /// A bell or notification from the terminal.
    pub fn attention(&mut self, message: &str, visible: bool, now: Instant) -> bool {
        if !self.is_agent() || self.reported {
            return false;
        }
        let before = self.snapshot();
        self.set(State::Input, Some(message), visible, now);
        before != self.snapshot()
    }

    /// You looked at the tab.
    pub fn seen(&mut self) -> bool {
        let changed = self.unseen || self.state == State::Exited;
        self.unseen = false;
        if self.state == State::Exited {
            *self = Self::default();
        }
        changed
    }

    /// How long it has worked, and waited for your input, so far.
    pub fn totals(&self, now: Instant) -> (Duration, Duration) {
        let current = now.saturating_duration_since(self.since);
        match self.state {
            State::Working => (self.worked + current, self.waited),
            State::Input => (self.worked, self.waited + current),
            _ => (self.worked, self.waited),
        }
    }

    fn set(&mut self, state: State, message: Option<&str>, visible: bool, now: Instant) {
        if state != self.state {
            (self.worked, self.waited) = self.totals(now);
            let stopped = matches!(self.state, State::Working) || state == State::Exited;
            self.unseen = match state {
                State::Working => false,
                State::Input | State::Exited => !visible,
                State::Done | State::Idle => (self.unseen || stopped) && !visible,
            };
            self.state = state;
            self.since = now;
            if message.is_none() {
                self.message.clear();
            }
        }
        if let Some(message) = message {
            self.message = message.chars().take(300).collect();
        }
    }

    fn snapshot(&self) -> (Option<String>, State, bool, bool, String) {
        (
            self.program.clone(),
            self.state,
            self.reported,
            self.unseen,
            self.message.clone(),
        )
    }
}

/// The agent list setting: names separated by commas or spaces.
pub fn agent_list(setting: Option<&str>) -> Vec<String> {
    match setting {
        Some(list) if !list.trim().is_empty() => list
            .split([',', ' '])
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(String::from)
            .collect(),
        _ => DEFAULT_AGENTS.iter().map(|name| name.to_string()).collect(),
    }
}

/// Whether a program name (`comm`, at most 15 characters) is one of the
/// agents. Bun-compiled tools can show up with a leading dot, and npm's
/// native binaries end in `.exe` (Claude Code's `claude.exe`).
pub fn is_known(program: &str, agents: &[String]) -> bool {
    let program = program.trim_start_matches('.');
    let program = program.strip_suffix(".exe").unwrap_or(program);
    agents.iter().any(|agent| {
        // The kernel cuts process names at 15 characters.
        let agent: String = agent.chars().take(15).collect();
        agent == program
    })
}

// ---------------------------------------------------------------------------
// Reports

/// One message on the socket.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub tab: Uuid,
    pub state: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// What an agent's latest reply used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// As the agent names it, e.g. `claude-opus-5-5`.
    pub model: String,
    /// Tokens in its context window.
    pub context: u64,
    /// The window's size, when the agent knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

impl Usage {
    /// The window it reported, or else the model's known one; none when the
    /// context is already larger (the model has a bigger window enabled).
    pub fn window(&self) -> Option<u64> {
        self.window
            .or_else(|| context_window(&self.model))
            .filter(|window| *window > 0 && *window >= self.context)
    }

    /// "Opus 5.5 · 45k context", or "… · 45k of 200k context (23%)".
    pub fn label(&self) -> String {
        let context = match self.window() {
            Some(window) => format!(
                "{} of {} context ({}%)",
                token_count(self.context),
                token_count(window),
                self.context * 100 / window
            ),
            None => format!("{} context", token_count(self.context)),
        };
        format!("{} · {context}", model_name(&self.model))
    }
}

/// Context windows by model ID, from models.dev.
const CONTEXT_WINDOWS: &str = include_str!("agents/context-windows.tsv");

/// A model's context window by its ID: as given (`claude-opus-5-5`), with a
/// provider (`anthropic/claude-opus-5-5`), or a date
/// (`claude-sonnet-4-5-20250929`, `…@20250929`). `[1m]` asks for 1M.
pub fn context_window(model: &str) -> Option<u64> {
    let model = model.trim().to_ascii_lowercase();
    if model.ends_with("[1m]") {
        return Some(1_000_000);
    }
    let model = model.rsplit('/').next().unwrap_or_default();
    let model = model.split('@').next().unwrap_or_default();
    let find = |id: &str| {
        CONTEXT_WINDOWS
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .find(|(known, _)| *known == id)
            .and_then(|(_, window)| window.parse().ok())
    };
    find(model).or_else(|| {
        let (base, date) = model.rsplit_once('-')?;
        (date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
            .then(|| find(base))
            .flatten()
    })
}

/// `claude-opus-5-5` → "Opus 5.5", `claude-3-5-haiku-20241022` → "Haiku
/// 3.5"; other names stay as they are.
pub fn model_name(model: &str) -> String {
    let Some(rest) = model.strip_prefix("claude-") else {
        return model.to_string();
    };
    let (mut names, mut version) = (Vec::new(), Vec::new());
    for word in rest.split('-') {
        if word.len() >= 8 && word.bytes().all(|b| b.is_ascii_digit()) {
            // A release date.
        } else if word.bytes().all(|b| b.is_ascii_digit()) {
            version.push(word);
        } else {
            let mut chars = word.chars();
            names.extend(
                chars
                    .next()
                    .map(|c| c.to_uppercase().chain(chars).collect::<String>()),
            );
        }
    }
    let mut name = names.join(" ");
    if !version.is_empty() {
        name = format!("{name} {}", version.join(".")).trim().to_string();
    }
    name
}

/// "820", "45k", "1.2M".
pub fn token_count(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{}k", tokens / 1_000),
        _ => {
            format!("{:.1}", tokens as f64 / 1_000_000.0)
                .trim_end_matches(".0")
                .to_string()
                + "M"
        }
    }
}

/// How much of the transcript's end is read for the latest usage.
const TRANSCRIPT_TAIL: u64 = 512 * 1024;

/// The latest usage in a Claude Code transcript (JSON lines): its last
/// reply in the main conversation, not a subagent's.
pub fn claude_usage(transcript: &Path) -> Option<Usage> {
    use std::io::{Seek, SeekFrom};
    let mut file = fs::File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL)))
        .ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    // The first line may be cut off; it just doesn't parse.
    tail.split(|b| *b == b'\n').rev().find_map(|line| {
        let entry: Value = serde_json::from_slice(line).ok()?;
        if entry.get("type")?.as_str()? != "assistant"
            || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let message = entry.get("message")?;
        // Claude Code's own stand-ins (errors, interruptions) say `<synthetic>`.
        let model = message.get("model")?.as_str()?;
        if model.starts_with('<') || model.contains('\0') {
            return None;
        }
        let usage = message.get("usage")?;
        let tokens = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let context = tokens("input_tokens")
            + tokens("cache_creation_input_tokens")
            + tokens("cache_read_input_tokens")
            + tokens("output_tokens");
        (context > 0).then(|| Usage {
            model: model.chars().take(100).collect(),
            context,
            window: None,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub provider: String,
    /// Empty when the agent didn't say: its latest session in the folder.
    pub id: String,
}

impl SessionMetadata {
    /// The latest session of `program`'s agent, when it can continue it.
    pub fn latest(program: &str) -> Option<Self> {
        let latest = Self {
            provider: program.into(),
            id: String::new(),
        };
        latest.resume_command().is_ok().then_some(latest)
    }

    /// The command that resumes the session. Arguments are passed directly to
    /// the PTY or typed into a shell; IDs are checked, so neither can run
    /// arbitrary commands. Unknown providers can't be resumed.
    pub fn resume_command(&self) -> Result<Vec<String>, String> {
        let words: &[&str] = match (self.provider.as_str(), self.id.is_empty()) {
            ("claude", false) => &["claude", "--resume"],
            ("codex", false) => &["codex", "resume"],
            ("opencode", false) => &["opencode", "--session"],
            ("pi", false) => &["pi", "--session"],
            ("claude", true) => &["claude", "--continue"],
            ("codex", true) => &["codex", "resume", "--last"],
            ("opencode", true) => &["opencode", "--continue"],
            ("pi", true) => &["pi", "--continue"],
            _ => return Err("This provider has no supported resume command.".into()),
        };
        let mut command: Vec<String> = words.iter().map(|word| word.to_string()).collect();
        if !self.id.is_empty() {
            crate::tasks::validate_id(&self.id)?;
            command.push(self.id.clone());
        }
        Ok(command)
    }
}

impl Report {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_REPORT_BYTES {
            return None;
        }
        let report: Report = serde_json::from_slice(bytes).ok()?;
        State::parse(&report.state)?;
        // These fields are displayed through GTK's NUL-terminated strings.
        if report.message.contains('\0')
            || report
                .session
                .as_ref()
                .is_some_and(|s| s.provider.contains('\0'))
            || report
                .usage
                .as_ref()
                .is_some_and(|u| u.model.contains('\0'))
        {
            return None;
        }
        Some(report)
    }
}

/// Where the running soi listens.
pub fn socket_path() -> Option<PathBuf> {
    let dir = crate::platform::current()
        .runtime_dir()
        .filter(|dir| private(dir))?;
    let name = if crate::dev_mode() {
        "soi-dev.sock"
    } else {
        "soi.sock"
    };
    Some(dir.join(name))
}

/// Ours, and only we can add files to it. Not a shared folder like `/tmp`
/// (macOS falls back to it without `$TMPDIR`), where another user could
/// put a socket of their own in our socket's place and receive our reports.
fn private(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    fs::metadata(dir).is_ok_and(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o022 == 0)
}

fn send(socket: &Path, report: &Report) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut line = serde_json::to_vec(report)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.shutdown(std::net::Shutdown::Write)
}

/// Sends a report for the tab this process runs in. Outside soi it does
/// nothing, so hooks never fail because of it.
fn report_from_env(state: State, message: &str) -> i32 {
    report_session_from_env(state, message, None, None)
}

fn report_session_from_env(
    state: State,
    message: &str,
    session: Option<SessionMetadata>,
    usage: Option<Usage>,
) -> i32 {
    let tab = std::env::var("SOI_TAB_ID")
        .ok()
        .and_then(|id| Uuid::parse_str(&id).ok());
    let socket = std::env::var_os("SOI_SOCKET").map(PathBuf::from);
    let (Some(tab), Some(socket)) = (tab, socket) else {
        return 0;
    };
    let state = match state {
        State::Working => "working",
        State::Input => "input",
        State::Done => "done",
        State::Idle | State::Exited => "idle",
    };
    let report = Report {
        tab,
        state: state.into(),
        message: message.into(),
        session,
        usage,
    };
    match send(&socket, &report) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("soi: cannot reach soi at {}: {err}", socket.display());
            0
        }
    }
}

/// `soi status <working|input|done> [message…]`
pub fn status_command(args: &[String]) -> i32 {
    let Some(state) = args.first().and_then(|s| State::parse(s)) else {
        eprintln!("usage: soi status <working|input|done> [message]");
        return 2;
    };
    report_from_env(state, &args[1..].join(" "))
}

/// `soi claude-hook`: reads a Claude Code hook event from stdin.
pub fn claude_hook_command() -> i32 {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let event = serde_json::from_str::<Value>(&input).ok();
    match event.as_ref().and_then(claude_event) {
        Some((state, message)) => {
            let session = event
                .as_ref()
                .and_then(|e| e.get("session_id"))
                .and_then(Value::as_str)
                .filter(|id| crate::tasks::validate_id(id).is_ok())
                .map(|id| SessionMetadata {
                    provider: "claude".into(),
                    id: id.into(),
                });
            let usage = event
                .as_ref()
                .and_then(|e| e.get("transcript_path"))
                .and_then(Value::as_str)
                .and_then(|path| claude_usage(Path::new(path)));
            report_session_from_env(state, &message, session, usage)
        }
        None => 0,
    }
}

/// The start of an agent's reply, small enough for a report.
fn reply(text: &str) -> String {
    text.trim().chars().take(MAX_REPLY_CHARS).collect()
}

/// Agents' replies are cut to this many characters.
const MAX_REPLY_CHARS: usize = 1000;

/// What a Claude Code hook event says about Claude's state.
pub fn claude_event(event: &Value) -> Option<(State, String)> {
    let text = |key: &str| event.get(key).and_then(Value::as_str).unwrap_or("");
    match text("hook_event_name") {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some((State::Working, String::new())),
        "Notification" => {
            let message = text("message");
            // Sent a while after Claude finished; it's still just done.
            let reminder = text("notification_type") == "idle_prompt"
                || message.contains("waiting for your input");
            (!reminder).then(|| (State::Input, message.to_string()))
        }
        // What it said last: what it did, or what it asks.
        "Stop" => Some((State::Done, reply(text("last_assistant_message")))),
        // Says which session it is right away, to resume it after a restart.
        "SessionStart" => Some((State::Idle, String::new())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Installing the integrations

/// The command Claude Code's hooks run. Does nothing outside soi.
pub const CLAUDE_COMMAND: &str =
    "[ -z \"$SOI_BIN\" ] || \"$SOI_BIN\" claude-hook # soi-agent-integration";

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
];

fn home() -> PathBuf {
    crate::dirs::home_dir()
}

pub fn claude_settings_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
        .join("settings.json")
}

fn is_ours(entry: &Value) -> bool {
    has_marker(entry, MARKER)
}

fn has_marker(entry: &Value, marker: &str) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(marker))
            })
        })
}

/// Adds soi's hooks to Claude Code settings, keeping everything else.
/// Returns the new settings, or `None` when they're all there already.
pub fn with_claude_hooks(settings: &Value) -> Result<Option<Value>, String> {
    let mut settings = settings.clone();
    let root = settings
        .as_object_mut()
        .ok_or("the settings are not a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("\"hooks\" is not a JSON object")?;
    let mut changed = false;
    for event in CLAUDE_EVENTS {
        let entries = hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("hooks.{event} is not a list"))?;
        let before = entries.len();
        entries.retain(|entry| !has_marker(entry, RIM_MARKER));
        changed |= entries.len() != before;
        if entries.iter().any(is_ours) {
            continue;
        }
        let mut entry = json!({ "hooks": [{ "type": "command", "command": CLAUDE_COMMAND }] });
        if event.ends_with("ToolUse") {
            entry["matcher"] = json!("*");
        }
        entries.push(entry);
        changed = true;
    }
    Ok(changed.then_some(settings))
}

pub fn claude_hooks_installed() -> bool {
    let Ok(text) = fs::read_to_string(claude_settings_path()) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|settings| with_claude_hooks(&settings).ok())
        .is_some_and(|update| update.is_none())
}

/// Writes soi's hooks into `~/.claude/settings.json`.
pub fn install_claude_hooks() -> Result<(), String> {
    let path = claude_settings_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => "{}".into(),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    let settings: Value = serde_json::from_str(if text.trim().is_empty() { "{}" } else { &text })
        .map_err(|err| {
        format!(
            "{} is not valid JSON ({err}); left it unchanged",
            path.display()
        )
    })?;
    let Some(updated) = with_claude_hooks(&settings)? else {
        return Ok(());
    };
    let mut json = serde_json::to_string_pretty(&updated).map_err(|err| err.to_string())?;
    json.push('\n');
    write_atomically(&path, &json)
}

/// The OpenCode plugin. It talks to the socket directly: no process per event.
/// One file for OpenCode 1.x and 2.x: 1.x calls the default export's
/// `server` and passes events to its `event` hook; 2.x calls `setup` and
/// ignores `server`, and its events come as a stream, with new names.
pub const OPENCODE_PLUGIN: &str = include_str!("agents/opencode-plugin.js");

pub fn opencode_plugin_path() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("opencode");
    // Older OpenCode versions read `plugin/`, newer ones `plugins/`.
    let dir = if !config.join("plugins").is_dir() && config.join("plugin").is_dir() {
        config.join("plugin")
    } else {
        config.join("plugins")
    };
    dir.join("soi.js")
}

/// The plugin rim wrote, if it's still there.
fn rim_opencode_plugin() -> Option<PathBuf> {
    let path = opencode_plugin_path().with_file_name("rim.js");
    fs::read_to_string(&path)
        .is_ok_and(|text| text.lines().next().unwrap_or("").contains(RIM_MARKER))
        .then_some(path)
}

pub fn opencode_plugin_installed() -> bool {
    fs::read_to_string(opencode_plugin_path()).is_ok_and(|text| text == OPENCODE_PLUGIN)
        && rim_opencode_plugin().is_none()
}

/// Writes the OpenCode plugin, replacing only a file soi wrote.
pub fn install_opencode_plugin() -> Result<(), String> {
    let path = opencode_plugin_path();
    if let Ok(existing) = fs::read_to_string(&path)
        && !existing.lines().next().unwrap_or("").contains(MARKER)
    {
        return Err(format!(
            "{} exists and wasn't written by Soi; left it unchanged",
            path.display()
        ));
    }
    write_atomically(&path, OPENCODE_PLUGIN)?;
    if let Some(old) = rim_opencode_plugin() {
        fs::remove_file(&old).map_err(|err| format!("cannot remove {}: {err}", old.display()))?;
    }
    Ok(())
}

/// The Pi extension. Like the OpenCode plugin, it talks to the socket
/// directly. Pi loads TypeScript itself; this is plain JavaScript so it needs
/// no types from Pi.
pub const PI_EXTENSION: &str = include_str!("agents/pi-extension.js");

pub fn pi_extension_path() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home().join(".pi/agent"))
        .join("extensions/soi.ts")
}

pub fn pi_extension_installed() -> bool {
    fs::read_to_string(pi_extension_path()).is_ok_and(|text| text == PI_EXTENSION)
}

/// Writes the Pi extension, replacing only a file soi wrote.
pub fn install_pi_extension() -> Result<(), String> {
    let path = pi_extension_path();
    if let Ok(existing) = fs::read_to_string(&path)
        && !existing.lines().next().unwrap_or("").contains(MARKER)
    {
        return Err(format!(
            "{} exists and wasn't written by Soi; left it unchanged",
            path.display()
        ));
    }
    write_atomically(&path, PI_EXTENSION)
}

/// `soi install-agents` (run by `make install`): installs the integrations
/// for the agents set up on this machine, i.e. whose config directory exists.
/// Running it again changes nothing.
pub fn install_agents_command() -> i32 {
    let opencode_dir = opencode_plugin_path()
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    struct Integration {
        name: &'static str,
        dir: Option<PathBuf>,
        installed: fn() -> bool,
        install: fn() -> Result<(), String>,
    }
    let integrations = [
        Integration {
            name: "Claude Code",
            dir: claude_settings_path().parent().map(Path::to_path_buf),
            installed: claude_hooks_installed,
            install: install_claude_hooks,
        },
        Integration {
            name: "OpenCode",
            dir: opencode_dir,
            installed: opencode_plugin_installed,
            install: install_opencode_plugin,
        },
        Integration {
            name: "Pi",
            dir: pi_extension_path()
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            installed: pi_extension_installed,
            install: install_pi_extension,
        },
    ];
    let mut failed = false;
    for Integration {
        name,
        dir,
        installed,
        install,
    } in integrations
    {
        if !dir.is_some_and(|dir| dir.is_dir()) {
            println!("{name}: not set up here, skipped");
        } else if installed() {
            println!("{name}: already installed");
        } else {
            match install() {
                Ok(()) => println!("{name}: installed (restart running sessions)"),
                Err(err) => {
                    eprintln!("{name}: {err}");
                    failed = true;
                }
            }
        }
    }
    i32::from(failed)
}

// ---------------------------------------------------------------------------
// Launching

mod shell_path;
pub use shell_path::agent_path;

/// Where `program` would be run from with `path` as PATH.
pub fn find_program(program: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| {
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return executable(Path::new(program)).then(|| PathBuf::from(program));
    }
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|candidate| executable(candidate))
}

/// The name people know an agent program by.
pub fn display_name(program: &str) -> &str {
    match program {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "pi" => "Pi",
        other => other,
    }
}

fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    crate::dirs::write_atomic(path, text.as_bytes(), 0o666)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_reject_oversized_json_and_nul_containing_fields() {
        let mut report = Report {
            tab: Uuid::new_v4(),
            state: "input".into(),
            message: "x".repeat(MAX_REPORT_BYTES),
            session: None,
            usage: None,
        };
        assert!(Report::parse(&serde_json::to_vec(&report).unwrap()).is_none());
        report.message = "\0".into();
        assert!(Report::parse(&serde_json::to_vec(&report).unwrap()).is_none());
        report.message.clear();
        report.session = Some(SessionMetadata {
            provider: "\0".into(),
            id: "abc".into(),
        });
        assert!(Report::parse(&serde_json::to_vec(&report).unwrap()).is_none());
    }

    #[test]
    fn session_metadata_is_optional_and_old_reports_still_parse() {
        let tab = Uuid::new_v4();
        let old = format!(r#"{{"tab":"{tab}","state":"working"}}"#);
        let report: Report = serde_json::from_str(&old).unwrap();
        assert!(report.session.is_none());
        let new = format!(
            r#"{{"tab":"{tab}","state":"done","session":{{"provider":"claude","id":"abc-123"}}}}"#
        );
        let report: Report = serde_json::from_str(&new).unwrap();
        assert_eq!(report.session.unwrap().id, "abc-123");
        assert_eq!(
            Report::parse(new.as_bytes()),
            Some(Report {
                tab,
                state: "done".into(),
                message: String::new(),
                session: Some(SessionMetadata {
                    provider: "claude".into(),
                    id: "abc-123".into()
                }),
                usage: None,
            })
        );
    }

    #[test]
    fn reads_the_latest_main_usage_from_a_claude_transcript() {
        let dir = std::env::temp_dir().join(format!("soi-usage-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        let reply = |model: &str, sidechain: bool, read: u64| {
            json!({
                "type": "assistant",
                "isSidechain": sidechain,
                "message": {"model": model, "usage": {
                    "input_tokens": 2,
                    "cache_creation_input_tokens": 100,
                    "cache_read_input_tokens": read,
                    "output_tokens": 50,
                }},
            })
            .to_string()
        };
        let lines = [
            reply("claude-sonnet-5-5", false, 1_000),
            reply("claude-opus-5-5", false, 45_000),
            reply("claude-haiku-5-5", true, 9_000),
            reply("<synthetic>", false, 0),
            json!({"type": "user", "message": {"content": "hi"}}).to_string(),
        ];
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert_eq!(
            claude_usage(&path),
            Some(Usage {
                model: "claude-opus-5-5".into(),
                context: 45_152,
                window: None,
            })
        );
        assert_eq!(claude_usage(&dir.join("missing.jsonl")), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reports_carry_usage_with_or_without_a_window() {
        let tab = Uuid::new_v4();
        let line = |usage: &str| format!(r#"{{"tab":"{tab}","state":"done","usage":{usage}}}"#);
        let report =
            Report::parse(line(r#"{"model":"claude-sonnet-5-5","context":30710}"#).as_bytes());
        assert_eq!(
            report.and_then(|r| r.usage),
            Some(Usage {
                model: "claude-sonnet-5-5".into(),
                context: 30_710,
                window: None,
            })
        );
        let report = Report::parse(
            line(r#"{"model":"Claude Opus 5.5","context":41235,"window":200000}"#).as_bytes(),
        );
        assert_eq!(
            report.and_then(|r| r.usage).and_then(|u| u.window),
            Some(200_000)
        );
        assert!(Report::parse(line(r#"{"model":"a\u0000b","context":1}"#).as_bytes()).is_none());
    }

    #[test]
    fn names_models_and_counts_tokens() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(model_name("claude-3-5-haiku-20241022"), "Haiku 3.5");
        assert_eq!(model_name("gpt-5"), "gpt-5");
        assert_eq!(token_count(820), "820");
        assert_eq!(token_count(45_152), "45k");
        assert_eq!(token_count(1_234_567), "1.2M");
        let mut usage = Usage {
            model: "Claude Opus 5.5".into(),
            context: 45_152,
            window: None,
        };
        assert_eq!(usage.label(), "Claude Opus 5.5 · 45k context");
        usage.window = Some(200_000);
        assert_eq!(usage.label(), "Claude Opus 5.5 · 45k of 200k context (22%)");
        usage.model = "claude-opus-5-5".into();
        usage.window = None;
        assert_eq!(usage.label(), "Opus 5.5 · 45k of 1M context (4%)");
    }

    #[test]
    fn knows_context_windows_by_model_id() {
        assert_eq!(context_window("claude-opus-5-5"), Some(1_000_000));
        assert_eq!(context_window("anthropic/claude-haiku-4-5"), Some(200_000));
        assert_eq!(context_window("claude-sonnet-4-5-20250929"), Some(200_000));
        assert_eq!(context_window("claude-sonnet-4-5@20250929"), Some(200_000));
        assert_eq!(context_window("claude-sonnet-4-5[1m]"), Some(1_000_000));
        assert_eq!(context_window("glm-4.6"), Some(204_800));
        assert_eq!(context_window("my-local-model"), None);
        // Beyond the known window: a bigger one is on, size unknown.
        let usage = Usage {
            model: "claude-haiku-4-5".into(),
            context: 250_000,
            window: None,
        };
        assert_eq!(usage.window(), None);
    }

    fn agent(program: &str) -> Agent {
        let mut agent = Agent::default();
        agent.observe(Some(program), true, false, true, Instant::now());
        agent
    }

    #[test]
    fn guesses_working_and_alerts_when_it_stops_unseen() {
        let now = Instant::now();
        let mut a = agent("claude");
        assert_eq!(
            (a.state, a.unseen),
            (State::Idle, false),
            "no alert at start"
        );
        assert!(a.observe(Some("claude"), true, true, false, now));
        assert_eq!((a.state, a.unseen), (State::Working, false));
        assert!(a.observe(Some("claude"), true, false, false, now));
        assert_eq!((a.state, a.unseen), (State::Idle, true));
        assert!(
            !a.observe(Some("claude"), true, false, false, now),
            "no change"
        );
        assert!(a.seen());
        assert!(!a.unseen);

        // Stopping while you watch doesn't alert.
        a.observe(Some("claude"), true, true, true, now);
        a.observe(Some("claude"), true, false, true, now);
        assert_eq!((a.state, a.unseen), (State::Idle, false));
    }

    #[test]
    fn bells_ask_for_input_until_busy_again() {
        let now = Instant::now();
        let mut a = agent("opencode");
        assert!(a.attention("Allow bash?", false, now));
        assert_eq!(
            (a.state, a.unseen, a.message.as_str()),
            (State::Input, true, "Allow bash?")
        );
        a.observe(Some("opencode"), true, false, false, now);
        assert_eq!(a.state, State::Input, "quiet keeps it waiting");
        a.observe(Some("opencode"), true, true, false, now);
        assert_eq!(
            (a.state, a.unseen, a.message.as_str()),
            (State::Working, false, "")
        );
        // Plain terminals don't become agents by ringing.
        assert!(!Agent::default().attention("", false, now));
    }

    #[test]
    fn adds_up_time_worked_and_waited() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let mut a = agent("claude");
        a.report(State::Working, "", true, at(0));
        a.report(State::Input, "Allow?", true, at(60));
        assert_eq!(
            a.totals(at(90)),
            (Duration::from_secs(60), Duration::from_secs(30))
        );
        a.report(State::Working, "", true, at(100));
        a.report(State::Done, "", true, at(110));
        let totals = (Duration::from_secs(70), Duration::from_secs(40));
        assert_eq!(a.totals(at(500)), totals, "not while done");
    }

    #[test]
    fn reports_replace_guessing() {
        let now = Instant::now();
        let mut a = agent("claude");
        a.report(State::Working, "", false, now);
        a.observe(Some("claude"), true, false, false, now);
        assert_eq!(a.state, State::Working, "silence doesn't override a report");
        assert!(!a.attention("", false, now), "bells neither");
        a.report(State::Input, "Allow Bash?", false, now);
        assert_eq!((a.state, a.unseen), (State::Input, true));
        a.report(State::Working, "", false, now);
        a.report(State::Done, "", false, now);
        assert_eq!((a.state, a.unseen), (State::Done, true));
        // Any name works once the agent reports.
        let mut b = Agent::default();
        b.report(State::Working, "", false, now);
        b.observe(Some("node"), false, false, false, now);
        assert_eq!(
            (b.program.as_deref(), b.state),
            (Some("node"), State::Working)
        );
    }

    #[test]
    fn exits_alert_unseen_and_vanish_once_seen() {
        let now = Instant::now();
        let mut a = agent("claude");
        a.report(State::Working, "", false, now);
        assert!(a.observe(None, false, false, false, now));
        assert_eq!(
            (a.state, a.unseen, a.reported),
            (State::Exited, true, false)
        );
        assert!(a.seen());
        assert!(!a.is_agent());
        // Exiting in view just ends it.
        let mut b = agent("claude");
        b.observe(Some("bash"), false, false, true, now);
        assert!(!b.is_agent());
        // Restarting after an exit starts over.
        let mut c = agent("claude");
        c.observe(None, false, false, false, now);
        c.observe(Some("claude"), true, false, false, now);
        assert_eq!((c.state, c.unseen), (State::Idle, false));
    }

    #[test]
    fn agent_names() {
        let defaults = agent_list(None);
        assert!(is_known("claude", &defaults));
        assert!(is_known(".opencode", &defaults));
        assert!(is_known("claude.exe", &defaults));
        assert!(!is_known("nvim", &defaults));
        assert_eq!(
            agent_list(Some("claude, my-agent  x")),
            ["claude", "my-agent", "x"]
        );
        assert_eq!(agent_list(Some("  ")), defaults);
        let long = agent_list(Some("very-long-agent-name"));
        assert!(is_known("very-long-agent", &long), "process names are cut");
    }

    #[test]
    fn maps_claude_hook_events() {
        let event = |json: &str| claude_event(&serde_json::from_str(json).unwrap());
        assert_eq!(
            event(r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#),
            Some((State::Working, String::new()))
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#
            ),
            Some((
                State::Input,
                "Claude needs your permission to use Bash".into()
            ))
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"Notification","message":"Claude is waiting for your input"}"#
            ),
            None
        );
        assert_eq!(
            event(r#"{"hook_event_name":"Stop"}"#),
            Some((State::Done, String::new()))
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"Stop","last_assistant_message":"\n Fixed the login. \n"}"#
            ),
            Some((State::Done, "Fixed the login.".into()))
        );
        let long = json!({"hook_event_name": "Stop", "last_assistant_message": "x".repeat(5000)});
        assert_eq!(
            claude_event(&long).map(|(_, said)| said.len()),
            Some(MAX_REPLY_CHARS)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SessionStart"}"#),
            Some((State::Idle, String::new()))
        );
    }

    #[test]
    fn sessions_resume_by_id_or_continue_the_latest() {
        let session = |provider: &str, id: &str| SessionMetadata {
            provider: provider.into(),
            id: id.into(),
        };
        assert_eq!(
            session("claude", "abc-123").resume_command().unwrap(),
            ["claude", "--resume", "abc-123"]
        );
        assert_eq!(
            SessionMetadata::latest("codex")
                .unwrap()
                .resume_command()
                .unwrap(),
            ["codex", "resume", "--last"]
        );
        assert!(session("claude", "a; rm -rf ~").resume_command().is_err());
        assert!(session("claude", "--help").resume_command().is_err());
        assert!(SessionMetadata::latest("aider").is_none());
    }

    #[test]
    fn sockets_only_go_in_folders_others_cannot_write() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("soi-run-{}", Uuid::new_v4().simple()));
        fs::create_dir(&dir).unwrap();
        for (mode, ok) in [
            (0o700, true),
            (0o755, true),
            (0o775, false),
            (0o1777, false),
        ] {
            fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(private(&dir), ok, "mode {mode:o}");
        }
        assert!(!private(&dir.join("missing")));
        // Owned by root, not us.
        assert!(!private(Path::new("/")) || unsafe { libc::geteuid() } == 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reports_round_trip_through_a_socket() {
        // Short: socket paths may be at most 104 bytes on macOS, and its
        // `$TMPDIR` is long already.
        let dir = std::env::temp_dir().join(format!("soi-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let tab = Uuid::new_v4();
        let report = Report {
            tab,
            state: "input".into(),
            message: "Allow edit?".into(),
            session: None,
            usage: None,
        };
        send(&path, &report).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert_eq!(Report::parse(&bytes), Some(report));
        assert_eq!(Report::parse(br#"{"tab":"nope","state":"done"}"#), None);
        assert_eq!(
            Report::parse(format!(r#"{{"tab":"{tab}","state":"sleeping"}}"#).as_bytes()),
            None
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adds_claude_hooks_once_and_keeps_other_settings() {
        let settings = json!({
            "model": "opus",
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "notify-send hi" }] }] }
        });
        let updated = with_claude_hooks(&settings).unwrap().unwrap();
        assert_eq!(updated["model"], "opus");
        let stop = updated["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "the user's own hook stays");
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-send hi");
        assert_eq!(updated["hooks"]["PreToolUse"][0]["matcher"], "*");
        for event in CLAUDE_EVENTS {
            assert!(
                updated["hooks"][event]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(is_ours)
            );
        }
        assert_eq!(with_claude_hooks(&updated).unwrap(), None, "already there");
        assert!(with_claude_hooks(&json!([])).is_err());
    }

    #[test]
    fn replaces_rims_claude_hooks() {
        let rim = "[ -z \"$RIM_BIN\" ] || \"$RIM_BIN\" claude-hook # rim-agent-integration";
        let settings = json!({
            "hooks": { "Stop": [
                { "hooks": [{ "type": "command", "command": "notify-send hi" }] },
                { "hooks": [{ "type": "command", "command": rim }] }
            ] }
        });
        let updated = with_claude_hooks(&settings).unwrap().unwrap();
        let stop = updated["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-send hi");
        assert!(is_ours(&stop[1]));
        assert_eq!(with_claude_hooks(&updated).unwrap(), None);
    }

    #[test]
    fn the_opencode_plugin_is_marked_as_ours() {
        assert!(OPENCODE_PLUGIN.lines().next().unwrap().contains(MARKER));
        assert!(PI_EXTENSION.lines().next().unwrap().contains(MARKER));
        assert!(CLAUDE_COMMAND.contains(MARKER));
    }

    #[test]
    fn programs_are_found_only_where_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("soi-find-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("tool");
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        let path = format!("/nonexistent:{}", dir.display());
        assert_eq!(find_program("tool", &path), None, "not executable yet");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_program("tool", &path), Some(tool.clone()));
        assert_eq!(find_program("missing", &path), None);
        assert_eq!(find_program(tool.to_str().unwrap(), ""), Some(tool));
        fs::remove_dir_all(&dir).unwrap();
    }
}
