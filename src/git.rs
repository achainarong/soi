//! Git operations used by task workspaces. Call on a worker, not the GTK thread.
//! External diff drivers, pagers, hooks and fsmonitor commands are never run.
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const LIMIT: u64 = 2 * 1024 * 1024;

fn run(directory: &Path, args: &[&str]) -> Result<String, String> {
    let output = output(directory, args)?;
    if !output.status.success() {
        let errors = String::from_utf8_lossy(&output.stderr);
        if errors.contains("not a git repository") {
            return Err("Not a Git repository.".into());
        }
        return Err(format!("Git failed: {}", errors.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn output(directory: &Path, args: &[&str]) -> Result<Output, String> {
    let mut child = Command::new("git")
        .args([
            "--no-pager",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "color.ui=false",
        ])
        .args(args)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start Git: {e}"))?;
    let stdout = child.stdout.take().ok_or("No Git output stream")?;
    let stderr = child.stderr.take().ok_or("No Git error stream")?;
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(e) => format!("Cannot wait for Git: {e}"),
                    _ => "Git timed out. Inspect the repository before retrying.".into(),
                });
            }
        }
    };
    let bytes = out
        .join()
        .map_err(|_| "Git output reader failed")?
        .map_err(|e| e.to_string())?;
    let errors = err
        .join()
        .map_err(|_| "Git error reader failed")?
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT || errors.len() as u64 > LIMIT {
        return Err(
            "Git output exceeds the 2 MiB limit. Use your Git client for this repository.".into(),
        );
    }
    Ok(Output {
        status,
        stdout: bytes,
        stderr: errors,
    })
}

pub fn repository(directory: &Path) -> Result<PathBuf, String> {
    let root = run(directory, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(root.trim_end_matches('\n'))
        .map_err(|e| format!("Cannot resolve repository directory: {e}"))
}

/// The repository's main checkout, also from inside one of its worktrees:
/// where the shared `.git` lives. Falls back to `directory`'s own top level.
pub fn main_repository(directory: &Path) -> Result<PathBuf, String> {
    let common = run(
        directory,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = Path::new(common.trim_end_matches('\n'));
    match common.parent() {
        Some(parent) if common.file_name().is_some_and(|name| name == ".git") => {
            std::fs::canonicalize(parent)
                .map_err(|e| format!("Cannot resolve repository directory: {e}"))
        }
        _ => repository(directory),
    }
}

/// Relative destinations always start at the repository root, never soi's
/// process cwd. Empty input uses `.worktrees/<branch>/<repository name>`, with
/// the branch made directory-safe (`feature/login` → `feature-login`).
pub fn worktree_destination(
    repository: &Path,
    branch: &str,
    destination: &Path,
) -> Result<PathBuf, String> {
    if !repository.is_absolute() {
        return Err("The repository root must be absolute.".into());
    }
    let destination = if destination.as_os_str().is_empty() {
        let name = branch
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        if name.is_empty() {
            return Err(
                "Use a branch name containing letters or digits, or enter a worktree directory."
                    .into(),
            );
        }
        let Some(repository_name) = repository.file_name() else {
            return Err("Enter a worktree directory.".into());
        };
        repository
            .join(".worktrees")
            .join(name)
            .join(repository_name)
    } else if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        repository.join(destination)
    };
    // Resolve existing symlink ancestors before reducing `..`. Otherwise a
    // destination such as `linked/../task` could land in the wrong directory.
    for ancestor in destination.ancestors() {
        if let Ok(mut resolved) = std::fs::canonicalize(ancestor) {
            let suffix = destination
                .strip_prefix(ancestor)
                .map_err(|e| e.to_string())?;
            for component in suffix.components() {
                match component {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        resolved.pop();
                    }
                    other => resolved.push(other.as_os_str()),
                }
            }
            return Ok(resolved);
        }
    }
    Err("Cannot resolve the worktree destination. Check directory permissions.".into())
}

/// An in-repository worktree should not accidentally appear in `git add .`.
/// Checking ignore rules never changes the user's .gitignore or Git index.
pub fn worktree_ignore_warning(
    repository: &Path,
    destination: &Path,
) -> Result<Option<String>, String> {
    let Ok(relative) = destination.strip_prefix(repository) else {
        return Ok(None);
    };
    if relative.as_os_str().is_empty() {
        return Ok(None);
    }
    let path = relative
        .to_str()
        .ok_or("The worktree path must be valid UTF-8")?;
    let result = output(
        repository,
        &["check-ignore", "--quiet", "--no-index", "--", path],
    )?;
    match result.status.code() {
        Some(0) => Ok(None),
        Some(1) => Ok(Some(format!(
            "{} is inside the repository but is not Git-ignored. Add an appropriate rule (for example .worktrees/) to .gitignore or .git/info/exclude before staging the parent repository. soi has not changed your ignore files.",
            destination.display()
        ))),
        _ => Err(format!(
            "Cannot check worktree ignore rules: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )),
    }
}

/// The new branch starts at `base` (a branch, tag or commit), or at the
/// checkout's `HEAD` when there's none.
pub fn create_worktree(
    repository: &Path,
    branch: &str,
    base: Option<&str>,
    destination: &Path,
) -> Result<(), String> {
    if branch.is_empty() || branch.starts_with('-') || branch.chars().any(char::is_control) {
        return Err("Choose a valid new branch name.".into());
    }
    run(repository, &["check-ref-format", "--branch", branch])?;
    // A task gets a branch of its own, never the repository's main one.
    let main = run(
        repository,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()
    .and_then(|head| head.trim().strip_prefix("origin/").map(String::from));
    if matches!(branch, "main" | "master") || main.as_deref() == Some(branch) {
        return Err(format!(
            "{branch} is the repository's main branch. Give the task a new branch, such as feature/<name>."
        ));
    }
    let local = format!("refs/heads/{branch}");
    if run(repository, &["rev-parse", "--verify", "--quiet", &local]).is_ok() {
        return Err(format!(
            "A branch named {branch} already exists. Choose a new name."
        ));
    }
    if !destination.is_absolute() {
        return Err("The worktree destination must be absolute.".into());
    }
    if destination.exists() {
        return Err("The destination already exists. Choose a new directory.".into());
    }
    // Git would otherwise make an empty worktree on an orphan branch.
    if run(repository, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Err(
            "This repository has no commits yet, so the worktree would be empty. Commit your files first."
                .into(),
        );
    }
    if let Some(base) = base {
        let commit = format!("{base}^{{commit}}");
        if base.starts_with('-')
            || base.chars().any(char::is_control)
            || run(repository, &["rev-parse", "--verify", "--quiet", &commit]).is_err()
        {
            return Err(format!(
                "No branch or commit named {base}. Check the base branch's name (for a remote one, such as origin/main, fetch first)."
            ));
        }
    }
    let destination = destination
        .to_str()
        .ok_or("The worktree path must be valid UTF-8")?;
    // Without tracking: a task branch started from origin/main must not
    // pull from or push to main.
    let mut args = vec![
        "worktree",
        "add",
        "--no-track",
        "-b",
        branch,
        "--",
        destination,
    ];
    args.extend(base);
    run(repository, &args)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Tracked,
    Ignored,
    /// Shows up in `git status` and is picked up by `git add .`.
    Untracked,
    Outside,
}

impl Visibility {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Tracked => "Tracked by Git: edits show up as changes on this branch.",
            Self::Ignored => "Ignored by Git: stays local to this worktree.",
            Self::Untracked => {
                "Not ignored: it will appear in git status and can be committed. Add a rule such as .soi/ to .git/info/exclude to keep it local; Soi does not change ignore files."
            }
            Self::Outside => "Outside this worktree's repository.",
        }
    }
}

/// Whether a (possibly not yet existing) file is tracked, ignored or would be
/// picked up by Git. Never changes the index or ignore files.
pub fn visibility(worktree: &Path, file: &Path) -> Result<Visibility, String> {
    let root = repository(worktree)?;
    // A plan may not exist yet: resolve its nearest existing ancestor.
    let file = file
        .ancestors()
        .find_map(|ancestor| {
            let resolved = std::fs::canonicalize(ancestor).ok()?;
            Some(resolved.join(file.strip_prefix(ancestor).ok()?))
        })
        .unwrap_or_else(|| file.to_path_buf());
    let Ok(relative) = file.strip_prefix(&root) else {
        return Ok(Visibility::Outside);
    };
    let path = relative
        .to_str()
        .ok_or("The plan path must be valid UTF-8")?;
    if output(&root, &["ls-files", "--error-unmatch", "--", path])?
        .status
        .success()
    {
        return Ok(Visibility::Tracked);
    }
    let result = output(
        &root,
        &["check-ignore", "--quiet", "--no-index", "--", path],
    )?;
    match result.status.code() {
        Some(0) => Ok(Visibility::Ignored),
        Some(1) => Ok(Visibility::Untracked),
        _ => Err(format!(
            "Cannot check ignore rules: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )),
    }
}

pub fn status(directory: &Path) -> Result<String, String> {
    run(
        directory,
        &["status", "--short", "--branch", "--untracked-files=normal"],
    )
}

/// What removing a task worktree would lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeState {
    /// Uncommitted changes, soi's own `.soi/` aside.
    pub changes: usize,
    /// Commits only `branch` has: on no other branch and no remote.
    pub unmerged: usize,
}

pub fn worktree_state(worktree: &Path, branch: &str) -> Result<WorktreeState, String> {
    let status = run(
        worktree,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).soi",
        ],
    )?;
    let exclude = format!("--exclude={branch}");
    let unmerged = run(
        worktree,
        &[
            "rev-list",
            "--count",
            &format!("refs/heads/{branch}"),
            "--not",
            &exclude,
            "--branches",
            "--remotes",
        ],
    )?;
    Ok(WorktreeState {
        changes: status.lines().count(),
        unmerged: unmerged
            .trim()
            .parse()
            .map_err(|_| "Git gave no commit count")?,
    })
}

/// Deletes a worktree with everything in it, then the branch if asked and
/// Git agrees it is merged (`branch -d`, never `-D`). The caller has shown
/// what would be lost. Returns a note when the branch was kept.
pub fn remove_worktree(
    repository: &Path,
    worktree: &Path,
    branch: Option<&str>,
) -> Result<Option<String>, String> {
    let path = worktree
        .to_str()
        .ok_or("The worktree path must be valid UTF-8")?;
    run(repository, &["worktree", "remove", "--force", "--", path])?;
    let Some(branch) = branch else {
        return Ok(None);
    };
    Ok(run(repository, &["branch", "-d", "--", branch])
        .err()
        .map(|_| format!("Kept the branch {branch}: Git doesn't see it merged yet.")))
}

pub fn changes(directory: &Path) -> Result<String, String> {
    let status = status(directory)?;
    let staged = run(
        directory,
        &[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--",
        ],
    )?;
    let unstaged = run(
        directory,
        &["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--"],
    )?;
    // Untracked files are listed, not opened: they can be huge, secrets, devices
    // or symlinks. A user can inspect them explicitly in their own editor.
    Ok(format!(
        "Workspace-wide changes (not attributed to an individual agent)\n\n{status}\n\n── Staged ──\n{staged}\n── Unstaged ──\n{unstaged}\nUntracked files are listed above with ??; their contents are not loaded."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_default_to_branch_names_and_accept_relative_and_absolute_paths() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let name = root.file_name().unwrap();
        assert_eq!(
            worktree_destination(&root, "feature/Login!", Path::new("")).unwrap(),
            root.join(".worktrees/feature-Login").join(name)
        );
        assert_eq!(
            worktree_destination(&root, "test", Path::new(".worktrees/test")).unwrap(),
            root.join(".worktrees/test")
        );
        let absolute = root.join(format!("soi-destination-{}", uuid::Uuid::new_v4()));
        assert_eq!(
            worktree_destination(&root, "test", &absolute).unwrap(),
            absolute
        );
        assert_eq!(
            worktree_destination(&root, "test", Path::new("nested/../task")).unwrap(),
            root.join("task")
        );
        assert!(worktree_destination(&root, "../", Path::new("")).is_err());
        assert!(
            worktree_destination(Path::new("relative-repository"), "test", Path::new("task"))
                .is_err()
        );
    }

    #[test]
    fn destination_resolution_respects_existing_symlinks() {
        let root = std::env::temp_dir().join(format!("soi-destination-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("real/child")).unwrap();
        std::os::unix::fs::symlink(root.join("real/child"), root.join("linked")).unwrap();
        let resolved = worktree_destination(&root, "test", Path::new("linked/../task")).unwrap();
        assert_eq!(
            resolved,
            root.join("real").canonicalize().unwrap().join("task")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn repositories_without_commits_get_no_worktree() {
        let root = std::env::temp_dir().join(format!("soi-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init"]).unwrap();
        let destination = root.join(".worktrees/task");
        let error = create_worktree(&root, "task", None, &destination).unwrap_err();
        assert!(error.contains("no commits yet"), "{error}");
        assert!(!destination.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn task_branches_are_new_and_never_the_main_branch() {
        let root = std::env::temp_dir().join(format!("soi-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init"]).unwrap();
        run(
            &root,
            &[
                "-c",
                "user.name=Soi Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        )
        .unwrap();
        run(&root, &["branch", "taken"]).unwrap();
        let destination = root.join(".worktrees/task");
        for main in ["main", "master"] {
            let error = create_worktree(&root, main, None, &destination).unwrap_err();
            assert!(error.contains("main branch"), "{error}");
        }
        let error = create_worktree(&root, "taken", None, &destination).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert!(!destination.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn task_branches_start_at_the_chosen_base() {
        let root = std::env::temp_dir().join(format!("soi-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let commit = || {
            run(
                &root,
                &[
                    "-c",
                    "user.name=Soi Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgSign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "change",
                ],
            )
            .unwrap();
        };
        run(&root, &["init"]).unwrap();
        commit();
        run(&root, &["branch", "release"]).unwrap();
        commit();
        let head = |directory: &Path| run(directory, &["rev-parse", "HEAD"]).unwrap();

        let from_head = root.join(".worktrees/from-head");
        create_worktree(&root, "from-head", None, &from_head).unwrap();
        assert_eq!(head(&from_head), head(&root));

        let from_release = root.join(".worktrees/from-release");
        create_worktree(&root, "from-release", Some("release"), &from_release).unwrap();
        assert_eq!(
            head(&from_release),
            run(&root, &["rev-parse", "release"]).unwrap()
        );
        assert!(
            run(&root, &["config", "branch.from-release.merge"]).is_err(),
            "doesn't track its base"
        );

        let missing = root.join(".worktrees/missing");
        let error = create_worktree(&root, "missing", Some("nope"), &missing).unwrap_err();
        assert!(error.contains("No branch or commit named nope"), "{error}");
        assert!(create_worktree(&root, "dash", Some("--bad"), &root.join("dash")).is_err());
        assert!(!missing.exists());
        assert!(run(&root, &["rev-parse", "--verify", "refs/heads/missing"]).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn removing_worktrees_reports_what_is_lost_and_keeps_unmerged_branches() {
        let root = std::env::temp_dir().join(format!("soi-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let commit = |directory: &Path| {
            run(
                directory,
                &[
                    "-c",
                    "user.name=Soi Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgSign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "change",
                ],
            )
            .unwrap();
        };
        run(&root, &["init"]).unwrap();
        commit(&root);
        let repository = repository(&root).unwrap();
        let worktree = repository.join(".worktrees/task");
        create_worktree(&repository, "task", None, &worktree).unwrap();
        // A worktree's tasks belong with its repository's.
        assert_eq!(main_repository(&worktree).unwrap(), repository);
        assert_eq!(main_repository(&repository).unwrap(), repository);
        std::fs::create_dir(worktree.join(".soi")).unwrap();
        std::fs::write(worktree.join(".soi/plan.md"), "plan").unwrap();
        let state = |worktree: &Path| worktree_state(worktree, "task").unwrap();
        assert_eq!(
            state(&worktree),
            WorktreeState {
                changes: 0,
                unmerged: 0
            },
            "soi's plan is not a change"
        );
        std::fs::write(worktree.join("new.txt"), "work").unwrap();
        commit(&worktree);
        assert_eq!(
            state(&worktree),
            WorktreeState {
                changes: 1,
                unmerged: 1
            }
        );
        let note = remove_worktree(&repository, &worktree, Some("task")).unwrap();
        assert!(note.unwrap().contains("Kept the branch task"));
        assert!(!worktree.exists());
        assert!(run(&repository, &["rev-parse", "--verify", "refs/heads/task"]).is_ok());

        let merged = repository.join(".worktrees/merged");
        create_worktree(&repository, "merged", None, &merged).unwrap();
        assert_eq!(
            remove_worktree(&repository, &merged, Some("merged")).unwrap(),
            None
        );
        assert!(run(&repository, &["rev-parse", "--verify", "refs/heads/merged"]).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn isolated_worktree_and_read_only_diffs() {
        let root = std::env::temp_dir().join(format!("soi-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init"]).unwrap();
        run(
            &root,
            &[
                "-c",
                "user.name=Soi Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        )
        .unwrap();
        let repository = repository(&root).unwrap();
        let relative = worktree_destination(&repository, "relative-task", Path::new("")).unwrap();
        assert!(
            worktree_ignore_warning(&repository, &relative)
                .unwrap()
                .is_some()
        );
        std::fs::write(root.join(".gitignore"), ".worktrees/\n").unwrap();
        assert!(
            worktree_ignore_warning(&repository, &relative)
                .unwrap()
                .is_none()
        );
        create_worktree(&repository, "task/relative", None, &relative).unwrap();
        assert!(status(&relative).unwrap().contains("task/relative"));
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            ".worktrees/\n"
        );
        let sibling = repository.parent().unwrap().join("outside-worktree");
        assert!(
            worktree_ignore_warning(&repository, &sibling)
                .unwrap()
                .is_none()
        );
        let destination = root.join("task space");
        create_worktree(&root, "task/test", None, &destination).unwrap();
        assert!(status(&destination).unwrap().contains("task/test"));
        assert!(create_worktree(&root, "another", None, &destination).is_err());
        assert!(create_worktree(&root, "--bad", None, &root.join("bad")).is_err());
        std::fs::write(destination.join("hello.txt"), "hello\n").unwrap();
        run(&destination, &["add", "hello.txt"]).unwrap();
        std::fs::write(destination.join("hello.txt"), "changed\n").unwrap();
        std::fs::write(destination.join("untracked.txt"), "private").unwrap();
        let before = status(&destination).unwrap();
        let diff = changes(&destination).unwrap();
        assert!(diff.contains("+hello"));
        assert!(diff.contains("+changed"));
        assert!(diff.contains("?? untracked.txt"));
        assert!(!diff.contains("private"));
        assert_eq!(before, status(&destination).unwrap());
        let plan = destination.join(".soi/plan.md");
        assert_eq!(
            visibility(&destination, &plan).unwrap(),
            Visibility::Untracked
        );
        assert_eq!(
            visibility(&destination, &destination.join("hello.txt")).unwrap(),
            Visibility::Tracked
        );
        std::fs::write(destination.join(".gitignore"), ".soi/\n").unwrap();
        assert_eq!(
            visibility(&destination, &plan).unwrap(),
            Visibility::Ignored
        );
        assert_eq!(
            visibility(&destination, &sibling.join("plan.md")).unwrap(),
            Visibility::Outside
        );
        std::fs::remove_file(destination.join(".gitignore")).unwrap();
        run(
            &root,
            &[
                "worktree",
                "remove",
                "--force",
                destination.to_str().unwrap(),
            ],
        )
        .unwrap();
        // Only this test's uniquely owned temporary repository is removed.
        std::fs::remove_dir_all(root).unwrap();
    }
}
