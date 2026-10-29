# soi

A terminal workspace manager for Linux and macOS, in the spirit of
cmux: many terminals, organized in
workspaces, groups, splits, tabs and windows, all of which come back after a
restart. Written in Rust with GTK 4, with its own terminal engine.

*Soi* (ซอย) is Thai for the small lanes that branch off a main road, as
your workspaces and panes branch off the sidebar.

## Quick install

On Linux (amd64, arm64) or macOS (Apple Silicon), paste this into a
terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/achainarong/soi/main/scripts/install.sh | sh
```

It downloads the latest [release](https://github.com/achainarong/soi/releases)
(GTK is inside, nothing else to install) and puts it in place for you, no
sudo needed:

- **Linux**: the AppImage becomes `~/.local/bin/soi`, with an app menu entry
  and icon. It needs glibc 2.39 or newer (Ubuntu 24.04, Debian 13,
  Fedora 40 and newer, Arch).
- **macOS**: `soi.app` goes to `~/Applications`, with the quarantine flag
  removed (soi isn't notarized), and `~/.local/bin/soi` links to it.

Then:

1. Start soi from your app menu or Launchpad, or run `soi` (make sure
   `~/.local/bin` is on your `PATH`; the script tells you if it isn't).
2. To use Claude Code, OpenCode or Pi with soi, open Settings ▸ Agents, or
   run `soi install-agents`.
3. To update, run the same command again; settings and layouts stay.

Options go before `sh`, e.g. `curl … | SOI_VERSION=v0.1.26 sh`:
`SOI_VERSION` installs a given release, `SOI_BIN_DIR` changes where the
`soi` command goes, `SOI_APP_DIR` (macOS) where `soi.app` goes.

To uninstall, delete `~/.local/bin/soi`, plus
`~/.local/share/applications/dev.soi.Soi.desktop` and
`~/.local/share/icons/hicolor/scalable/apps/dev.soi.Soi.svg` on Linux or
`~/Applications/soi.app` on macOS. Settings and layouts are in
`~/.config/soi` and `~/.local/state/soi`.

Other ways to install are under [Install](#install).

## Features

- **Workspaces** in a sidebar, optionally in collapsible **groups**;
  rearrange them by drag and drop, rename, move them between windows.
  Double-click a workspace or group name to rename it inline; Enter saves,
  Escape cancels. Group arrows expand and collapse their workspaces; a
  group's + opens a new terminal in it.
- **One main tab bar per workspace**. Each tab owns its own **split panes**
  (side by side or stacked), with one terminal per pane. Switching tabs
  restores that tab's layout and focused pane; hidden terminals keep running.
  Select tabs by number, cycle panes in stable layout order, or focus a pane
  directly with directional shortcuts.
- **Drag and drop**: drag a main tab to reorder it or move it to another
  workspace's tab bar (including another window). Drop it at a pane's edge
  to move its whole layout into that split. Drag a pane by its header to
  rearrange splits, or drop it on a main tab bar to make it a tab. Drop on a
  sidebar workspace row to move there as a main tab; hover over a workspace
  or main tab while dragging to reveal its panes. Center drops move the item
  to the destination workspace's main tab bar, never into nested pane tabs.
  Running terminals and output stay intact; edge highlights and insertion
  markers show where the item will land.
- **Several windows**, each with its own sidebar, named and switchable from
  the sidebar. The window switcher (the name above the sidebar) lists open
  windows and **closed windows**: click one to go there or reopen it, and
  use the buttons on its row to rename, close or delete it (the trash
  button asks for a second click). Closing a window keeps it under closed
  windows until you delete it.
- **Layouts persist**: windows, groups, workspaces, splits, tabs and each
  tab's folder are saved continuously. Open windows return on start; closed
  windows stay available in the switcher. Each tab's **output comes back**
  too (its last 2000 lines, with colors), above a fresh shell in its folder.
  **Agents come back too**: a tab that ran an agent resumes it in its
  shell. Claude Code, OpenCode and Pi say which session they're in (with
  soi's integration, `soi install-agents`) and get exactly that one back;
  Codex, or one of them without the integration, continues its latest
  session in the tab's folder. Other programs that were running are not
  restarted.
- **Own terminal engine** with true color, scrollback, re-wrapping on resize,
  colored curly, dotted, dashed and double underlines (e.g. Neovim's
  diagnostics),
  the Kitty keyboard protocol, synchronized output, **images** (Kitty
  graphics protocol, e.g. fastfetch logos, and Sixel, e.g. img2sixel or
  chafa) and **copying from programs**
  (OSC 52: Neovim, tmux, also over SSH; programs can't read the clipboard).
- **Clickable links** (Ctrl+click), with the real target shown first.
- **Browse mode**: move through the output with vim keys, search, select and
  copy, jump between commands.
- **Knows your commands** when the shell marks them (OSC 133): fish does on
  its own, and soi starts bash and zsh with a small script that loads your
  configuration and adds the marks (Settings ▸ Shell integration; not for
  `bash -l`): commands that failed get a red mark at their prompt,
  Ctrl+Shift+Y copies the last command's output, and a command that ran
  for 10 seconds or more (Settings ▸ Agents) says so when it ends while you
  look elsewhere (banner, or a desktop notification while you're in
  another app).
  While a program runs that reports its folder (OSC 7, e.g. a nested
  shell, or tmux when it passes that on), new tabs and saved layouts use
  that folder.
- Programs can **change the colors** (OSC 4, 10, 11, 12 and their resets,
  e.g. base16-shell or themes that follow the time of day), and ask
  for them.
- The cursor **blinks** if you like (Settings ▸ Cursor), or when the
  program asks for it.
- **Progress on the tab** for programs that report it (OSC 9;4, as for
  Windows Terminal or Ghostty), and of all of a workspace's tabs in the
  sidebar.
- **Looks like your terminal**: colors, font and shell come from your
  Konsole or Ghostty settings; or pick a color scheme, terminal font,
  opacity and more per **profile**. The rest of the app (sidebar, tabs,
  menus) uses the system font, or an app font you pick in Settings.
- **Agents panel** for coding agents (Claude Code, OpenCode, Pi, Codex, …):
  every agent in every window with its state (working, needs input, done),
  one click to jump to it. When an agent stops while you look elsewhere,
  soi marks it in the panel, on its tab and workspace, with a counter and
  a banner. While you're in another app, a desktop notification tells you
  instead; click it to go to the agent (Settings ▸ Agents turns it off).
  An agent that asks shows its question in the panel, with a box to
  answer it from there, and each agent shows how long it worked and how
  long it waited for you.
  See [Agents](#agents).
- **Rebindable shortcuts** in Settings ▸ Shortcuts.
- **Task workspaces**: create an isolated Git worktree and branch with a ticket
  (description, acceptance criteria, checklist, issue link), write its Markdown
  plan inside Soi, hand the brief to an agent, and track task review status.
  The sidebar shows branch and dirty state. See [Task workflows](#task-workflows).
- **Agent session bookmarks** with search and explicit resume, a **read-only
  workspace diff inspector**, and a **live Markdown plan preview** beside terminals.
- **Screen readers** (e.g. Orca) can read what's on the terminal: it
  shows itself as a terminal, with its title, text and cursor.
- Asks before closing something that still runs a program, and before
  pasting several lines into a shell that would run them right away.

## Install

New to soi? [Getting started](docs/getting-started.md) walks you from a
release download to starting Claude Code, OpenCode or Pi for a task.

### Download

[Quick install](#quick-install) does these steps for you. By hand:
[Releases](https://github.com/achainarong/soi/releases) have soi with GTK
inside, so you don't need GTK installed:

- **Linux** (amd64 or arm64): `soi-x86_64.AppImage` or `soi-aarch64.AppImage`.
  Make it executable (`chmod +x soi-*.AppImage`) and run it. It needs glibc
  2.39 or newer (Ubuntu 24.04, Debian 13, Fedora 40 and newer, Arch); without
  FUSE, run it with `--appimage-extract-and-run`.
- **macOS** (Apple Silicon): unzip `soi-macos-arm64.zip` and move `soi.app`
  to Applications. It isn't notarized, so the first time, right-click it and
  choose Open, or run `xattr -dr com.apple.quarantine soi.app`.

Settings ▸ Agents sets up Claude Code, OpenCode and Pi for a downloaded soi.

### Build from source

You need Rust (1.92 or newer) and GTK 4.14 or newer with its development
files:

```sh
# Arch, Garuda, Manjaro
sudo pacman -S rust gtk4
# Debian 13, Ubuntu 24.04 and newer (Rust via https://rustup.rs)
sudo apt install build-essential libgtk-4-dev
# Fedora
sudo dnf install rust cargo gtk4-devel
# macOS (Apple Silicon or Intel; install Xcode command-line tools first)
brew install rust gtk4 pkg-config librsvg python
```

Then:

```sh
make install     # builds and installs for your user (~/.local), no sudo
```

soi then appears in your app menu, and Claude Code, OpenCode and Pi (if
you use them) report their state to it (see [Agents](#agents)).
`make uninstall` removes soi again;
settings and layouts stay. To install system-wide instead:
`sudo make install PREFIX=/usr/local`.

On macOS, `make install` creates `~/Applications/soi.app` and a shell command
in `~/.local/bin` (add that directory to your PATH). `APPDIR` changes the bundle
destination; `PREFIX` changes the shell command's prefix. `AGENTS=0` skips
agent integration changes. `make bundle` builds `target/soi.app` without installing.
The bundle includes its non-system libraries, image loaders, GTK schemas and
dependency license notices, and is ad-hoc signed. It can be moved without
Homebrew at runtime; it is not notarized or a universal binary. Public
redistribution still requires complying with the bundled dependencies' licenses.

The bundle's minimum macOS version is computed from the executable and all
bundled libraries, not assumed from the source. For example, Homebrew GTK built
for macOS 27 makes the bundle require macOS 27. Build with compatible dependencies
on the intended oldest system for older macOS releases. See
[macOS validation and limitations](docs/macos-port.md).

| Command      | What it does |
|--------------|--------------|
| `make`       | build an optimized binary (`target/release/soi`) |
| `make run`   | build and start it |
| `make dev`   | start a debug build next to an installed soi, with its own layout |
| `make test`  | run the tests |
| `make test-gui` | run the display-dependent GUI checks (requires a desktop and bash) |
| `make bundle` | macOS: build a relocatable app bundle without installing |
| `make appimage` | Linux: build `target/soi-<arch>.AppImage` with GTK inside (downloads [linuxdeploy](https://github.com/linuxdeploy/linuxdeploy)) |

Pushing a tag such as `v0.2.0` builds soi 0.2.0 for Linux (amd64, arm64)
and macOS (Apple Silicon) on GitHub and publishes them as a release; the
tag sets the version. Changes to the packaging build them without a release.

The GUI suite uses a dedicated main-thread executable because macOS rejects
GTK initialization on Rust's normal test worker threads. Each check gets a
temporary home, configuration, state and socket directory; it does not replace
your session or agent settings. To select checks:

```sh
cargo test --features gui-tests --test gui -- --list
cargo test --features gui-tests --test gui -- --exact bash_marks_its_commands
python3 -B -m unittest discover -s scripts -p 'test_*.py'  # packaging regressions
```

## Default shortcuts

All of these can be changed in **Settings ▸ Shortcuts**. The table below shows
Linux defaults. On macOS, Ctrl stays available to shell programs; defaults use
Cmd instead, with these common bindings:

| macOS keys | Action |
|------------|--------|
| `Cmd+T` / `Cmd+W` / `Cmd+Shift+T` | New / close / reopen tab or workspace |
| `Cmd+1` … `Cmd+9` | Select main tab 1–9, left to right |
| `Cmd+Shift+]` / `Cmd+Shift+[` | Next / previous main tab |
| `Cmd+Option+]` / `Cmd+Option+[` | Next / previous split pane |
| `Cmd+Shift+W` | Close the focused split pane |
| `Cmd+D` / `Cmd+Shift+D` | Split right / down |
| `Cmd+Option+Arrows` | Focus a pane |
| `Cmd+Shift+Option+Arrows` | Move the divider |
| `Cmd+Shift+Enter` / `Cmd+Shift+B` | Zoom pane / broadcast input |
| `Cmd+Shift+A` / `Cmd+Option+1` … `Cmd+Option+9` | New workspace / workspace in current group |
| `Cmd+Ctrl+Down` / `Cmd+Ctrl+Up` | Next / previous workspace within the current group |
| `Cmd+Ctrl+Shift+Down` / `Cmd+Ctrl+Shift+Up` | Next / previous group |
| `Cmd+P` / `Cmd+N` | Quick switcher / new window |
| `Cmd+Enter` or `Ctrl+Option+Esc` | Window fills the screen (not fullscreen), and back |
| `Ctrl+Option+Arrows` | Window to the left / right / top / bottom half of the screen |
| `Cmd+C` / `Cmd+V` / `Cmd+F` | Copy / paste / search |
| `Cmd+Shift+Y` / `Cmd+Shift+Space` | Copy last output / browse mode |
| `Cmd+Plus` / `Cmd+Minus` / `Cmd+0` | Bigger / smaller / normal text (kept after a restart) |
| `Cmd+Shift+G` / `Cmd+Shift+J` | Agents panel / next agent |
| `Cmd+Shift+I` / `Cmd+Shift+M` | Task details / plan preview |
| `Cmd+,` / `Cmd+Q` | Settings / quit |

`F2` renames on both platforms. On macOS, use Cmd+click to open terminal links.

If Rectangle or a similar tool uses Ctrl+Option+Arrows too, it takes them
first and can't move soi's windows; add soi to its ignored apps (Rectangle:
"Ignore Soi" in its menu). On Linux, your window manager snaps windows
(Super+Arrows in GNOME and KDE).

| Keys | Action |
|------|--------|
| `Ctrl+Shift+T` / `Ctrl+Shift+W` | New tab / close tab |
| `Ctrl+Shift+Z` | Reopen the tab or workspace closed last, with its folders and output (also after a restart) |
| `Alt+1` … `Alt+9` | Select main tab 1–9, left to right |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous main tab |
| `Ctrl+Shift+]` / `Ctrl+Shift+[` | Next / previous split pane |
| `Ctrl+Alt+W` | Close the focused split pane |
| `Ctrl+Shift+E` / `Ctrl+Shift+O` | Split right / split down |
| `Ctrl+Shift+Arrows` | Focus the pane in that direction |
| `Ctrl+Shift+Alt+Arrows` | Move the divider next to the pane that way |
| `Ctrl+Shift+Enter` | Zoom the pane to fill the workspace, and back |
| `Ctrl+Shift+B` | Type into all panes of the current main tab at once (their borders turn red), and back |
| `Ctrl+Shift+A` | New workspace |
| `Ctrl+Alt+1` … `Ctrl+Alt+9` | Go to workspace 1–9 of the current group (ungrouped workspaces count as one group) |
| `Ctrl+PgUp/PgDn` | Previous / next workspace within the current group |
| `Ctrl+Shift+PgUp/PgDn` | Previous / next group, back to the workspace you were on there |
| `F2` | Rename workspace |
| `Ctrl+Shift+P` | Quick switcher: type to go to any workspace or agent |
| `Ctrl+Shift+N` | New window |
| `Alt+F10` | Window fills the screen (not fullscreen), and back |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / paste |
| `Ctrl+Shift+F` | Search |
| `Ctrl+Shift+Y` | Copy the last command's output |
| `Ctrl+Shift+Space` | Browse mode (vim keys; `q` leaves) |
| `Ctrl+Plus` / `Ctrl+Minus` / `Ctrl+0` | Bigger / smaller / normal text (kept after a restart) |
| `Ctrl+Shift+G` | Show / hide the agents panel |
| `Ctrl+Shift+J` | Go to the next agent that needs you |
| `Ctrl+Shift+I` | Open the current task's details |
| `Ctrl+Shift+M` | Show / hide the plan beside the terminal |
| `Ctrl+,` | Settings |

Workspace, main-tab and pane cycling wrap at either end. Pane order follows
left before right and top before bottom through the split layout. Numbered
selection is exact: a missing number does nothing; tabs beyond nine remain
reachable by cycling or clicking. Closing a main tab closes its entire split
layout (with confirmation when programs are running); closing the last pane
closes that tab. Closing the final tab leaves a fresh terminal in the workspace.
Zoom and broadcast input apply only to the selected main tab. Tasks and plans
remain attached to the workspace.

**Session format change:** workspace-level tabs start a new session format;
old pane-tab sessions are not migrated. Settings and custom shortcuts remain.

In the terminal: double-click selects a word, triple-click a line,
Ctrl+click opens a link, Shift+PgUp/PgDn scrolls.

## Agents

Tabs running a known agent program (`claude`, `opencode`, `pi`, `codex`, `aider`,
`gemini`, … — editable in Settings ▸ Agents) show up in the agents panel
(◨ in the sidebar, or Ctrl+Shift+G). An agent started through a wrapper,
such as `mate opencode`, counts too: soi looks at every program in the tab's
foreground, not just the one leading it.

On its own, soi sees whether an agent is busy (printing output) or quiet,
and treats a bell or a notification (OSC 9 / 777) as "needs input". For
exact states, let the agent report them. `make install` sets this up for
the agents configured on your machine (`AGENTS=0` skips it); so do
`soi install-agents` and Settings ▸ Agents, with one click each.

- **Claude Code**: hooks in `~/.claude/settings.json` that run
  `soi claude-hook` on `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
  `Notification` and `Stop`. By hand, add for each of these events:

  ```json
  { "hooks": [{ "type": "command",
    "command": "[ -z \"$SOI_BIN\" ] || \"$SOI_BIN\" claude-hook # soi-agent-integration" }] }
  ```
- **OpenCode**: a plugin, `~/.config/opencode/plugins/soi.js`, that sends
  OpenCode's busy, permission, question, idle and error events, with the ID
  of the session you work in (not its subagents').
- **Pi**: an extension, `~/.pi/agent/extensions/soi.ts` (under
  `$PI_CODING_AGENT_DIR` when set), that sends when a run starts and settles,
  when Pi waits on a prompt (a confirm or a question from an extension), and
  a run's final error, with the ID of the session (not of `--no-session` ones).
- **Anything else**: run `soi status working|input|done [message]` from
  inside a tab, e.g. from your own agent's hooks or at the end of a script.

Outside soi, all of these do nothing. Each tab's shell gets
`SOI_TAB_ID`, `SOI_SOCKET` (`$XDG_RUNTIME_DIR/soi.sock` on Linux,
`$TMPDIR/soi.sock` on macOS, readable only by you) and `SOI_BIN`;
reports are JSON lines on that socket:
`{"tab": "<SOI_TAB_ID>", "state": "input", "message": "Allow edit?"}`.

Integrations may also attach `"session": {"provider": "claude", "id": "SESSION_ID"}`
to bookmark the provider's session for the reporting tab. Session IDs are
validated; an unknown provider is saved but cannot launch a resume command.

### Agents to start

**Settings ▸ Agents ▸ Agents to start** sets up what the agent dropdowns
in Task details and New task worktree offer: a name, the tool (Claude Code,
OpenCode or Pi), a model, a variant, a wrapper to start it through and more
arguments. Add a tool more than once to have it with different models, or
once plain and once through a wrapper. Under each one is the command it
starts, for example:

| Agent | Starts |
|-------|--------|
| Claude Code, model `opus`, effort `high` | `claude --model opus --effort high '<brief>'` |
| OpenCode, model `openrouter/qwen/qwen3-coder` | `opencode --model openrouter/qwen/qwen3-coder --prompt '<brief>'` |
| OpenCode, model `openai/gpt-6.1-sol`, arguments `--agent sol-high` | `opencode --model openai/gpt-6.1-sol --prompt '<brief>' --agent sol-high` |
| Pi, model `anthropic/claude-opus-5-5`, variant `high` | `pi --model anthropic/claude-opus-5-5 --thinking high '<brief>'` |
| Claude Code, model `opus`, start through `mate`, arguments `-- --no-git` | `mate claude --model opus '<brief>' -- --no-git` |

Models are passed as you write them: `opencode models` lists OpenCode's
(as provider/model); Claude Code takes an alias like `opus` or a full name;
`pi --list-models` lists Pi's, and Pi's variant is its thinking level (`off`
… `max`).
OpenCode's terminal app has no variant option, so for one, define an
OpenCode agent with that model and variant and pass `--agent <name>`.

OpenCode 2 (so far) takes neither `--model` nor `--agent` in its terminal app
and quits when given them
([anomalyco/opencode#56218](https://github.com/anomalyco/opencode/issues/56218)).
When the installed `opencode` is 2.x, soi leaves both out (an `--agent` after
`--` is the wrapper's and stays). OpenCode then starts with the model you used
last; switch with `/models`. The preview under each agent shows the full
command, as OpenCode 1.x gets it.

**Start through** puts a command in front of the tool's, for a wrapper that
starts it, like [mate](https://github.com/uniqbit-ag/mate-cli): `mate` gives
`mate claude …` or `mate opencode …`, which pass everything on to the agent;
mate's own options go after `--` in the arguments. soi still sees the tab as
running Claude Code or OpenCode. The wrapper must be on your PATH, or given
with its full path. They are kept in `~/.config/soi/agent-presets.json`.

For agent launches, Soi discovers PATH from the shell configured in Settings.
When that shell is zsh, it uses an **interactive login shell**, loading `.zshrc`
(or `$ZDOTDIR/.zshrc`) as well as login configuration. This lets desktop-launched
Soi find NVM-installed tools such as `mate` and `node`. Other shells keep their
login-shell discovery behavior. Only PATH is imported, not aliases, functions
or other environment variables. Discovery has a three-second timeout and falls
back to Soi's inherited PATH plus `~/.local/bin` if startup fails. Results are
cached separately per configured shell; restart Soi after changing shell PATH
configuration. Normal zsh startup scripts run during discovery, so interactive
setup that requires a real terminal should be guarded appropriately.

### Skill and command collections

**Settings ▸ Collections** keeps sets of skills and commands you request
together, say `tdd` and `/review` for backend work. Click **New
collection**, name it, and click the skills and commands it chooses (the
filter narrows long lists; chosen ones move to the front). The dropdown
switches between collections, the trash button deletes one.
Tick **Default for new tasks** on one to have New task worktree choose it
every time; at most one is the default, and you can still pick another
collection, none, or change the toggles for that task. Only your own skills
and commands are listed there; a repository's show up in the task dialogs.
They are kept in `~/.config/soi/collections.json`. soi comes with none:
collections are yours to make.

A **group** can have its own: right-click its name, **Collection for new
tasks**, and pick one (or **The default collection** again). A new task
worktree in that group, from the group's **New task worktree in group…** or
while one of its workspaces is active, starts with the group's collection
instead of the default, say one for a C++ project and another for Terraform.
Groups keep it with the layout.

## Task workflows

Open **Tasks** in the sidebar toolbar:

- **New task worktree…** creates a new branch from the repository's current
  HEAD and a worktree at a not-yet-existing directory. To start from somewhere
  else, enter a **Base branch**: any branch, tag or commit, such as `develop`
  or `origin/main` (as of your last fetch). The new branch doesn't track it,
  so pushing never goes to the base branch. The branch must not
  exist yet and can't be the main branch (`main`, `master`, or the remote's
  default); the repository needs at least one commit. Relative destinations
  resolve from the repository root (for example `.worktrees/test`); absolute
  paths also work. Leave the destination empty to use
  `.worktrees/<branch>/<repository>`, with the branch made directory-safe
  (`feature/login` becomes `feature-login`). soi warns after
  creation if an in-repository destination is not Git-ignored; it never edits
  `.gitignore` or `.git/info/exclude` for you. Add an optional ticket number
  (like `PROJ-123`), issue link, description and checklist (one item per
  line). By default soi creates the plan in the task's folder (see below),
  seeded from those details; you can choose another path (relative to the
  worktree, or absolute), link a plan that already exists, or skip
  it. An existing file is linked, never overwritten. Optionally start one of
  your agents (see [Agents to start](#agents-to-start)) in a new tab, change
  its model for this start, and tick **Give the agent the task brief** to
  pass the brief as its first prompt. Click the **skills** and **commands**
  you want the agent to use (several if you like; hover one for its
  description), or choose a **collection** of them (see
  [Skill and command collections](#skill-and-command-collections)); the
  group's collection, or else the default collection, is chosen to begin
  with. soi lists skills in
  `~/.claude/skills`, `~/.agents/skills`, `~/.codex/skills`, OpenCode's and
  Pi's skill folders, and commands in `~/.claude/commands` (a subfolder
  makes `folder:name`), `~/.codex/prompts`, OpenCode's commands and Pi's
  prompts; also those of installed Claude Code plugins (as `plugin:name`)
  and the repository's own (`.claude/skills`, `.agents/skills`,
  `.claude/commands`, …). The brief names them, with where each file is.
  The tools must already be installed and configured. Uncommitted changes are
  not copied to the worktree.
- **Task folders** keep each task's plan and history on your machine, outside
  the repository and next to soi's layouts and sessions:
  `~/.local/state/soi/plans/<project>/<prefix>-<task>/` (under
  `$XDG_STATE_HOME` if set; the same on macOS), with `plan.md` and
  `metadata.md`. `<project>` is the main checkout's folder name, so all of
  its worktrees share it; a `.repository` file there names the checkout, and
  another project with the same name gets `<name>-<hash>` instead. `<prefix>`
  is the ticket number, or a short id without one (the same ticket and task
  name find the same folder again, whose plan is linked, never overwritten).
  `metadata.md` starts with what the task is now (id, ticket, issue, status,
  repository, branch, plan, requested skills and commands, when it was
  created), which soi rewrites, and a **History** that it only adds to:
  created (with the skills and commands requested, and the collection they
  came from), renamed, status, ticket, issue, branch, plan, skills and
  commands changes, checklist items ticked, edits, agents started, and
  the worktree removed. Removing a worktree or its branch never touches the
  folder. The brief tells agents where the history and the project's earlier
  tasks are, and agents soi starts may read and edit the project's folder
  without asking you: Claude Code with `--add-dir <folder>` and
  `--allowedTools "Edit(/<folder>/**)"`, OpenCode with
  `permission.external_directory` set to `allow` for it (through
  `OPENCODE_CONFIG_CONTENT`, on top of your own configuration). Pi has no
  permission prompts, so it needs nothing. Elsewhere
  outside the worktree they still ask. Tasks from before keep their plan where it is
  and get a folder for their history the next time they're saved.
- **New task in this workspace…** (in a workspace's sidebar menu) makes
  any workspace a task in its own checkout: no worktree or branch is
  created, so there is no ⎇ mark and nothing for **Remove worktree…** to
  delete. It opens Task details for a new task named after the workspace;
  **Save** attaches it, and from then on the workspace works like a task
  worktree (plan, brief, agents, history). Agents work in that checkout, so
  tasks sharing one also share its changes. A workspace that has a task
  shows **Task details…** there instead.
- **Task details…** opens one window per workspace with four tabs
  (**Alt+1**–**Alt+4**):
  - **Details**: name, status (Active, Blocked, Ready for review, Finished), ticket number,
    issue link (a reference only; nothing is imported or fetched),
    description, acceptance criteria, a checklist, important files and the
    skills and commands to request (choosing a collection replaces them with
    its own).
    Relative paths resolve inside the workspace. Agent activity never changes
    task status. Every agent in the workspace inherits its task; the agents
    panel filters activity and task status independently.
  - **Plan**: create, link or unlink the plan, edit it and preview the
    rendered Markdown. The tab shows where the file is saved and whether Git
    tracks it, ignores it, or would pick it up (add `.soi/` to
    `.git/info/exclude` to keep plans local). The plan is reread every second.
    With no unsaved edits, outside changes (an agent's, say) load
    automatically. With unsaved edits soi asks whether to load the disk version
    or keep yours. Saving is atomic and refuses to replace a version you
    haven't seen. Unlinking never deletes the file.
  - **Agents**: linking a plan does **not** send it anywhere. **Copy brief**
    puts a Markdown brief (task, ticket, checklist, skills, commands and plan path) on the
    clipboard to paste into any agent. Pick one of your agents (and change
    its model for this start if you like) and **Start with brief** to open a
    new tab running it with the brief as its first prompt.
    Running terminals are never typed into. Below are the linked sessions,
    which you can resume, plus bookmarks and history.
  - **Changes**: workspace-wide `git status`, with a link to the full diff.

  Ticket fields are saved with the session; the plan is an ordinary file in
  the worktree, so agents can read and edit it. **Save** (**Ctrl+S**) writes
  both. Closing with unsaved edits (including with **Escape**) asks whether to
  save, discard or keep editing.
- **Session history…** searches local session bookmarks by title, provider,
  ID and directory. The Claude hooks, the OpenCode plugin and the Pi extension bookmark their
  session IDs automatically, linked to the tab's task; use **Bookmark
  session…** for Codex and other providers. Resume
  launches `claude --resume ID`, `opencode --session ID`, `pi --session ID`,
  or `codex resume ID`
  directly in a new terminal tab at the recorded directory. No shell input is
  injected and no agent permission settings are changed. Missing directories,
  invalid IDs and unsupported providers cannot be resumed. Conversations remain
  in provider-owned storage; soi stores metadata, not transcripts. Provider
  CLI versions must support these commands.
- **Inspect changes…** shows workspace-wide status and staged/unstaged text
  diffs, with manual refresh. Changes are not attributed to one agent in a
  shared checkout. Untracked files are listed without loading their contents;
  binary changes are summarized by Git. There are no staging, discard, merge,
  or commit actions. Git output is bounded and commands time out.
- **Toggle plan preview** (**Ctrl+Shift+M**) opens the plan beside the
  terminal; drag the divider between them to make it wider or narrower, and
  it reopens at that width. It is a live
  Markdown editor: you type Markdown, and headings, emphasis, code, links,
  quotes and lists are styled as you go, with their markup shown faintly.
  Click a task's `[ ]` to tick it. Enter continues a list (the next bullet,
  number or checkbox) and ends it on an empty item; Tab and Shift+Tab indent
  list items; **Ctrl+B**, **Ctrl+I** and **Ctrl+K** make the selection bold,
  italic or a link. The plan saves itself shortly after you stop typing
  (**Ctrl+S** saves now) and when the panel closes. Outside changes (an
  agent's, say) load automatically while you have no unsaved edits; otherwise
  soi offers **Load theirs** or **Keep mine** rather than overwriting. HTML is
  shown as text, and links/images never execute or fetch resources. Plans are
  limited to 512 KiB. Edit plans here, in Task details or in any editor.

The quick switcher (**Ctrl+Shift+P**) also searches task names and saved session
metadata. Choosing a saved session explicitly resumes it; restored layouts
never restart agents automatically.

Task worktrees are marked ⎇ in the sidebar. Closing workspaces or marking
tasks finished **never deletes worktrees or branches**. To delete one, choose
**Remove worktree…** in the workspace's sidebar menu. soi first says what
would be lost: uncommitted changes, the plan if it lives in the worktree,
running programs, and commits on the branch that are on no other branch or
remote. The branch is deleted only when you ask and Git considers it merged
(`git branch -d`); a branch with such commits is always kept. The repository's
own checkout is never removed. A failed worktree operation may leave partial
state; soi reports the error and does not attempt destructive rollback.

Task context persists with the existing layout. Session history lives in
`~/.local/state/soi/agent-sessions.json` with user-only permissions. Dev mode
uses `agent-sessions-dev.json`, separate from normal history. If history is
corrupt, writes fail without replacing it.

### macOS rendering troubleshooting

GTK's macOS backend shows a window as tiles and updates only those that
changed. Some tiles can then keep an old frame: stale or double-size pieces
of panes, or the desktop showing through, especially with background opacity
below 1. soi therefore starts with `GSK_DEBUG=full-redraw`, so every frame
updates the whole window. Programs started in soi's terminals don't inherit
it. If you set `GSK_DEBUG` yourself, soi leaves it alone; `GSK_DEBUG= soi`
turns the workaround off for comparison.

If sidebar text or icons become stretched or corrupted, fully quit soi and
try the software renderer for comparison:

```sh
GSK_RENDERER=cairo make dev
# Or, for an installed command:
GSK_RENDERER=cairo soi
```

This bypasses GTK's GPU renderer and may be slower. It does not change font,
zoom, layout or theme settings. Quit the existing instance first: starting
another instance normally activates the already-running process instead of
changing its renderer. The default renderer has not been changed globally.

With GTK 4.24.1, macOS may also print
`gdk_frame_timings_presented() called on skipped frame`. This was reproduced
with a minimal GTK window without Soi code. It was non-fatal in local checks;
switching renderers is not guaranteed to resolve it. Soi does not suppress
GTK diagnostics or patch system libraries.

## Files

These XDG paths also apply on macOS; Soi does not use `~/Library/Application Support`
for its own settings. `$XDG_CONFIG_HOME` and `$XDG_STATE_HOME` override the defaults.

| Path | Contents |
|------|----------|
| `~/.local/state/soi/session.json` | Windows and layouts |
| `~/.local/state/soi/agent-sessions.json` | Agent session bookmarks |
| `~/.local/state/soi/scrollback/` | Each tab's saved output |
| `~/.local/state/soi/soi.log` | Errors and crash reports |
| `~/.config/soi/config` | The active profile |
| `~/.config/soi/profiles/<name>.conf` | A profile's settings |
| `~/.config/soi/keybindings.conf` | Changed shortcuts |
| `~/.config/soi/agent-presets.json` | Agents to start |
| `~/.config/soi/collections.json` | Skill and command collections |

Shells started by soi have `TERM_PROGRAM=soi` set, so shell configs can
tell (e.g. to pick a fastfetch logo type).

## How it's built

- `src/app/`: the user interface, a folder or file per feature.
  - `mod.rs`: the shared state, starting and quitting
  - `window.rs`: windows and the saved windows of the window switcher
  - `persist.rs`: saving and restoring layouts and output
  - `actions.rs`: actions and their shortcuts
  - `panes/`: terminal panes, splitting and focus
  - `workspace_tabs.rs`: the main tab bar, per-tab layouts and pane cycling
  - `workspaces/`: workspaces and their groups
  - `sidebar/`: the sidebar's rows, menu, renaming and drag and drop
  - `settings_window/`: applying settings, and the Settings window
  - `switcher.rs`: the quick switcher
  - `panel.rs`: the agents panel, its alerts and the socket agents
    report to
- `src/platform/`: what differs between Linux and macOS (process details,
  runtime directory, default font, where other terminals keep settings),
  behind the `Platform` trait.
- `src/dirs.rs`: soi's config and state directories (XDG, on macOS too).
- `src/agents.rs`: agent states, reports, the `soi status` and
  `soi claude-hook` commands, and the Claude Code / OpenCode / Pi setup.
- `src/term/`: the terminal engine.
  - `screen.rs`: the screen model (grid, scrollback, cursor, modes, re-wrapping)
  - `graphics.rs`: Kitty images
  - `sixel.rs`: Sixel images
  - `boxdraw.rs`: line and block characters, drawn to fill their cells
  - `keys.rs`: key encoding
  - `widget.rs`: drawing and input
  - `area.rs`: the terminal's widget, which GTK draws on the GPU
  - `browse.rs`: browse mode
  - `links.rs`: links
  - `pty.rs`: the shell's pseudo-terminal
- Escape sequences are tokenized by the [`vte`](https://crates.io/crates/vte)
  crate (Alacritty's parser); text is laid out with Pango and drawn by GTK
  on the GPU (render nodes, see `src/term/area.rs`).

Terminal output is treated as untrusted: image files are only read if they
are regular image files outside `/proc`, `/sys`, `/dev` and `/run`, memory
for images is bounded, links only open on Ctrl+click and only for web,
mail and local non-executable files, and the engine is fuzz-tested
(`cargo test --release fuzz_long -- --ignored`).
