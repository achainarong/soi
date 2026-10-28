# Handoff: Nimbalyst-inspired task UI for Soi

## Goal

Bring Nimbalyst's task-and-plan workflow into Soi without turning the terminal
workspace manager into a full IDE. The user wants to create and manage a ticket
inside Soi, rather than manually create a Markdown file and type its path.

This handoff documents remaining work. It does not approve a detailed
implementation design or authorize unrelated features.

## Repository state at handoff

- Repository: `/Users/a.chainarong/Development/github/soi`
- Branch: `feat/agent-task-workspaces`
- HEAD: `4d4e427` — `adding worktrees`
- Working tree was clean before this handoff was added.
- Keep the work on this feature branch; do not merge into `main`.
- Recheck status before editing: other changes may have happened since handoff.

## User constraints

- **Do not launch GUI tests, test windows, screenshots, or repeated window
  switching without explicit permission.** Previous GUI validation visibly
  disrupted the user's desktop; they specifically asked it to stop.
- **No CI work:** do not add, change, run, plan, or suggest CI jobs unless asked.
  See the repository's `AGENTS.md`.
- Follow the configured discussion → proposal → approval → implementation →
  validation → senior-review workflow. Propose files and snippets before coding.
- The configured workflow prohibits commits. Do not commit or push under those
  instructions; the existing commits were not created by this agent.
- Preserve unfamiliar user changes. Do not reset or discard working-tree edits.
- Never automatically delete worktrees/branches, merge, or discard Git changes.
- Keep the `soi` executable, application ID and existing config/state paths.
  The visible application label is now **Soi**.

## Already implemented

- Task workspaces backed by Git branches and worktrees.
- Repository field prefills from the active terminal's current directory,
  falling back to the selected workspace's original directory, then home.
- Relative worktree destinations resolve from the repository root; absolute
  destinations remain supported. Blank input defaults to
  `.worktrees/<normalized-task-name>`.
- In-repository worktrees trigger a warning when not Git-ignored. Ignore files
  are not silently modified. Existing destinations are protected.
- Workspace context: task title, explicit status, repository/branch references,
  linked plan path and important-file paths.
- Agents inherit their workspace task. Activity and task-status filters are
  independent; an idle/done agent does not automatically finish a task.
- Searchable local agent-session bookmarks and explicit resume.
  Claude hooks bookmark automatically; OpenCode/Codex can be bookmarked manually.
- Read-only workspace-wide staged/unstaged diffs; untracked filenames are listed
  without loading their contents. Changes are not falsely attributed to an agent.
- Read-only Markdown plan preview beside terminals, refreshed from disk.
- New utility windows, controls and diff colors use Soi's theme.
- Escape closes task/context/bookmark/history/change windows. Closing is blocked
  during worktree creation. The preview itself has a close button/menu toggle.
- Application labels and window-title suffixes use **Soi**.

### Current limitation that motivated the next feature

The Plan file field only links an existing Markdown file. It does **not** create
the file, provide an editor, import a ticket, or send the plan to an agent.
The user found this confusing and asked for the richer Nimbalyst-style UI.

## Proposed remaining scope

These were discussed as the desired direction, not yet implemented:

### 1. Ticket/task editor

- Create/edit a title, description, acceptance criteria and checklist in Soi.
- Optionally attach a Jira/GitHub issue URL as a reference.
- Keep explicit task status and workspace/agent associations visible.
- Users should not need to prepare a plan file or know its path first.

### 2. Create and edit plans inside Soi

- Add an editable Markdown plan view with a preview; reuse the existing safe
  renderer rather than introducing a browser or full document framework.
- Create/link a local Markdown file behind the scenes when requested.
- Show where the file is saved, allow existing files, and never overwrite one
  silently. Resolve relative paths against the task worktree.
- Decide where managed plans live before implementation. They must be readable
  by the agent, and the UI must make Git visibility clear.
- Handle unsaved edits, save failures and external edits/atomic file replacement
  without silently losing either the user's or an agent's changes.

### 3. Connected task detail/review view

- Bring task details, Plan/Edit/Preview, linked sessions and workspace changes
  together in a coherent task view instead of unrelated dialogs.
- Reuse existing session-resume and read-only change-inspection behavior.
- Preserve the terminal as the primary working surface.
- Clearly distinguish workspace-wide changes from reliable per-agent attribution.

### 4. Deliberate agent context handoff

- Make it clear whether the plan is merely linked or has been shared with an agent.
- Propose an explicit action to copy a task brief/plan reference, or a supported
  provider-specific launch option. Define the first version before implementing.
- Do not silently paste instructions into a running terminal, change permissions,
  scrape private provider storage, or claim that linking a plan sends it to an agent.

### Not part of the first version

- Jira/GitHub authentication, issue import/sync or publishing.
- Full chat UI, code editor, drawing canvas, multiplayer or mobile support.
- A task board/context-graph system unless separately requested.
- Automatic agent orchestration, Git staging, commits, merging or cleanup.

## Relevant code

| File | Responsibility |
| --- | --- |
| `src/tasks.rs` | `TaskContext`, `TaskStatus`, `AgentSession`, bookmark storage |
| `src/session.rs` | Persisted workspaces; optional boxed task context |
| `src/app/tasks.rs` | Task creation/context/history/bookmark dialogs; Escape helper |
| `src/app/preview.rs` | Safe, read-only Markdown preview and file refresh |
| `src/app/changes.rs` | Read-only Git change inspector |
| `src/app/panel.rs` | Agent/task labels, filters, reports and navigation |
| `src/app/switcher.rs` | Workspace/task/agent/bookmark search |
| `src/app.rs` | Workspace lifecycle, theme CSS and GUI regression tests |
| `src/git.rs` | Worktree destinations, ignore checks, creation, status/diffs |
| `src/agents.rs` | State/session reporting and provider integrations |
| `README.md` | Current task workflow and limitations |

`TaskContext` currently contains `id`, `title`, `status`, `repository`, `branch`,
`plan` and `important_files`. There are no description, issue-link, acceptance-
criteria or checklist fields yet. Decide whether Markdown is the source of truth
for ticket content, rather than duplicating it in JSON. Any added serialized
fields must preserve compatibility with existing saved sessions.

## Outstanding rendering issue

The user supplied a screenshot of vertically stretched sidebar text/icons after
clicking an agent. This is **not a confirmed normal font-zoom problem**.

- Agent navigation now avoids re-presenting an already-active window.
- A repeated-navigation regression checks sidebar geometry, label metrics and
  terminal zoom. This is a mitigation/check, not proof the screenshot's cause is fixed.
- The exact corruption was not reproduced in controlled OpenGL/Cairo checks.
- No global renderer fallback was added. `README.md` documents the opt-in
  `GSK_RENDERER=cairo` comparison; an existing instance must be quit first.
- Do not claim this issue resolved without user confirmation. Do not run more
  visible reproduction attempts without asking.

## Acceptance criteria for the next implementation

1. A user can create a task with a description and checklist without manually
   creating a file or entering a plan path.
2. Soi can create a Markdown plan in an explicitly shown location inside the
   task worktree, or safely link an existing plan.
3. Plan editing and preview are accessible from the task view; external edits
   and unsaved edits are handled without silent data loss.
4. Task content, status and links survive restart; old sessions still load.
5. Task details, linked agents/sessions and workspace changes are easy to navigate.
6. Linking a plan is not presented as automatically delivering agent context.
7. Controls match dark/light themes, support keyboard access and retain Escape
   behavior, with an explicit policy for unsaved edits.
8. No automatic agent restart, permission bypass or destructive Git operation.

## Validation and next step

Start by proposing the first-version task view, storage model and save/close
behavior, with exact files and snippets. Wait for approval before implementation.

Use local, non-GUI validation: `cargo fmt --check`, `cargo check`,
`cargo clippy --all-targets -- -D warnings`, and the ordinary non-ignored tests.
Do not use `--ignored` or run the temporary GUI harness without permission.
Previous checks passed, but rerun relevant checks against the new changes.
