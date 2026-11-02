//! Where soi keeps its files: the XDG base directories, on macOS too (like
//! Ghostty, Kitty or Neovim there), so settings stay in `~/.config`.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// The user's home directory, else `/`.
pub fn home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_STATE_HOME`, else `~/.local/state`.
pub fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home_dir().join(fallback))
}

/// Replaces `path` with `bytes`, so a crash or a full disk leaves the old
/// file or the new one, never a truncated mix. A symlink (a dotfiles
/// checkout) is written through to its target rather than replaced, and an
/// existing file keeps its permissions; a new one gets `mode`, less the
/// umask.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let path = match fs::canonicalize(path) {
        Ok(target) => target,
        // A dangling symlink: let the plain write create what it points to.
        Err(err) if err.kind() == io::ErrorKind::NotFound && path.is_symlink() => {
            return fs::write(path, bytes);
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(err) => return Err(err),
    };
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::other(format!("cannot write {}", path.display())));
    };
    fs::create_dir_all(dir)?;
    let permissions = fs::metadata(&path).ok().map(|meta| meta.permissions());
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

type Job = Box<dyn FnOnce() + Send>;

/// The thread that writes state files, so the GTK thread never waits for
/// the disk (`write_atomic` syncs). Jobs run one at a time, in order.
fn writer() -> &'static mpsc::Sender<Job> {
    static WRITER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (send, receive) = mpsc::channel::<Job>();
        // Never fall back to synchronous disk I/O on the GTK thread.
        let _ = std::thread::Builder::new()
            .name("soi-writer".into())
            .spawn(move || {
                for job in receive {
                    job();
                }
            });
        send
    })
}

/// Runs `job` on the writer thread, after the jobs queued before it.
pub fn write_later(job: impl FnOnce() + Send + 'static) {
    if writer().send(Box::new(job)).is_err() {
        crate::log::write("state writer is unavailable; queued work was not completed");
    }
}

/// What to do to a file once the writer thread gets to it: write these
/// bytes (with this mode), or delete it.
type Pending = Option<(Vec<u8>, u32)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveStatus {
    Pending,
    Saved,
    Failed(String),
    Superseded,
}

#[derive(Clone)]
pub struct SaveReceipt(Arc<Mutex<SaveStatus>>);

impl SaveReceipt {
    pub fn status(&self) -> SaveStatus {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, status: SaveStatus) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

struct PendingSave {
    content: Pending,
    receipt: SaveReceipt,
    scheduled: bool,
    attempts: u32,
    retry_at: Instant,
    what: &'static str,
    last_error: Option<String>,
}

fn pending() -> &'static Mutex<HashMap<PathBuf, PendingSave>> {
    static PENDING: OnceLock<Mutex<HashMap<PathBuf, PendingSave>>> = OnceLock::new();
    PENDING.get_or_init(Mutex::default)
}

/// `write_atomic` on the writer thread, or deleting the file with `None`.
/// Only the latest content queued for a path is written, so however slow
/// the disk, at most one copy of each file waits in memory. What fails is
/// retained for retry and logged as failing to save `what`.
pub fn save_later(path: PathBuf, content: Pending, what: &'static str) -> SaveReceipt {
    let receipt = SaveReceipt(Arc::new(Mutex::new(SaveStatus::Pending)));
    let schedule = {
        let mut pending = pending().lock().unwrap_or_else(PoisonError::into_inner);
        let scheduled = pending.get(&path).is_some_and(|save| save.scheduled);
        let last_error = pending.get(&path).and_then(|save| save.last_error.clone());
        if let Some(old) = pending.insert(
            path.clone(),
            PendingSave {
                content,
                receipt: receipt.clone(),
                scheduled: true,
                attempts: 0,
                retry_at: Instant::now(),
                what,
                last_error,
            },
        ) {
            old.receipt.set(SaveStatus::Superseded);
        }
        !scheduled
    };
    if schedule {
        write_later(move || process_save(path));
    }
    receipt
}

fn process_save(path: PathBuf) {
    let (content, receipt, what) = {
        let pending = pending().lock().unwrap_or_else(PoisonError::into_inner);
        let Some(save) = pending.get(&path) else {
            return;
        };
        (save.content.clone(), save.receipt.clone(), save.what)
    };
    let result = match content {
        Some((bytes, mode)) => write_atomic(&path, &bytes, mode),
        None => match fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
    };
    let mut pending = pending().lock().unwrap_or_else(PoisonError::into_inner);
    let Some(save) = pending.get_mut(&path) else {
        return;
    };
    if !Arc::ptr_eq(&save.receipt.0, &receipt.0) {
        // A newer revision arrived during I/O. Never discard it or acknowledge
        // it as saved; run it after this revision finishes.
        drop(pending);
        write_later(move || process_save(path));
        return;
    }
    match result {
        Ok(()) => {
            receipt.set(SaveStatus::Saved);
            pending.remove(&path);
        }
        Err(err) => {
            receipt.set(SaveStatus::Failed(err.to_string()));
            save.last_error = Some(err.to_string());
            save.scheduled = false;
            save.attempts = save.attempts.saturating_add(1);
            save.retry_at = Instant::now() + Duration::from_secs(1 << save.attempts.min(5));
            drop(pending);
            crate::log::write(&format!("failed to save {what}: {err}"));
        }
    }
}

/// Retry retained failures, with capped exponential backoff. Explicit Retry
/// bypasses the delay, but never starts another copy of an in-flight write.
pub fn retry_pending(force: bool) {
    let paths = {
        let mut pending = pending().lock().unwrap_or_else(PoisonError::into_inner);
        pending
            .iter_mut()
            .filter_map(|(path, save)| {
                if !save.scheduled && (force || Instant::now() >= save.retry_at) {
                    save.scheduled = true;
                    save.receipt.set(SaveStatus::Pending);
                    Some(path.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    for path in paths {
        write_later(move || process_save(path));
    }
}

/// Latest queued content, including writes in flight or awaiting retry. Reads
/// need not wait for unrelated disk work to see the newest revision.
pub fn pending_content(path: &Path) -> Option<Option<Vec<u8>>> {
    pending()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        .map(|save| save.content.as_ref().map(|(bytes, _)| bytes.clone()))
}

pub fn save_error() -> Option<String> {
    pending()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .find_map(|save| {
            save.last_error
                .as_ref()
                .map(|error| format!("Cannot save {}: {error}", save.what))
        })
}

pub fn flush_timeout(timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let (done, wait) = mpsc::channel();
        write_later(move || {
            let _ = done.send(());
        });
        wait.recv_timeout(timeout.saturating_sub(started.elapsed()))
            .map_err(|e| format!("State writes did not finish: {e}"))?;
        let pending = pending().lock().unwrap_or_else(PoisonError::into_inner);
        if pending.is_empty() {
            return Ok(());
        }
        if let Some(error) = pending
            .values()
            .filter(|save| !save.scheduled)
            .find_map(|save| {
                save.last_error
                    .as_ref()
                    .map(|error| format!("Cannot save {}: {error}", save.what))
            })
        {
            return Err(error);
        }
        if started.elapsed() >= timeout {
            return Err("State writes are still pending.".into());
        }
        // An in-flight write can enqueue its newer revision behind our first
        // barrier. Wait for that revision too, within the original deadline.
    }
}

/// Bounded compatibility helper for tests and explicit synchronous callers.
#[cfg(test)]
pub fn flush() {
    if let Err(error) = flush_timeout(Duration::from_secs(5)) {
        crate::log::write(&error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    static WRITER_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("soi-dirs-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saves_queued_for_a_file_keep_only_the_latest() {
        let _guard = WRITER_TEST_LOCK.lock().unwrap();
        let dir = TempDir::new();
        let path = dir.0.join("state.json");
        // Holds up the writer thread, as a slow disk would.
        let (release, wait) = mpsc::channel::<()>();
        write_later(move || {
            let _ = wait.recv();
        });
        for text in ["one", "two", "three"] {
            save_later(path.clone(), Some((text.into(), 0o600)), "test");
        }
        let queued = pending()
            .lock()
            .unwrap()
            .keys()
            .filter(|p| **p == path)
            .count();
        release.send(()).unwrap();
        flush();
        assert_eq!(queued, 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "three");

        save_later(path.clone(), Some((b"four".to_vec(), 0o600)), "test");
        save_later(path.clone(), None, "test");
        flush();
        assert!(!path.exists());
    }

    #[test]
    fn failed_saves_are_retained_and_retry_without_another_edit() {
        let _guard = WRITER_TEST_LOCK.lock().unwrap();
        let dir = TempDir::new();
        let parent = dir.0.join("blocked");
        fs::write(&parent, "not a directory").unwrap();
        let path = parent.join("session.json");
        let receipt = save_later(path.clone(), Some((b"latest".to_vec(), 0o600)), "test");
        let result = flush_timeout(Duration::from_secs(2));
        let status = receipt.status();
        let retained = pending_content(&path);
        fs::remove_file(&parent).unwrap();
        // The normal timer retries a failed save when its backoff expires.
        pending().lock().unwrap().get_mut(&path).unwrap().retry_at = Instant::now();
        retry_pending(false);
        flush_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_err());
        assert!(matches!(status, SaveStatus::Failed(_)));
        assert_eq!(retained, Some(Some(b"latest".to_vec())));
        assert_eq!(receipt.status(), SaveStatus::Saved);
        assert_eq!(fs::read(&path).unwrap(), b"latest");
        assert!(pending_content(&path).is_none());
    }

    #[test]
    fn blocked_writer_has_bounded_flush_and_readable_pending_content() {
        let _guard = WRITER_TEST_LOCK.lock().unwrap();
        let dir = TempDir::new();
        let path = dir.0.join("session.json");
        let (release, wait) = mpsc::channel();
        write_later(move || {
            let _ = wait.recv_timeout(Duration::from_secs(3));
        });
        let old = save_later(path.clone(), Some((b"old".to_vec(), 0o600)), "test");
        let latest = save_later(path.clone(), Some((b"new".to_vec(), 0o600)), "test");
        let started = Instant::now();
        let result = flush_timeout(Duration::from_millis(20));
        let elapsed = started.elapsed();
        let content = pending_content(&path);
        let status = latest.status();
        release.send(()).unwrap();
        flush_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_err());
        assert!(elapsed < Duration::from_secs(1));
        assert_eq!(content, Some(Some(b"new".to_vec())));
        assert_eq!(status, SaveStatus::Pending);
        assert_eq!(old.status(), SaveStatus::Superseded);
        assert_eq!(latest.status(), SaveStatus::Saved);
        assert_eq!(fs::read(path).unwrap(), b"new");
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn write_atomic_creates_parents_and_replaces_contents() {
        let dir = TempDir::new();
        let path = dir.0.join("nested/file.json");
        write_atomic(&path, b"one", 0o600).unwrap();
        write_atomic(&path, b"two", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(mode(&path), 0o600);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temp file is left behind");
    }

    #[test]
    fn write_atomic_keeps_an_existing_files_permissions() {
        let dir = TempDir::new();
        let path = dir.0.join("config");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&path, b"new", 0o600).unwrap();
        assert_eq!(mode(&path), 0o640);
    }

    #[test]
    fn write_atomic_writes_through_a_symlink() {
        let dir = TempDir::new();
        let target = dir.0.join("dotfiles/settings.json");
        fs::create_dir(dir.0.join("dotfiles")).unwrap();
        fs::write(&target, "old").unwrap();
        let link = dir.0.join("settings.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_atomic(&link, b"new", 0o644).unwrap();
        assert!(link.is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn write_atomic_creates_a_dangling_symlinks_target() {
        let dir = TempDir::new();
        let target = dir.0.join("missing");
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_atomic(&link, b"new", 0o644).unwrap();
        assert!(link.is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn write_atomic_leaves_the_old_file_when_it_cannot_write() {
        let dir = TempDir::new();
        let path = dir.0.join("file");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o500)).unwrap();
        let result = write_atomic(&path, b"new", 0o644);
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
    }
}
