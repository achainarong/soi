//! Linux: process details from `/proc`, the XDG runtime and data directories.

use std::path::PathBuf;

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
        let mut members: Vec<u32> = std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .filter(|&pid| {
                pid != group
                    && stat_fields(pid).is_some_and(|f| f.get(2) == Some(&group.to_string()))
            })
            .collect();
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
