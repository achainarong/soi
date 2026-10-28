//! Asking a short-lived helper program something, such as a version or a
//! login shell's PATH, without trusting it to finish, stay quiet or close
//! its output.

use std::io::{self, Read};
use std::ops::ControlFlow;
use std::os::fd::AsRawFd;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Runs `command` with no stdin or stderr and hands its stdout to `output`
/// chunk by chunk until it exits. `None` at `timeout`, on a read error, or
/// when `output` breaks. The direct child is always killed and reaped, and
/// pipe EOF is never awaited: a background process the helper left behind
/// may hold it open.
pub fn run(
    command: &mut Command,
    timeout: Duration,
    mut output: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Option<ExitStatus> {
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
        let mut chunk = [0; 8192];
        loop {
            let status = child.try_wait().ok()?;
            // Drain while the child runs: a chatty program can fill the pipe.
            loop {
                if started.elapsed() >= timeout {
                    return None;
                }
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(len) => {
                        if output(&chunk[..len]).is_break() {
                            return None;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => return None,
                }
            }
            if let Some(status) = status {
                return Some(status);
            }
            if started.elapsed() < timeout {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}
