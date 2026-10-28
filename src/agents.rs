//! Coding agents running in terminals (Claude Code, OpenCode, …): what each
//! one is doing, and how agents tell soi about it.
//!
//! soi guesses an agent's state from its terminal (busy output, bells,
//! notifications). Agents with an integration report it exactly instead:
//! Claude Code through hooks that run `soi claude-hook`, OpenCode through a
//! plugin, Pi through an extension. Reports arrive as JSON lines on a Unix socket
//! (`$XDG_RUNTIME_DIR/soi.sock`), whose path each tab gets in
//! `SOI_SOCKET`, along with its own id in `SOI_TAB_ID`.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
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
const MAX_REPORT_BYTES: usize = 64 * 1024;

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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub provider: String,
    pub id: String,
}

impl SessionMetadata {
    /// The command that resumes the session. Arguments are passed directly to
    /// the PTY or typed into a shell; IDs are checked, so neither can run
    /// arbitrary commands. Unknown providers can't be resumed.
    pub fn resume_command(&self) -> Result<Vec<String>, String> {
        crate::tasks::validate_id(&self.id)?;
        let (program, flag) = match self.provider.as_str() {
            "claude" => ("claude", "--resume"),
            "codex" => ("codex", "resume"),
            "opencode" => ("opencode", "--session"),
            "pi" => ("pi", "--session"),
            _ => return Err("This provider has no supported resume command.".into()),
        };
        Ok(vec![program.into(), flag.into(), self.id.clone()])
    }
}

impl Report {
    pub fn parse(bytes: &[u8]) -> Option<(Uuid, State, String)> {
        let report: Report = serde_json::from_slice(bytes).ok()?;
        Some((report.tab, State::parse(&report.state)?, report.message))
    }
}

/// Where the running soi listens.
pub fn socket_path() -> Option<PathBuf> {
    let dir = crate::platform::current().runtime_dir()?;
    let name = if crate::dev_mode() {
        "soi-dev.sock"
    } else {
        "soi.sock"
    };
    Some(dir.join(name))
}

/// Reads one report from a connection: everything up to the end, or a
/// newline. Gives up on overlong input.
pub fn read_report(mut stream: impl Read) -> Option<(Uuid, State, String)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.contains(&b'\n') {
            break;
        }
        if data.len() > MAX_REPORT_BYTES {
            return None;
        }
    }
    let line = data.split(|&b| b == b'\n').next()?;
    Report::parse(line)
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
    report_session_from_env(state, message, None)
}

fn report_session_from_env(state: State, message: &str, session: Option<SessionMetadata>) -> i32 {
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
            report_session_from_env(state, &message, session)
        }
        None => 0,
    }
}

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
        "Stop" => Some((State::Done, String::new())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Installing the integrations

/// The command Claude Code's hooks run. Does nothing outside soi.
pub const CLAUDE_COMMAND: &str =
    "[ -z \"$SOI_BIN\" ] || \"$SOI_BIN\" claude-hook # soi-agent-integration";

const CLAUDE_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
];

fn home() -> PathBuf {
    crate::session::home_dir()
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
pub const OPENCODE_PLUGIN: &str = r#"// soi-agent-integration: written by soi (Settings ▸ Agents).
// Tells soi what OpenCode is doing, for its agents panel, and which session
// it is, so soi can link it to the tab's task. Outside soi it does nothing.
// soi only replaces this file while this first line is here.
import { connect } from "node:net"

// What to do with each event, or nothing outside soi.
const reporter = () => {
  const socket = process.env.SOI_SOCKET
  const tab = process.env.SOI_TAB_ID
  if (!socket || !tab) return undefined

  // The session the user works in; subagents' sessions have a parent.
  let main = ""
  const children = new Set()
  const notice = (info) => {
    if (!info?.id) return
    if (info.parentID) children.add(info.id)
    else main = info.id
  }
  const mainOf = (id) => {
    if (!main && id && !children.has(id)) main = id
  }

  let last = ""
  const send = (state, message = "") => {
    const report = { tab, state, message }
    if (main) report.session = { provider: "opencode", id: main }
    const line = JSON.stringify(report) + "\n"
    if (line === last) return
    // Set before it's delivered, so the same report right after (1.x says
    // idle twice) isn't sent again; one soi missed is.
    last = line
    const missed = () => {
      if (last === line) last = ""
    }
    try {
      const client = connect(socket, () => client.end(line))
      client.on("error", missed)
    } catch {
      missed()
    }
  }

  // Busy sessions (subagents have their own) and open questions.
  const busy = new Set()
  let asking = 0
  const settle = () => {
    if (asking > 0) return
    if (busy.size > 0) send("working")
    else send("done")
  }

  const asked = (message) => {
    asking++
    send("input", message)
  }
  const answered = () => {
    asking = Math.max(0, asking - 1)
    settle()
  }

  return (type, p) => {
    switch (type) {
      case "session.created":
      case "session.updated":
        // 1.x has the session as `info`; 2.x has its fields.
        notice(p.info ?? { id: p.sessionID, parentID: p.parentID })
        break
      // 2.x: one run of the agent.
      case "session.execution.started":
        mainOf(p.sessionID)
        busy.add(p.sessionID)
        settle()
        break
      case "session.execution.succeeded":
      case "session.execution.interrupted":
        busy.delete(p.sessionID)
        settle()
        break
      case "session.execution.failed": {
        busy.delete(p.sessionID)
        const error = p.error
        send("input", error?.data?.message ?? error?.message ?? error?.name ?? "Error")
        break
      }
      case "form.created": {
        const form = p.form ?? {}
        asked(form.title ?? form.fields?.[0]?.title ?? "Has a question")
        break
      }
      case "form.replied":
      case "form.cancelled":
        answered()
        break
      // 1.x
      case "session.status": {
        mainOf(p.sessionID)
        const status = p.status?.type
        if (status === "busy" || status === "retry") busy.add(p.sessionID)
        else if (status === "idle") busy.delete(p.sessionID)
        settle()
        break
      }
      case "session.idle":
        busy.delete(p.sessionID)
        settle()
        break
      // Both: 2.x has `message` and `action` where 1.x has `title` and
      // `permission`.
      case "permission.updated":
      case "permission.asked": {
        const action = p.permission ?? p.action
        asked(p.title ?? p.message ?? (action ? `Allow ${action}?` : "Permission needed"))
        break
      }
      case "question.asked":
        asked(p.questions?.[0]?.question ?? "Has a question")
        break
      case "permission.replied":
      case "question.replied":
      case "question.rejected":
        answered()
        break
      case "session.error": {
        const error = p.error
        send("input", error?.data?.message ?? error?.message ?? error?.name ?? "Error")
        break
      }
    }
  }
}

// Late 1.x versions call both `server` and `setup`: only the first reports.
let started = false
const start = () => {
  if (started) return undefined
  const report = reporter()
  started = report !== undefined
  return report
}

// OpenCode 1.x
const server = async () => {
  const report = start()
  if (!report) return {}
  return { event: async ({ event }) => report(event.type, event.properties ?? {}) }
}

// OpenCode 2.x
const setup = (opencode) => {
  const report = start()
  if (!report) return undefined
  const stop = new AbortController()
  ;(async () => {
    for await (const event of opencode.event.subscribe({ signal: stop.signal })) {
      report(event.type, event.data ?? {})
    }
  })().catch(() => {})
  // Unloaded, as 2.x does to reload it: the next `setup` reports.
  return () => {
    stop.abort()
    started = false
  }
}

export default { id: "soi", server, setup }
"#;

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
            "{} exists and wasn't written by soi; left it unchanged",
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
pub const PI_EXTENSION: &str = r#"// soi-agent-integration: written by soi (Settings ▸ Agents).
// Tells soi what Pi is doing, for its agents panel, and which session it is,
// so soi can link it to the tab's task. Outside soi it does nothing.
// soi only replaces this file while this first line is here.
import { connect } from "node:net"

export default function (pi) {
  const socket = process.env.SOI_SOCKET
  const tab = process.env.SOI_TAB_ID
  if (!socket || !tab) return

  // Ephemeral sessions (--no-session) can't be resumed, so they aren't linked.
  let session = ""
  const notice = (ctx) => {
    const manager = ctx?.sessionManager
    if (manager?.getSessionFile?.()) session = manager.getSessionId?.() ?? ""
  }

  let last = ""
  const send = (state, message = "") => {
    const report = { tab, state, message }
    if (session) report.session = { provider: "pi", id: session }
    const line = JSON.stringify(report) + "\n"
    if (line === last) return
    try {
      // Only a delivered report counts: one soi missed is sent again.
      const client = connect(socket, () => {
        last = line
        client.end(line)
      })
      client.on("error", () => {})
    } catch {}
  }

  // A run lasts from agent_start until agent_settled: retries, compaction and
  // queued messages can start more after agent_end.
  let running = false
  let asking = 0
  let error = ""
  const settle = () => {
    if (asking > 0) return
    if (running) send("working")
    else if (error) send("input", error)
    else send("done")
  }

  pi.on("session_start", async (_event, ctx) => {
    notice(ctx)
  })
  pi.on("agent_start", async (_event, ctx) => {
    notice(ctx)
    running = true
    error = ""
    settle()
  })
  pi.on("tool_execution_start", async () => {
    settle()
  })
  pi.on("agent_end", async (event) => {
    const reply = [...(event.messages ?? [])].reverse().find((m) => m.role === "assistant")
    error = reply?.stopReason === "error" ? reply.errorMessage || "Error" : ""
  })
  pi.on("agent_settled", async (event, ctx) => {
    notice(ctx)
    running = false
    if (event.aborted) error = ""
    settle()
  })
  pi.on("ui_prompt_start", async (event) => {
    asking++
    send("input", event.title || "Waiting for your answer")
  })
  pi.on("ui_prompt_end", async () => {
    asking = Math.max(0, asking - 1)
    settle()
  })
}
"#;

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
            "{} exists and wasn't written by soi; left it unchanged",
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
    use std::os::fd::AsRawFd;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let result = (|| {
        let mut stdout = child.stdout.take()?;
        let fd = stdout.as_raw_fd();
        // SAFETY: stdout owns this live pipe; preserve its other descriptor flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        let started = Instant::now();
        let mut output = Vec::new();
        let mut found = None;
        let mut chunk = [0; 8192];
        loop {
            let status = child.try_wait().ok()?;
            // Drain while the child runs: a chatty .zshrc can fill the pipe.
            loop {
                if started.elapsed() >= timeout {
                    return None;
                }
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(len) => {
                        output.extend_from_slice(&chunk[..len]);
                        if output.len() > MAX_SHELL_OUTPUT {
                            output.drain(..output.len() - MAX_SHELL_OUTPUT);
                        }
                        if framed && let Some(path) = framed_path(&output) {
                            found = Some(path);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => return None,
                }
            }
            if let Some(status) = status {
                if !status.success() {
                    return None;
                }
                return if framed {
                    found
                } else {
                    // Keep the existing login-shell behavior for bash and fish.
                    std::str::from_utf8(&output)
                        .ok()?
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty())
                        .map(String::from)
                };
            }
            if started.elapsed() < timeout {
                std::thread::sleep(Duration::from_millis(20))
            }
        }
    })();
    // Also reap unsuccessful/timed-out probes; never wait for pipe EOF from a
    // background process a startup script may have left behind.
    let _ = child.kill();
    let _ = child.wait();
    result
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
    let fail = |err: io::Error| format!("cannot write {}: {err}", path.display());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    let tmp = path.with_extension("soi-tmp");
    fs::write(&tmp, text).map_err(fail)?;
    fs::rename(&tmp, path).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some((tab, State::Done, String::new()))
        );
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
        assert_eq!(event(r#"{"hook_event_name":"SessionStart"}"#), None);
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
        };
        send(&path, &report).unwrap();
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(
            read_report(stream),
            Some((tab, State::Input, "Allow edit?".into()))
        );
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
