//! Plan files on disk. Soi never overwrites a plan it has not seen: saves
//! compare the file with the version the editor started from, and a new plan
//! is only ever created where no file exists yet.
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use uuid::Uuid;

pub const LIMIT: u64 = 512 * 1024;

/// Bounded, non-blocking read so a FIFO or huge file cannot hang the UI.
pub fn read(path: &Path) -> Result<String, String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Plans must be regular files.".into());
    }
    let mut text = String::new();
    (&mut file)
        .take(LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > LIMIT {
        return Err("Plan exceeds the 512 KiB limit.".into());
    }
    Ok(text)
}

/// Creates a new plan, including missing parent directories. An existing file
/// is an error, never a target: the caller links it instead.
pub fn create(path: &Path, text: &str) -> Result<(), CreateError> {
    if !path.is_absolute() {
        return Err(CreateError::Failed("Plan paths must be absolute.".into()));
    }
    if text.len() as u64 > LIMIT {
        return Err(CreateError::Failed(
            "Plan exceeds the 512 KiB limit.".into(),
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CreateError::Failed(format!("Cannot create {}: {e}", parent.display())))?;
    }
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(CreateError::Exists),
        Err(e) => {
            return Err(CreateError::Failed(format!(
                "Cannot create {}: {e}",
                path.display()
            )));
        }
    };
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| CreateError::Failed(format!("Cannot write {}: {e}", path.display())))
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    Exists,
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The file no longer matches what the editor loaded; carries the
    /// current disk content (`None` when it was deleted).
    Conflict(Option<String>),
    Failed(String),
}

/// Atomically replaces the plan with `text` if the file still holds `base`
/// (`None`: the file must not exist). A symlinked plan updates its target.
/// Another writer can still slip in between the check and the rename; the
/// editor's disk polling reports that change afterwards.
pub fn save(path: &Path, text: &str, base: Option<&str>) -> Result<(), SaveError> {
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(SaveError::Failed(format!("Cannot resolve plan: {e}"))),
    };
    let current = match read(&target) {
        Ok(current) => Some(current),
        Err(_) if !target.exists() => None,
        Err(e) => return Err(SaveError::Failed(e)),
    };
    if current.as_deref() != base {
        return Err(SaveError::Conflict(current));
    }
    if text.len() as u64 > LIMIT {
        return Err(SaveError::Failed("Plan exceeds the 512 KiB limit.".into()));
    }
    let Some(parent) = target.parent() else {
        return Err(SaveError::Failed("Plans need a parent directory.".into()));
    };
    fs::create_dir_all(parent).map_err(|e| SaveError::Failed(e.to_string()))?;
    let mode = fs::metadata(&target).map_or(0o644, |m| m.permissions().mode() & 0o7777);
    let temp = temp_path(&target);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(&temp, &target)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(SaveError::Failed(format!(
            "Cannot save {}: {e}",
            target.display()
        )));
    }
    Ok(())
}

/// Whether `path`, an existing file under `root`, really lives elsewhere:
/// a symlink a repository committed can point anywhere. Unresolvable
/// paths count as elsewhere.
pub fn leaves(path: &Path, root: &Path) -> bool {
    match (fs::canonicalize(path), fs::canonicalize(root)) {
        (Ok(target), Ok(root)) => !target.starts_with(root),
        _ => true,
    }
}

fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{}.tmp", Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let root = std::env::temp_dir().join(format!("soi-plan-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn symlinks_out_of_the_root_leave_it() {
        let root = dir();
        let worktree = root.join("worktree");
        fs::create_dir_all(worktree.join(".soi")).unwrap();
        fs::write(root.join("outside.md"), "secret").unwrap();
        fs::write(worktree.join("inside.md"), "plan").unwrap();
        let plan = worktree.join(".soi/plan.md");
        std::os::unix::fs::symlink("../../outside.md", &plan).unwrap();
        assert!(leaves(&plan, &worktree));
        let inner = worktree.join(".soi/inner.md");
        std::os::unix::fs::symlink("../inside.md", &inner).unwrap();
        assert!(!leaves(&inner, &worktree));
        assert!(!leaves(&worktree.join("inside.md"), &worktree));
        assert!(leaves(&worktree.join("missing.md"), &worktree));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_regular_files_and_bounds_size() {
        let root = dir();
        let path = root.join("plan.md");
        assert!(read(&path).is_err());
        assert!(read(&root).is_err());
        fs::write(&path, "# Plan").unwrap();
        assert_eq!(read(&path).unwrap(), "# Plan");
        fs::write(&path, vec![b'a'; LIMIT as usize + 1]).unwrap();
        assert!(read(&path).unwrap_err().contains("limit"));
        fs::write(&path, "").unwrap();
        assert_eq!(read(&path).unwrap(), "");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_creation_leaves_no_file_or_directories() {
        let root = dir();
        let path = root.join("new/plan.md");
        assert!(matches!(
            create(&path, &"x".repeat(LIMIT as usize + 1)),
            Err(CreateError::Failed(_))
        ));
        assert!(!path.parent().unwrap().exists());
        create(&path, &"x".repeat(LIMIT as usize)).unwrap();
        assert_eq!(read(&path).unwrap().len(), LIMIT as usize);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_never_overwrites() {
        let root = dir();
        let path = root.join(".soi/plan.md");
        create(&path, "# New").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# New");
        assert_eq!(create(&path, "# Other"), Err(CreateError::Exists));
        assert_eq!(fs::read_to_string(&path).unwrap(), "# New");
        assert!(matches!(
            create(Path::new("relative.md"), ""),
            Err(CreateError::Failed(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_detects_external_edits_and_keeps_mode_and_symlinks() {
        let root = dir();
        let path = root.join("plan.md");
        assert_eq!(
            save(&path, "x", Some("old")),
            Err(SaveError::Conflict(None))
        );
        save(&path, "one", None).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        save(&path, "two", Some("one")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::write(&path, "agent edit").unwrap();
        assert_eq!(
            save(&path, "mine", Some("two")),
            Err(SaveError::Conflict(Some("agent edit".into())))
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "agent edit");
        let link = root.join("link.md");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        save(&link, "via link", Some("agent edit")).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "via link");
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            2,
            "no temp files left"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
