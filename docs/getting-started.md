# Getting started with soi

This guide takes you from a GitHub release to a soi that starts Claude Code,
OpenCode or Pi for your tasks. The [README](../README.md) has the full
reference.

## 1. Install from a release

Download the latest build from
[Releases](https://github.com/achainarong/soi/releases). It has GTK inside,
so you don't need to install GTK.

### Linux (amd64 or arm64)

```sh
chmod +x soi-x86_64.AppImage     # or soi-aarch64.AppImage
./soi-x86_64.AppImage
```

- You need glibc 2.39 or newer: Ubuntu 24.04, Debian 13, Fedora 40 or a
  newer release, or Arch.
- Without FUSE, run it with `--appimage-extract-and-run`.
- To start it like any other program, move it onto your PATH, for example
  `mv soi-x86_64.AppImage ~/.local/bin/soi`.

### macOS (Apple Silicon)

1. Unzip `soi-macos-arm64.zip` and move `soi.app` to Applications.
2. soi isn't notarized. The first time, right-click it and choose **Open**,
   or run `xattr -dr com.apple.quarantine /Applications/soi.app`.

### Updating

Download the new release and replace the old file. Your settings, layouts
and task plans stay where they are (see [Where things live](#where-things-live)).

## 2. Install the agents themselves

soi starts agents but doesn't ship them. Install the ones you want and log
in to each once in a normal terminal:

| Agent | Program | Check that it works |
|-------|---------|---------------------|
| Claude Code | `claude` | `claude` and log in |
| OpenCode | `opencode` | `opencode`, then `/connect` a provider; `opencode models` lists models |
| Pi | `pi` | `pi`, log in; `pi --list-models` lists models |

soi finds them on the PATH of the shell set in **Settings ▸ General**. It
also looks in `~/.local/bin`. If soi says an agent "isn't installed or isn't on
your PATH", make sure your shell's login configuration adds its folder to
PATH, then restart soi.

## 3. Connect the agents to soi (Settings ▸ Agents ▸ Exact states)

On its own, soi can only tell whether an agent is printing output. To show
**working**, **needs input** and **done** exactly, and to link each agent's
session to its task, every agent needs a small integration:

1. Open Settings (⚙ in the sidebar) ▸ **Agents**.
2. Under **Exact states**, click **Install** next to Claude Code, OpenCode and
   Pi. Skip any agent you don't use.
3. Restart agents that are already running.

What each install writes:

| Agent | File | What it does |
|-------|------|--------------|
| Claude Code | hooks in `~/.claude/settings.json` | Your other settings and hooks stay as they are |
| OpenCode | `~/.config/opencode/plugins/soi.js` | Sends busy, permission, question, idle and error events |
| Pi | `~/.pi/agent/extensions/soi.ts` (under `$PI_CODING_AGENT_DIR` if set) | Sends run, prompt and error events |

Outside soi, all three do nothing. When a newer soi changes them, the button
says **Reinstall**.

## 4. Choose which agents to start (Settings ▸ Agents ▸ Agents to start)

**New task worktree** and **Task details ▸ Start with brief** offer the agents
listed here. Each agent starts in a new tab inside the task's worktree, with
the task brief as its first prompt.

### The defaults

Out of the box, there is one entry per tool, with no model and no options:

| Entry | Starts |
|-------|--------|
| Claude Code | `claude '<brief>'` |
| OpenCode | `opencode --prompt '<brief>'` |
| Pi | `pi '<brief>'` |

With no model set, each agent uses **its own default model**. In practice:

- **Claude Code** uses the model from its own settings (`/model` inside
  Claude Code).
- **OpenCode** uses the model you used last (`/models` inside OpenCode).
- **Pi** uses `defaultProvider` and `defaultModel` from
  `~/.pi/agent/settings.json` (choose one with `/model` inside Pi).

**This is the setup we recommend.** You choose the model once in each agent,
and soi follows it. You don't need to change anything in soi.

> If an agent fails with an error like *"model not supported"*, its own
> default model is wrong, not soi. Fix the default in the agent (for example
> `/model` in Pi), or give the soi entry a model (see below).

### When to change the entries

Edit, add or remove entries under **Agents to start** if you want:

- **A fixed model for tasks**, separate from the agent's default.
- **Several models per tool**, e.g. *Claude Opus* and *Claude Sonnet* side by
  side. Click **Add agent**, then choose the same tool again.
- **A stronger or weaker reasoning level** (the *variant* field).
- **A wrapper**, such as `mate`, that starts the agent for you (**Start
  through**).
- **Extra arguments** for the agent.

Each entry has these fields:

| Field | Claude Code | OpenCode | Pi |
|-------|-------------|----------|----|
| Model | alias or full name: `opus`, `sonnet` | `provider/model`, from `opencode models` | `provider/model`, from `pi --list-models` |
| Variant | effort: `low` … `max` | not supported: use `--agent <name>` (see below) | thinking level: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| Start through | e.g. `mate` | e.g. `mate` | e.g. `mate` |
| More arguments | e.g. `--permission-mode plan` | e.g. `--agent plan` | any `pi` option |

The command an entry starts is shown under it, so you can check it before
you use it. The entries are saved in `~/.config/soi/agent-presets.json`.

### Example setups

| You want | Tool | Model | Variant | More arguments |
|----------|------|-------|---------|----------------|
| Claude for hard tasks | Claude Code | `opus` | `high` | |
| Claude for quick tasks | Claude Code | `sonnet` | | |
| Pi with an OpenAI model | Pi | `openai/gpt-6.1-sol` | `medium` | |
| OpenCode with a reasoning variant | OpenCode | `openai/gpt-6.1-sol` | | `--agent sol-high` |

For the last row, define an OpenCode agent named `sol-high` that sets the
model and variant in your OpenCode configuration. OpenCode's terminal app has
no variant option of its own.

### OpenCode 2

OpenCode 2 (so far) quits when given `--model` or `--agent`. When the
installed `opencode` is 2.x, soi leaves both out, and OpenCode starts with the
model you used last. Switch with `/models`.

### Permissions in a task

A task keeps its plan and history in `~/.local/state/soi/plans/…`, outside the
worktree. soi lets each agent use that folder without asking:

- **Claude Code** gets `--add-dir <folder>` and edit permission for it.
- **OpenCode** gets permission for it through `OPENCODE_CONFIG_CONTENT`, on
  top of your own configuration.
- **Pi** has no permission prompts, so it needs nothing.

Anywhere else outside the worktree, the agents still ask as usual.

## 5. Your first task

1. Click **Tasks ▸ New task worktree…** in the sidebar.
2. Fill in a name, a new branch and the repository.
3. Under **Optional agent**, choose one of your agents, and leave **Give the
   agent the task brief as its first prompt** checked.
4. Click **Create**. soi creates the worktree, a workspace and a plan, and
   starts the agent with the brief.

The agent appears in the agents panel (◨, or Ctrl+Shift+G) with its state.
Once its integration reports a session, you can resume it later from **Task
details ▸ Agents**.

## Where things live

| What | Where |
|------|-------|
| Settings | `~/.config/soi/` |
| Agents to start | `~/.config/soi/agent-presets.json` |
| Task plans and history | `~/.local/state/soi/plans/` |
| Layouts, session bookmarks, log | `~/.local/state/soi/` |

## Troubleshooting

- **"… isn't installed or isn't on your PATH"**: run `which <program>` in
  the shell from Settings ▸ General. Add its folder to PATH in that shell's
  login configuration, then restart soi.
- **The agent starts but shows an API or model error**: the agent's own
  default model doesn't work with your login. Set another one in the agent,
  or give the soi entry a model.
- **Agents only show "busy" or "quiet"**: install their integration under
  Settings ▸ Agents ▸ Exact states, then restart the agent.
- **Nothing happens on macOS the first time**: see the quarantine step in
  [Install](#macos-apple-silicon).
