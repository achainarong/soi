# Task: what's still open after the last round

soi is a GTK 4 terminal workspace manager written in Rust. Most of the list
that was here is done (box-drawing characters, screen reader updates, the
failed-command gutter, bash and zsh integration, reopening workspaces and
keeping closed things and zoom over a restart, sidebar progress, Sixel
background and cursor placement, named colors, the long-command setting).
Since then soi gained tasks with their own worktrees and plans (a live
Markdown editor beside the terminal), agent sessions named after their
workspace and resumed there, notifications queued under the agents, word
jumps and selection in the terminal, and keyboard maximize and snap.
This is what's left.

## Rules

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
  `cargo test` stay clean. GTK tests are `#[ignore]`d and run in their own
  process (`isolated` in `src/app/window_tests.rs`): GTK can only be started
  once per test process. Their windows open on the desktop, so don't type
  while they run.
- Terminal output is untrusted: anything parsed from it stays bounded, and new
  escape sequences go into the fuzzer's pieces (`src/term/mod.rs`).
- No `cfg(target_os)` outside `src/platform/`.
- Match the surrounding code's style (short doc comments saying why, plain
  names). Commit messages describe the change for a user, like the existing log.

## Open

1. **bash as a login shell** (`bash -l`, the usual case on a Mac) gets no
   shell integration: `--rcfile` doesn't apply to login shells
   (`src/shell_integration.rs`), so there are no failed-command marks, no
   Ctrl+Shift+Y and no long-command notice. kitty handles it with `ENV` and
   `--posix`.
2. **Finish the macOS port.** It runs on Apple Silicon with macOS 27 (see
   `docs/macos-port.md`). Not tried yet: Intel, older macOS, a clean Mac and
   opening from Finder past Gatekeeper. The bundle needs macOS 27 because of
   the Homebrew libraries it's built from, and it's ad-hoc signed, not
   notarized.
3. **Check CI** on Ubuntu 24.04 (GTK 4.14, the oldest soi supports); the
   last rounds were only tested locally.
4. **Inspect changes** only shows status and diffs: no staging, discarding
   or committing, and in a shared checkout changes aren't tied to the agent
   that made them.
5. **Resuming sessions** works for Claude Code, OpenCode and Codex; other
   agents' sessions can be bookmarked but not resumed.

## Looked at and left as they are

- **Trying it with a screen reader** isn't planned. The terminal still
  exposes its text, cursor and title (`src/term/area.rs`) and reports only
  the part that changed.
- **Caching each line's render nodes between frames** was tried and measured
  slower: hashing every cell costs more than building the nodes (1.7 instead
  of 1.3 ms per frame while output scrolls), and the extra nodes slow GTK's
  renderer. Most of a frame's time is GTK rendering it.
- **Typing into all panes** doesn't mirror Shift+PgUp and the like: those
  scroll your own view, not the programs'. A paste is confirmed once, for all
  panes.
- **The agents panel's reply box** keeps its text and focus when rows come and
  go (there's a test for it).
