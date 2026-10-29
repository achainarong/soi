//! Where soi keeps its files: the XDG base directories, on macOS too (like
//! Ghostty, Kitty or Neovim there), so settings stay in `~/.config`.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, mpsc};

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
        // Without the thread the receiver is dropped: jobs run where queued.
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
    if let Err(mpsc::SendError(job)) = writer().send(Box::new(job)) {
        job();
    }
}

/// Waits until the writes queued so far are done: before reading what
/// they wrote, and before quitting.
pub fn flush() {
    let (done, wait) = mpsc::channel();
    write_later(move || {
        let _ = done.send(());
    });
    let _ = wait.recv();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
