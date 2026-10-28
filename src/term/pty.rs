//! Starts the shell on a pseudo-terminal and moves its output to the GTK
//! main loop through a bounded channel. Reads and writes run on background
//! threads; queued input is bounded and never waits for the slave to read.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub enum Event {
    Output(Vec<u8>),
    Exited,
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    input: Arc<InputQueue>,
    stopped: Arc<AtomicBool>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pub pid: Option<u32>,
}

const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const OUTPUT_QUEUE_SIZE: usize = 64;
const IO_CHUNK_BYTES: usize = 64 * 1024;
static REAPERS: AtomicUsize = AtomicUsize::new(0);

struct Reaper;

impl Drop for Reaper {
    fn drop(&mut self) {
        REAPERS.fetch_sub(1, Ordering::Release);
    }
}

/// After all terminals have closed, let parallel reapers finish before the
/// process exits. Otherwise a child ignoring SIGHUP outlives its supervisor.
pub(crate) fn wait_for_shutdown() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while REAPERS.load(Ordering::Acquire) != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if REAPERS.load(Ordering::Acquire) != 0 {
        crate::log::write("terminal child cleanup exceeded the shutdown deadline");
    }
}

#[derive(Default)]
struct InputState {
    bytes: VecDeque<u8>,
    closed: bool,
}

/// Admit each input atomically, so rejecting a paste cannot leave half a
/// bracketed-paste sequence in the shell. Only the worker touches the writer.
#[derive(Default)]
struct InputQueue {
    state: Mutex<InputState>,
    ready: Condvar,
}

impl InputQueue {
    fn push(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if bytes.len() > MAX_INPUT_BYTES.saturating_sub(state.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "terminal input queue is full",
            ));
        }
        state.bytes.extend(bytes);
        self.ready.notify_one();
        Ok(())
    }

    fn take(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock().unwrap();
        while state.bytes.is_empty() && !state.closed {
            state = self.ready.wait(state).unwrap();
        }
        if state.closed {
            return None;
        }
        let count = state.bytes.len().min(IO_CHUNK_BYTES);
        Some(state.bytes.drain(..count).collect())
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.bytes.clear();
        self.ready.notify_all();
    }
}

fn wait_ready(fd: BorrowedFd<'_>, events: libc::c_short) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    // SAFETY: descriptor is valid for the call, and fd remains owned by the
    // worker. The finite timeout lets shutdown interrupt a stalled peer.
    if unsafe { libc::poll(&mut descriptor, 1, 20) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

fn send_event(
    sender: &async_channel::Sender<Event>,
    mut event: Event,
    stopped: &AtomicBool,
) -> bool {
    while !stopped.load(Ordering::Relaxed) {
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(async_channel::TrySendError::Closed(_)) => return false,
            Err(async_channel::TrySendError::Full(pending)) => {
                event = pending;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    false
}

pub fn spawn(
    argv: &[String],
    cwd: &Path,
    env: &[(&str, &str)],
    size: PtySize,
) -> Result<(Pty, async_channel::Receiver<Event>), String> {
    let program = argv.first().ok_or("no command to run")?;
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| e.to_string())?;
    let raw_fd = pair
        .master
        .as_raw_fd()
        .ok_or("PTY has no Unix descriptor")?;
    // SAFETY: pair.master owns raw_fd for both fcntl calls and this borrow.
    // Cloned PTY handles share these status flags, so both workers handle EAGAIN.
    let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // SAFETY: pair.master keeps the descriptor alive until duplication finishes.
    let write_fd = unsafe { BorrowedFd::borrow_raw(raw_fd) }
        .try_clone_to_owned()
        .map_err(|e| e.to_string())?;
    let read_fd = write_fd.try_clone().map_err(|e| e.to_string())?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let mut command = CommandBuilder::new(program);
    command.args(&argv[1..]);
    command.cwd(cwd);
    for (key, outer) in crate::platform::gtk_only_env() {
        match outer {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        }
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| format!("{program}: {e}"))?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let pid = child.process_id();

    let input = Arc::new(InputQueue::default());
    let stopped = Arc::new(AtomicBool::new(false));
    let writer_input = input.clone();
    let writer_stopped = stopped.clone();
    std::thread::spawn(move || {
        while let Some(bytes) = writer_input.take() {
            let mut remaining = bytes.as_slice();
            while !remaining.is_empty() && !writer_stopped.load(Ordering::Relaxed) {
                match writer.write(remaining) {
                    Ok(0) => {
                        writer_input.close();
                        return;
                    }
                    Ok(n) => remaining = &remaining[n..],
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        if wait_ready(write_fd.as_fd(), libc::POLLOUT).is_err() {
                            writer_input.close();
                            return;
                        }
                    }
                    Err(_) => {
                        writer_input.close();
                        return;
                    }
                }
            }
        }
    });

    let (sender, receiver) = async_channel::bounded(OUTPUT_QUEUE_SIZE);
    let reader_input = input.clone();
    let reader_stopped = stopped.clone();
    REAPERS.fetch_add(1, Ordering::Relaxed);
    std::thread::spawn(move || {
        let _reaper = Reaper;
        let mut buffer = vec![0u8; IO_CHUNK_BYTES];
        while !reader_stopped.load(Ordering::Relaxed) {
            match reader.read(&mut buffer) {
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if wait_ready(read_fd.as_fd(), libc::POLLIN).is_err() {
                        break;
                    }
                }
                // Linux reports EIO once the child has exited.
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if !send_event(
                        &sender,
                        Event::Output(buffer[..n].to_vec()),
                        &reader_stopped,
                    ) {
                        break;
                    }
                }
            }
        }
        // On macOS a child exiting with unread output waits until the
        // terminal is drained or its master closed; close ours first.
        drop(reader);
        drop(read_fd);
        let mut closing = None;
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {}
            }
            if reader_stopped.load(Ordering::Relaxed) {
                let started = closing.get_or_insert_with(std::time::Instant::now);
                if started.elapsed() >= std::time::Duration::from_millis(500) {
                    if let Some(pid) = child.process_id() {
                        // SAFETY: this worker still owns the unreaped child;
                        // its PID cannot be reused before our wait completes.
                        unsafe {
                            libc::kill(pid as libc::pid_t, libc::SIGKILL);
                        }
                    }
                    let _ = child.wait();
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        reader_input.close();
        send_event(&sender, Event::Exited, &reader_stopped);
    });

    Ok((
        Pty {
            master: pair.master,
            input,
            stopped,
            killer,
            pid,
        },
        receiver,
    ))
}

/// The program in the foreground of a shell's terminal, or `None` when the
/// shell itself is in the foreground (waiting at its prompt).
pub fn foreground_program(shell: u32) -> Option<String> {
    crate::platform::current().foreground_program(shell)
}

/// Every program in the foreground of a shell's terminal, the leading one
/// first; empty when the shell itself is in the foreground.
pub fn foreground_programs(shell: u32) -> Vec<String> {
    crate::platform::current().foreground_programs(shell)
}

impl Pty {
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.input.push(bytes)
    }

    pub fn resize(&self, size: PtySize) {
        let _ = self.master.resize(size);
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.input.close();
        let _ = self.killer.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_a_terminal_reaps_a_child_that_ignores_hangup() {
        let argv = [
            "/bin/sh",
            "-c",
            "trap '' HUP; printf ready; exec /bin/sleep 60",
        ]
        .map(String::from);
        let (pty, events) = spawn(&argv, &std::env::temp_dir(), &[], test_size()).unwrap();
        let pid = pty.pid.unwrap();
        assert!(matches!(events.recv_blocking().unwrap(), Event::Output(_)));
        drop(pty);
        let deadline = Instant::now() + Duration::from_secs(2);
        let path = format!("/proc/{pid}");
        while Path::new(&path).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let survived = Path::new(&path).exists();
        if survived {
            // Clean up only this test's child, still owned by the waiting worker.
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        assert!(
            !survived,
            "closing retained a live child and its reaper thread"
        );
    }

    fn test_size() -> PtySize {
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    #[test]
    fn regression_input_queue_rejects_whole_pastes() {
        let queue = InputQueue::default();
        queue.push(b"existing input").unwrap();
        assert_eq!(
            queue.push(&vec![b'x'; MAX_INPUT_BYTES]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(queue.take().unwrap(), b"existing input");
        queue.close();
        assert!(queue.take().is_none());
        assert_eq!(
            queue.push(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn regression_stalled_slave_does_not_block_input() {
        let argv = [
            "sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; exec sleep 30".into(),
        ];
        let (mut pty, events) = spawn(&argv, &std::env::temp_dir(), &[], test_size()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut output = Vec::new();
        while !output.windows(5).any(|w| w == b"READY") && Instant::now() < deadline {
            if let Ok(Event::Output(bytes)) = events.try_recv() {
                output.extend(bytes);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            output.windows(5).any(|w| w == b"READY"),
            "slave must stop reading before the paste"
        );
        let mut killer = pty.killer.clone_killer();
        let input = pty.input.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = pty.write(&vec![b'x'; MAX_INPUT_BYTES]);
            let _ = sender.send(result);
            let _ = released.recv_timeout(Duration::from_secs(3));
            // Drop must cancel the writer even when the slave never drains input.
        });
        let result = receiver.recv_timeout(Duration::from_secs(2));
        if result.is_ok() {
            // Let the writer reach the full kernel buffer before closing.
            let deadline = Instant::now() + Duration::from_secs(1);
            while input.state.lock().unwrap().bytes.len() == MAX_INPUT_BYTES
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
        } else {
            let _ = killer.kill();
        }
        let pending = input.state.lock().unwrap().bytes.len();
        let _ = release.send(());
        worker.join().unwrap();
        result
            .expect("input must be queued without waiting for the slave")
            .unwrap();
        assert!(
            pending > 0 && pending < MAX_INPUT_BYTES,
            "writer must encounter a stalled slave"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while !events.is_closed() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            events.is_closed(),
            "dropping a stalled PTY must stop its workers"
        );
    }

    #[test]
    fn regression_async_input_preserves_bytes_and_order_under_backpressure() {
        let argv = [
            "sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; exec cat".into(),
        ];
        let (mut pty, events) = spawn(&argv, &std::env::temp_dir(), &[], test_size()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut output = Vec::new();
        while output.len() < 5 && Instant::now() < deadline {
            if let Ok(Event::Output(bytes)) = events.try_recv() {
                output.extend(bytes);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(output, b"READY");
        let input: Vec<u8> = (0..=255).cycle().take(1024 * 1024).collect();
        for chunk in input.chunks(256 * 1024) {
            pty.write(chunk).unwrap();
        }
        output.clear();
        let deadline = Instant::now() + Duration::from_secs(5);
        while output.len() < input.len() && Instant::now() < deadline {
            if let Ok(Event::Output(bytes)) = events.try_recv() {
                output.extend(bytes);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(output.len(), input.len(), "all accepted input must arrive");
        assert!(
            output == input,
            "partial nonblocking writes must preserve byte order"
        );
    }

    #[test]
    fn regression_output_queue_is_bounded_and_shutdown_cancels_backpressure() {
        let argv = [
            "sh".into(),
            "-c".into(),
            "while :; do printf '%4096s' x; done".into(),
        ];
        let (pty, events) = spawn(&argv, &std::env::temp_dir(), &[], test_size()).unwrap();
        assert_eq!(events.capacity(), Some(OUTPUT_QUEUE_SIZE));
        let deadline = Instant::now() + Duration::from_secs(3);
        while events.len() < OUTPUT_QUEUE_SIZE && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            events.len(),
            OUTPUT_QUEUE_SIZE,
            "exercise a saturated output queue"
        );
        drop(pty);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !events.is_closed() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            events.is_closed(),
            "reader must stop even while its output queue is full"
        );
    }

    #[test]
    fn sees_what_runs_in_the_foreground() {
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let argv = ["sh".to_string()];
        let (mut pty, _events) = spawn(&argv, &std::env::temp_dir(), &[], size).unwrap();
        let shell = pty.pid.unwrap();
        let wait_for = |want: Option<&str>| {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                if foreground_program(shell).as_deref() == want {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(wait_for(None), "an idle shell isn't a running program");
        pty.write(b"sleep 5\n").unwrap();
        assert!(wait_for(Some("sleep")));
        pty.write(b"\x03").unwrap();
        assert!(wait_for(None), "back at the prompt after Ctrl+C");
    }

    #[test]
    fn sees_a_shells_current_folder() {
        let argv = ["sh".to_string()];
        let (mut pty, _events) = spawn(&argv, &std::env::temp_dir(), &[], test_size()).unwrap();
        let shell = pty.pid.unwrap();
        let wait_for = |want: &Path| {
            // The real path: on macOS `$TMPDIR` lies behind the /var symlink.
            let want = std::fs::canonicalize(want).unwrap();
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                if crate::platform::current().process_cwd(shell) == Some(want.clone()) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(wait_for(&std::env::temp_dir()), "starts where it was told");
        pty.write(b"cd /\n").unwrap();
        assert!(wait_for(Path::new("/")), "follows cd");
    }

    #[test]
    fn sees_what_a_wrapper_runs() {
        let argv = ["sh".to_string()];
        let (mut pty, _events) = spawn(&argv, &std::env::temp_dir(), &[], test_size()).unwrap();
        let shell = pty.pid.unwrap();
        // A wrapper leads the foreground group; what it starts joins it.
        pty.write(b"sh -c 'sleep 5 & wait'\n").unwrap();
        let start = Instant::now();
        let mut programs = Vec::new();
        while start.elapsed() < Duration::from_secs(3) {
            programs = foreground_programs(shell);
            if programs.iter().any(|p| p == "sleep") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // The wrapper (`sh`, or bash as on macOS) first, then what it runs.
        assert_eq!(programs.len(), 2, "{programs:?}");
        assert_eq!(programs[1], "sleep");
        pty.write(b"\x03").unwrap();
    }
}
