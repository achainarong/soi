//! Linux: process details from `/proc`, the XDG runtime and data directories.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::Platform;

pub struct Linux;

/// The fields of `/proc/<pid>/stat` after "pid (name) ": state ppid pgrp
/// session tty_nr tpgid …
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat[stat.rfind(')')? + 1..].split_whitespace();
    Some(fields.map(String::from).collect())
}

/// The terminal's foreground process group (its leader's PID), unless it's
/// the shell's own.
fn foreground_group(shell: u32) -> Option<u32> {
    let foreground: u32 = stat_fields(shell)?.get(5)?.parse().ok()?;
    (foreground != 0 && foreground != shell).then_some(foreground)
}

/// The process group a process is in.
fn group_of(pid: u32) -> Option<u32> {
    stat_fields(pid)?.get(2)?.parse().ok()
}

/// Stops a walk through a fork bomb's descendants, or a process's threads:
/// how many of either it reads.
const MAX_DESCENDANTS: usize = 4096;

/// The members of `group` below `shell`, other than its leader: those the
/// terminal runs. Walks the shell's descendants only, so it costs the same
/// however many processes the system has (it runs every second for every
/// terminal). None when the kernel doesn't list children.
fn group_descendants(shell: u32, group: u32) -> Option<Vec<u32>> {
    static LISTED: OnceLock<bool> = OnceLock::new();
    if !*LISTED.get_or_init(|| Path::new("/proc/thread-self/children").exists()) {
        return None;
    }
    let mut members = Vec::new();
    let mut pending = vec![shell];
    let mut visited = 0;
    'walk: while let Some(pid) = pending.pop() {
        // Each thread lists the children it started. Some may exit meanwhile.
        let Ok(threads) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for thread in threads.flatten() {
            visited += 1;
            if visited > MAX_DESCENDANTS {
                break 'walk;
            }
            let Ok(children) = std::fs::read_to_string(thread.path().join("children")) else {
                continue;
            };
            for child in children.split_whitespace().filter_map(|c| c.parse().ok()) {
                if child != group && group_of(child) == Some(group) {
                    members.push(child);
                }
                pending.push(child);
            }
        }
    }
    Some(members)
}

/// The members of `group` other than its leader, from every process on the
/// system: slow, for kernels that don't list children.
fn group_scan(group: u32) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter(|&pid| pid != group && group_of(pid) == Some(group))
        .collect()
}

fn comm(pid: u32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(name.trim().to_string())
}

impl Platform for Linux {
    fn foreground_program(&self, shell: u32) -> Option<String> {
        let foreground = foreground_group(shell)?;
        comm(foreground)
    }

    fn foreground_programs(&self, shell: u32) -> Vec<String> {
        let Some(group) = foreground_group(shell) else {
            return Vec::new();
        };
        let mut members = group_descendants(shell, group).unwrap_or_else(|| group_scan(group));
        members.sort_unstable();
        std::iter::once(group)
            .chain(members)
            .filter_map(comm)
            .collect()
    }

    fn process_cwd(&self, pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    fn runtime_dir(&self) -> Option<PathBuf> {
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
    }

    fn monospace_family(&self) -> &'static str {
        "Monospace"
    }

    fn kernel_filesystems(&self) -> &'static [&'static str] {
        &["/proc", "/sys", "/dev", "/run"]
    }

    /// `$XDG_DATA_HOME` followed by `$XDG_DATA_DIRS`.
    fn data_dirs(&self) -> Vec<PathBuf> {
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        std::iter::once(crate::dirs::data_home())
            .chain(system.split(':').map(PathBuf::from))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    #[test]
    fn walking_descendants_finds_the_group_like_a_full_scan() {
        let mut leader = std::process::Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 30 & wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        let group = leader.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = Vec::new();
        while found.is_empty() && Instant::now() < deadline {
            found = group_descendants(std::process::id(), group).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut scanned = group_scan(group);
        found.sort_unstable();
        scanned.sort_unstable();
        // Read /proc while the child is alive; cleanup can remove its entry.
        let name = found.first().and_then(|&pid| comm(pid));
        let _ = std::process::Command::new("/bin/kill")
            .args(["--", &format!("-{group}")])
            .status();
        let _ = leader.wait();
        assert_eq!(found.len(), 1);
        assert_eq!(found, scanned);
        assert_eq!(name.as_deref(), Some("sleep"));
    }
}
