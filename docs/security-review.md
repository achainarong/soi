# Security review: TASK-0001

This targeted source review covered terminal links, clipboard pastes, Kitty
image decoding, agent reports, and task/Git command execution. It is not a
complete security audit or a dependency-advisory scan.

## Fixes

- **Launcher links:** file-link validation now checks both the requested path
  and its canonical target. A symlink named `readme.txt` cannot hide a
  `.desktop` launcher, and `.app` bundles are rejected even though they are
  directories. Launcher extensions are case-insensitive. File URIs with raw
  queries/fragments or decoded control characters are rejected to avoid
  validating a path different from the launcher's interpretation. Percent-encoded
  literal `?` and `#` in filenames remain supported.
- **Clipboard command injection:** pasted text loses control characters except
  tabs and line endings before previewing, broadcasting, or sending it. Removing
  just the exact bracketed-paste end marker once was insufficient: nested
  markers could leave a new end marker after replacement. Stripping ESC also
  blocks other line-editor escape commands; stripping other controls blocks
  pasted control-key commands. Unicode text, tabs, and ordinary multiline paste
  remain supported, with the existing multiline confirmation behavior.
- **Image allocation limits:** raw RGB/RGBA and PNG images are checked against
  the final RGBA byte limit before allocating/converting their pixels. Previously
  grayscale/RGB PNGs could fit the decoder buffer limit but exceed the RGBA
  limit during expansion. Chunk payload limits are checked before copying,
  including initial and unchunked requests.

Regression tests exercise disguised launchers, ambiguous file URIs, nested paste
markers, line-editor controls, normal Unicode paste, oversized raw images, and
oversized grayscale PNG headers without allocating huge test images.

## Remaining boundaries

- Opening a local file still invokes the desktop's registered handler after an
  explicit modified click. This is not a sandbox, and a file may change between
  validation and launch. Do not treat approved links as safe document contents.
- Kitty file transmission still supports raw pixels from regular local files,
  as before; pathname/regular-file checks are not a content confidentiality
  boundary. A policy that disallows terminal-output-initiated local reads would
  require restricting that protocol feature.
- Agent reports are trusted local-user IPC, not a boundary against another
  process already running as the same user.
- macOS-specific launcher behavior was not exercised on macOS in this review.

## Validation

- `cargo test`: 170 passed, 5 ignored.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo fmt --check` and `git diff --check`: passed.
- Packaging Python tests: 11 passed.
- GUI suite: 31 passed, 1 failed. The sidebar inline-renaming check timed out
  and failed its `editor.is_editing()` assertion, including on an isolated
  rerun. Its cause was not established in this task; the GUI suite is not green.
- `cargo-audit` was not installed; no dependency-advisory result is claimed.

## Follow-up: performance changes (0.1.26–0.1.28)

Reviewed the changes that moved work off the GTK thread: the `/proc`
descendant walk, the agent check on a worker thread, the state-file writer
thread, gradual output saves, git polling intervals and session dirty marks.
No dependencies were added.

### Fixed

- **Unbounded write queue.** Saves were queued on the writer thread with a
  full copy of their content each. With a stalled disk (hung network mount,
  slow fsync) the session and every terminal's output kept piling up in
  memory. `dirs::save_later` now keeps only the latest pending content per
  file, so at most one copy of each waits.
- **Thread count in the process walk.** `MAX_DESCENDANTS` bounded the
  processes visited, not their threads, and each thread's `children` file is
  read. A program with very many threads now uses up the same budget.
- **Saved output readable by other users** (older than these changes).
  Saved terminal output and the session, which can hold whatever a terminal
  showed, were created `0666` less the umask (typically `0644`) in `0755`
  folders: only a private `~/.local/state` kept other local users out. They
  are now created `0600` in `0700` folders, and at start soi tightens those
  saved by older versions. Symlinks are left alone.

### Checked, no change

- The walk can't loop: a process tree has no cycles, and PID reuse during a
  walk is cut off by the budget. Pending PIDs are bounded by the system's
  process count.
- Program names read from `/proc` (any program can choose its own) reach
  only plain labels, not Pango markup, as before.
- The agent check applies a result only if the terminal still has the PID it
  was asked about; soi reaps its own shells, so that PID can't be reused
  meanwhile.
- `flush()` is never called from a writer job, so it can't wait on itself. A
  panicking job ends the writer thread; later saves then run on the calling
  thread and `flush` returns, rather than hanging.
- Paths are fixed when a save is queued; `write_atomic`'s symlink and
  permission handling is unchanged.

### Remaining boundaries

- A hard crash (abort, SIGKILL, power loss) loses saves still queued, at most
  about a second's worth, as well as changes not yet saved.
