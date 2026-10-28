//! Local task context and provider-independent session bookmarks. No transcripts
//! or credentials are copied from an agent's private storage.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Active,
    Blocked,
    Review,
    Finished,
}

impl TaskStatus {
    pub const ALL: [Self; 4] = [Self::Active, Self::Blocked, Self::Review, Self::Finished];
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Blocked => "Blocked",
            Self::Review => "Ready for review",
            Self::Finished => "Finished",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskContext {
    pub id: Uuid,
    pub title: String,
    #[serde(default)]
    pub status: TaskStatus,
    pub repository: PathBuf,
    pub branch: Option<String>,
    pub plan: Option<PathBuf>,
    #[serde(default)]
    pub important_files: Vec<PathBuf>,
    /// Ticket fields live here so a task needs no file; the plan is the
    /// long-form Markdown agents read. Defaults keep older sessions loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub acceptance: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<ChecklistItem>,
    /// The ticket's number, e.g. `PROJ-123`; it names the task's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// When the task was created (Unix seconds; 0 for older tasks).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub created: u64,
    /// The task's folder among soi's data (see `task_folder`): its
    /// `metadata.md`, and the plan unless another was chosen. It outlives
    /// the worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

/// One item per line; Markdown task markers (`- [x] Done`) are understood.
pub fn parse_checklist(text: &str) -> Vec<ChecklistItem> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line);
            let (done, line) = if let Some(rest) = line
                .strip_prefix("[x]")
                .or_else(|| line.strip_prefix("[X]"))
            {
                (true, rest)
            } else {
                (false, line.strip_prefix("[ ]").unwrap_or(line))
            };
            let text = line.trim();
            (!text.is_empty()).then(|| ChecklistItem {
                text: text.into(),
                done,
            })
        })
        .collect()
}

/// A ticket number names a folder, so only letters, digits, `-`, `_` and `.`.
pub fn validate_ticket(text: &str) -> Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > 40
        || text.starts_with(['.', '-'])
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(
            "Ticket numbers use letters, digits, - _ and . (up to 40), like PROJ-123.".into(),
        );
    }
    Ok(Some(text.into()))
}

/// Where soi keeps task plans and metadata, one folder per project, next to
/// its layouts and sessions.
pub fn plans_home() -> PathBuf {
    crate::dirs::state_home().join("soi/plans")
}

/// The folder of the project whose main checkout is `repository`: in
/// `plans`, named after the checkout. A `.repository` file there says whose
/// it is, so another project of the same name gets `<name>-<hash>` instead.
pub fn project_folder_in(plans: &Path, repository: &Path) -> PathBuf {
    let name = repository
        .file_name()
        .map_or_else(|| "repository".into(), |n| n.to_string_lossy().into_owned());
    let owner = repository.to_string_lossy();
    // FNV-1a: the same path gives the same name with every Rust version.
    let hash = owner.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    let unique = plans.join(format!("{name}-{:06x}", hash & 0xffffff));
    for folder in [plans.join(&name), unique.clone()] {
        let marker = folder.join(".repository");
        match fs::read_to_string(&marker) {
            Ok(text) if text.trim_end() == owner => return folder,
            Ok(_) => continue,
            // Unclaimed: claim it. If that fails, the hashed name is safe.
            Err(_) => {
                let claimed = fs::create_dir_all(&folder)
                    .and_then(|()| fs::write(&marker, format!("{owner}\n")));
                if claimed.is_ok() {
                    return folder;
                }
            }
        }
    }
    unique
}

/// See `project_folder_in`.
pub fn project_folder(repository: &Path) -> PathBuf {
    project_folder_in(&plans_home(), repository)
}

/// Letters and digits of `text`, the rest as single dashes: `Fix / Login!`
/// → `fix-login`.
pub fn slug(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
}

/// Only web links are kept as issue references; nothing is fetched.
pub fn validate_issue(text: &str) -> Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if !(text.starts_with("https://") || text.starts_with("http://"))
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("Issue links must be http(s) URLs without spaces.".into());
    }
    Ok(Some(text.into()))
}

impl TaskContext {
    pub fn new(title: String, repository: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            title,
            repository,
            status: TaskStatus::Active,
            branch: None,
            plan: None,
            important_files: Vec::new(),
            issue: None,
            description: String::new(),
            acceptance: String::new(),
            checklist: Vec::new(),
            ticket: None,
            created: now(),
            folder: None,
        }
    }

    /// The task's folder in its project's (see `project_folder`):
    /// `<ticket or short id>-<title>`. The same ticket and title find the
    /// same folder again.
    pub fn task_folder(&self, project: &Path) -> PathBuf {
        let prefix = self
            .ticket
            .clone()
            .unwrap_or_else(|| self.id.simple().to_string()[..6].to_string());
        let name = match slug(&self.title) {
            title if title.is_empty() => prefix,
            title => format!("{prefix}-{title}"),
        };
        project.join(name)
    }

    /// The task's folder; for tasks from before there were folders, the one
    /// it gets when next saved (see `adopt_folder`).
    pub fn folder_or_default(&self) -> PathBuf {
        self.folder.clone().unwrap_or_else(|| {
            let main = crate::git::main_repository(&self.repository)
                .unwrap_or_else(|_| self.repository.clone());
            self.task_folder(&project_folder(&main))
        })
    }

    /// Gives a task from before there were folders its folder.
    pub fn adopt_folder(&mut self) {
        if self.folder.is_none() {
            self.folder = Some(self.folder_or_default());
        }
    }

    pub fn metadata_path(&self) -> Option<PathBuf> {
        Some(self.folder.as_ref()?.join("metadata.md"))
    }

    /// What changed from `old` to `self`, as history entries.
    pub fn changes_since(&self, old: &TaskContext) -> Vec<String> {
        let mut changes = Vec::new();
        if self.title != old.title {
            changes.push(format!("Renamed from “{}” to “{}”", old.title, self.title));
        }
        if self.status != old.status {
            changes.push(format!(
                "Status: {} → {}",
                old.status.label(),
                self.status.label()
            ));
        }
        let or_none = |value: &Option<String>| value.clone().unwrap_or_else(|| "none".into());
        if self.ticket != old.ticket {
            changes.push(format!(
                "Ticket: {} → {}",
                or_none(&old.ticket),
                or_none(&self.ticket)
            ));
        }
        if self.issue != old.issue {
            changes.push(format!(
                "Issue: {} → {}",
                or_none(&old.issue),
                or_none(&self.issue)
            ));
        }
        if self.branch != old.branch {
            changes.push(format!(
                "Branch: {} → {}",
                or_none(&old.branch),
                or_none(&self.branch)
            ));
        }
        if self.plan != old.plan {
            changes.push(match &self.plan {
                Some(plan) => format!("Plan linked: {}", plan.display()),
                None => "Plan unlinked".into(),
            });
        }
        for (name, new, old) in [
            ("Description", &self.description, &old.description),
            ("Acceptance criteria", &self.acceptance, &old.acceptance),
        ] {
            if new != old {
                changes.push(format!("{name} edited"));
            }
        }
        let texts = |task: &TaskContext| {
            task.checklist
                .iter()
                .map(|item| item.text.clone())
                .collect::<Vec<_>>()
        };
        if texts(self) == texts(old) {
            for (new, old) in self.checklist.iter().zip(&old.checklist) {
                if new.done != old.done {
                    let verb = if new.done { "Checked" } else { "Unchecked" };
                    changes.push(format!("{verb}: {}", new.text));
                }
            }
        } else {
            let done = self.checklist.iter().filter(|item| item.done).count();
            changes.push(format!(
                "Checklist edited ({done} of {} done)",
                self.checklist.len()
            ));
        }
        if self.important_files != old.important_files {
            changes.push("Important files edited".into());
        }
        changes
    }

    /// `metadata.md`: what the task is now, which soi rewrites, then its
    /// history, which it only adds to (from `existing`, the file as it was).
    pub fn metadata(&self, existing: Option<&str>, events: &[String], at: u64) -> String {
        let mut out = format!(
            "# {}

",
            self.title
        );
        out.push_str(&format!(
            "- ID: {}
",
            self.id
        ));
        if let Some(ticket) = &self.ticket {
            out.push_str(&format!(
                "- Ticket: {ticket}
"
            ));
        }
        if let Some(issue) = &self.issue {
            out.push_str(&format!(
                "- Issue: {issue}
"
            ));
        }
        out.push_str(&format!(
            "- Status: {}
",
            self.status.label()
        ));
        out.push_str(&format!(
            "- Repository: {}
",
            self.repository.display()
        ));
        if let Some(branch) = &self.branch {
            out.push_str(&format!(
                "- Branch: {branch}
"
            ));
        }
        if let Some(plan) = &self.plan {
            out.push_str(&format!(
                "- Plan: {}
",
                plan.display()
            ));
        }
        if self.created > 0 {
            out.push_str(&format!(
                "- Created: {}
",
                local_time(self.created)
            ));
        }
        out.push_str("\n## History\n\n");
        let history = existing
            .and_then(|text| text.split_once("\n## History\n"))
            .map(|(_, history)| history.trim())
            .unwrap_or("");
        if !history.is_empty() {
            out.push_str(history);
            out.push('\n');
        }
        for event in events {
            out.push_str(&format!("- {} — {event}\n", local_time(at)));
        }
        out
    }

    /// Adds `events` to the task's `metadata.md`, and brings its header up
    /// to date. Tasks without a folder keep none.
    pub fn record(&self, events: &[String]) -> Result<(), String> {
        let Some(path) = self.metadata_path() else {
            return Ok(());
        };
        let existing = fs::read_to_string(&path).ok();
        let text = self.metadata(existing.as_deref(), events, now());
        if existing.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        crate::plan::save(&path, &text, existing.as_deref()).map_err(|e| match e {
            crate::plan::SaveError::Conflict(_) => {
                format!("{} changed while soi wrote it", path.display())
            }
            crate::plan::SaveError::Failed(e) => e,
        })
    }

    fn ticket(&self, out: &mut String) {
        if let Some(issue) = &self.issue {
            out.push_str(&format!("Issue: {issue}\n\n"));
        }
        for (heading, text) in [
            ("Description", &self.description),
            ("Acceptance criteria", &self.acceptance),
        ] {
            if !text.trim().is_empty() {
                out.push_str(&format!("## {heading}\n\n{}\n\n", text.trim()));
            }
        }
        if !self.checklist.is_empty() {
            out.push_str("## Checklist\n\n");
            for item in &self.checklist {
                let mark = if item.done { 'x' } else { ' ' };
                out.push_str(&format!("- [{mark}] {}\n", item.text));
            }
            out.push('\n');
        }
    }

    /// Starting content for a new plan file, seeded from the ticket.
    pub fn plan_template(&self) -> String {
        let mut out = format!("# {}\n\n", self.title);
        self.ticket(&mut out);
        out.push_str("## Plan\n\n- \n");
        out
    }

    /// What the user explicitly hands to an agent. Plain Markdown, so it can be
    /// pasted anywhere or passed as an initial prompt argument. `worktree` is
    /// the folder the agent works in (the task workspace's).
    pub fn brief(&self, worktree: &Path) -> String {
        let mut out = format!("# Task: {}\n\n", self.title);
        out.push_str(&format!("- ID: {}\n", self.id));
        if let Some(ticket) = &self.ticket {
            out.push_str(&format!("- Ticket: {ticket}\n"));
        }
        out.push_str(&format!("- Status: {}\n", self.status.label()));
        if self.created > 0 {
            out.push_str(&format!("- Created: {}\n", local_time(self.created)));
        }
        out.push_str("\n## Workspace\n\n");
        out.push_str(&format!("- Worktree: {}\n", worktree.display()));
        if let Some(branch) = &self.branch {
            out.push_str(&format!("- Branch: {branch}\n"));
        }
        out.push_str(&format!("- Repository: {}\n", self.repository.display()));
        let same = worktree.canonicalize().ok() == self.repository.canonicalize().ok();
        if !same {
            out.push_str(
                "\nWork and commit only in the worktree, on its branch; leave the repository's own checkout alone.\n",
            );
        }
        out.push('\n');
        self.ticket(&mut out);
        if let Some(plan) = &self.plan {
            out.push_str(&format!(
                "## Plan\n\nRead and keep the plan up to date: {}\n\n",
                plan.display()
            ));
        }
        if !self.important_files.is_empty() {
            out.push_str("## Important files\n\n");
            for file in &self.important_files {
                out.push_str(&format!("- {}\n", file.display()));
            }
            out.push('\n');
        }
        if let (Some(metadata), Some(others)) = (
            self.metadata_path(),
            self.folder.as_ref().and_then(|f| f.parent()),
        ) {
            out.push_str(&format!(
                "## Notes\n\nThis task's history: {}\nEarlier tasks in this repository, with their plans and history: {}\n",
                metadata.display(),
                others.display()
            ));
        }
        out.trim_end().to_string() + "\n"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    pub provider: String,
    pub session_id: String,
    pub title: String,
    pub directory: PathBuf,
    pub task_id: Option<Uuid>,
    /// The workspace it ran in, to resume it there while it still exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,
    pub updated: u64,
}

impl AgentSession {
    pub fn matches(&self, query: &str) -> bool {
        let text = format!(
            "{} {} {} {}",
            self.title,
            self.provider,
            self.session_id,
            self.directory.display()
        )
        .to_lowercase();
        query
            .to_lowercase()
            .split_whitespace()
            .all(|word| text.contains(word))
    }

    /// Arguments are passed directly to the PTY, never to a shell. Unknown
    /// providers remain useful bookmarks, but cannot execute arbitrary commands.
    pub fn resume_command(&self) -> Result<Vec<String>, String> {
        validate_id(&self.session_id)?;
        match self.provider.as_str() {
            "claude" => Ok(vec![
                "claude".into(),
                "--resume".into(),
                self.session_id.clone(),
            ]),
            "codex" => Ok(vec![
                "codex".into(),
                "resume".into(),
                self.session_id.clone(),
            ]),
            "opencode" => Ok(vec![
                "opencode".into(),
                "--session".into(),
                self.session_id.clone(),
            ]),
            _ => Err("This provider has no supported resume command.".into()),
        }
    }
}

pub fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 256
        || id.starts_with('-')
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(
            "Session IDs must contain only letters, digits, underscores or hyphens.".into(),
        );
    }
    Ok(())
}

/// `2026-10-27 14:03`, in local time.
fn local_time(secs: u64) -> String {
    gtk::glib::DateTime::from_unix_local(secs as i64)
        .and_then(|time| time.format("%Y-%m-%d %H:%M"))
        .map_or_else(|_| secs.to_string(), |text| text.to_string())
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn history_path() -> PathBuf {
    crate::dirs::state_home()
        .join("soi")
        .join(if crate::dev_mode() {
            "agent-sessions-dev.json"
        } else {
            "agent-sessions.json"
        })
}

/// Serializes read/merge/write across hook reports and manual bookmarks.
pub fn remember(session: AgentSession) -> Result<(), String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().map_err(|_| "Session history lock failed")?;
    validate_id(&session.session_id)?;
    let path = history_path();
    let mut history = History::load(&path).map_err(|e| e.to_string())?;
    if history.sessions.iter().any(|old| {
        old.provider == session.provider
            && old.session_id == session.session_id
            && old.directory == session.directory
            && old.title == session.title
            && old.task_id == session.task_id
            && old.workspace_id == session.workspace_id
            && session.updated.saturating_sub(old.updated) < 60
    }) {
        return Ok(());
    }
    history.upsert(session);
    history.save(&path).map_err(|e| e.to_string())
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub sessions: Vec<AgentSession>,
}

impl History {
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn upsert(&mut self, session: AgentSession) {
        self.sessions.retain(|old| {
            old.provider != session.provider
                || old.session_id != session.session_id
                || old.directory != session.directory
        });
        self.sessions.push(session);
        self.sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> AgentSession {
        AgentSession {
            provider: "claude".into(),
            session_id: "abc-123".into(),
            title: "Fix login".into(),
            directory: "/project".into(),
            task_id: None,
            workspace_id: None,
            updated: 1,
        }
    }
    #[test]
    fn ticket_numbers_name_folders_safely() {
        assert_eq!(validate_ticket("  PROJ-123 "), Ok(Some("PROJ-123".into())));
        assert_eq!(validate_ticket(""), Ok(None));
        for bad in ["../x", "a/b", "-x", ".x", "a b", "ä", &"x".repeat(41)] {
            assert!(validate_ticket(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn task_folders_go_by_ticket_or_short_id() {
        let mut task = TaskContext::new("Fix / Login!".into(), "/src/soi".into());
        let project = Path::new("/plans/soi");
        let id = task.id.simple().to_string()[..6].to_string();
        assert_eq!(
            task.task_folder(project),
            project.join(format!("{id}-fix-login"))
        );
        task.ticket = Some("PROJ-7".into());
        assert_eq!(task.task_folder(project), project.join("PROJ-7-fix-login"));
        task.title = "!!".into();
        assert_eq!(task.task_folder(project), project.join("PROJ-7"));
    }

    #[test]
    fn projects_of_the_same_name_get_their_own_folders() {
        let plans = std::env::temp_dir().join(format!("soi-plans-{}", Uuid::new_v4()));
        let mine = project_folder_in(&plans, Path::new("/src/api"));
        assert_eq!(mine, plans.join("api"));
        assert_eq!(
            project_folder_in(&plans, Path::new("/src/api")),
            mine,
            "found again"
        );
        let theirs = project_folder_in(&plans, Path::new("/work/api"));
        assert_ne!(theirs, mine);
        assert!(
            theirs
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("api-")
        );
        assert_eq!(project_folder_in(&plans, Path::new("/work/api")), theirs);
        fs::remove_dir_all(plans).unwrap();
    }

    #[test]
    fn metadata_keeps_its_history_and_adds_to_it() {
        let mut task = TaskContext::new("Login".into(), "/src/soi".into());
        task.ticket = Some("PROJ-7".into());
        task.branch = Some("login".into());
        let first = task.metadata(None, &["Created".into()], 0);
        assert!(first.starts_with("# Login\n\n"));
        assert!(first.contains("- Ticket: PROJ-7\n- Status: Active\n"));
        assert!(first.contains("- Branch: login\n"));
        let old = task.clone();
        task.status = TaskStatus::Review;
        let changes = task.changes_since(&old);
        assert_eq!(changes, ["Status: Active → Ready for review"]);
        let second = task.metadata(Some(&first), &changes, 0);
        assert!(second.contains("- Status: Ready for review\n"));
        let history = second.split_once("## History\n\n").unwrap().1;
        let lines: Vec<_> = history.lines().collect();
        assert_eq!(lines.len(), 2, "{history}");
        assert!(lines[0].ends_with(" — Created"));
        assert!(lines[1].ends_with(" — Status: Active → Ready for review"));
    }

    #[test]
    fn changes_name_what_was_ticked() {
        let mut task = TaskContext::new("x".into(), "/r".into());
        task.checklist = parse_checklist("- [ ] one\n- [ ] two");
        let old = task.clone();
        task.checklist[1].done = true;
        assert_eq!(task.changes_since(&old), ["Checked: two"]);
        task.checklist.push(ChecklistItem {
            text: "three".into(),
            done: false,
        });
        assert_eq!(task.changes_since(&old), ["Checklist edited (1 of 3 done)"]);
        assert!(task.changes_since(&task).is_empty());
    }

    #[test]
    fn safe_resume_and_search() {
        let mut s = record();
        assert_eq!(
            s.resume_command().unwrap(),
            ["claude", "--resume", "abc-123"]
        );
        assert!(s.matches("LOGIN project"));
        assert!(!s.matches("missing"));
        s.provider = "opencode".into();
        assert_eq!(
            s.resume_command().unwrap(),
            ["opencode", "--session", "abc-123"]
        );
        s.provider = "codex".into();
        assert_eq!(s.resume_command().unwrap(), ["codex", "resume", "abc-123"]);
        for id in ["", "--help", "a;b", "$(whoami)", "a\nb", "../x"] {
            s.session_id = id.into();
            assert!(s.resume_command().is_err());
        }
        s.session_id = "safe".into();
        s.provider = "arbitrary".into();
        assert!(s.resume_command().is_err());
    }
    #[test]
    fn checklist_issue_and_brief() {
        let items = parse_checklist("- [x] Done\n\n[ ] Open\n* plain\n- [ ]  \n");
        assert_eq!(
            items,
            [
                ChecklistItem {
                    text: "Done".into(),
                    done: true
                },
                ChecklistItem {
                    text: "Open".into(),
                    done: false
                },
                ChecklistItem {
                    text: "plain".into(),
                    done: false
                },
            ]
        );
        assert_eq!(validate_issue(" ").unwrap(), None);
        assert!(validate_issue("javascript:alert(1)").is_err());
        assert!(validate_issue("https://x/1 2").is_err());
        let mut task = TaskContext::new("Fix login".into(), "/repo".into());
        task.issue = validate_issue("https://example.com/issues/1").unwrap();
        task.description = "Users get logged out.".into();
        task.checklist = items;
        task.plan = Some("/repo/.worktrees/fix/.soi/plan.md".into());
        task.branch = Some("fix".into());
        let brief = task.brief(Path::new("/repo/.worktrees/fix/repo"));
        assert!(brief.starts_with("# Task: Fix login\n"));
        assert!(brief.contains("Issue: https://example.com/issues/1"));
        assert!(brief.contains("- [x] Done\n- [ ] Open"));
        assert!(brief.contains("/repo/.worktrees/fix/.soi/plan.md"));
        assert!(brief.contains("- Worktree: /repo/.worktrees/fix/repo\n- Branch: fix\n- Repository: /repo\n"));
        let template = task.plan_template();
        assert!(template.starts_with("# Fix login\n"));
        assert!(template.contains("## Description\n\nUsers get logged out."));
        assert!(!template.contains("plan.md"));
    }

    #[test]
    fn history_round_trip_and_update() {
        let dir = std::env::temp_dir().join(format!("soi-history-{}", Uuid::new_v4()));
        let path = dir.join("history.json");
        let mut h = History::load(&path).unwrap();
        h.upsert(record());
        let mut s = record();
        s.title = "Updated".into();
        h.upsert(s);
        h.save(&path).unwrap();
        let loaded = History::load(&path).unwrap();
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].title, "Updated");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(&path, "not valid JSON").unwrap();
        assert!(History::load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "not valid JSON");
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
