# Soi

Soi organizes terminals and coding-agent work into persistent workspaces.

## Language

**Workspace**:
A named working context containing main tabs, optionally associated with a task.
_Avoid_: Project (a repository may have several workspaces)

**Main tab**:
A workspace's selectable terminal layout, containing one or more split panes.
_Avoid_: Pane tab, nested tab

**Task**:
A tracked piece of work with its own status, description and acceptance criteria.
_Avoid_: Agent activity (an agent's state does not determine task status)

**Plan**:
A task's linked Markdown working document, which may be edited by the user or an agent.
_Avoid_: Task metadata, task history

**Saved layout**:
The arrangement of windows, workspaces, main tabs and panes kept for reopening.
_Avoid_: Agent session

**Agent session**:
A provider-owned conversation that can be bookmarked and resumed independently of a saved layout.
_Avoid_: Saved layout, terminal output
