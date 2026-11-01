# Handoff: native platform shortcuts and terminal behavior

## Goal

Soi should feel native to the OS on which **Soi itself is running**: macOS conventions on macOS, Linux desktop/terminal conventions on Linux. This covers mouse gestures, keyboard shortcuts, clipboard behavior, menus, tooltips, and shortcut settings—not just replacing a modifier label.

This is a documentation-only handoff for a future implementation session. It does not authorize implementation, commits, or changes to users' settings. Propose concrete changes and compatibility decisions before editing code.

## User-reported issue and ownership

While running Tha inside Soi on macOS, the user can open a Security advisory URL with **Control + left-click**, but not with the expected **Command + left-click**.

Tha renders the advisory URL as plain terminal text. Soi detects and opens the link. Fix this at the **Soi terminal layer**, so it works for links printed by Tha, shells, logs, and other terminal applications. Do not require a Tha-specific workaround or remap input inside child applications.

Expected default:

| Interaction | macOS | Linux |
| --- | --- | --- |
| Open a terminal link | Command + left-click | Control + left-click |
| Terminal copy / paste | Command+C / Command+V | Control+Shift+C / Control+Shift+V |
| Terminal interrupt | Control+C to the foreground program | Control+C to the foreground program |
| Soi GUI actions | Platform-appropriate Command-based defaults | Existing Linux defaults, respecting desktop shortcuts |
| Display modifier names | Command/⌘, Option/⌥, Control/⌃ where appropriate | Ctrl, Alt, Shift, Super as appropriate |

These are action-specific defaults, **not** a blanket Ctrl↔Command translation. Linux terminal copy/paste differs from ordinary GUI text-field copy/paste. Preserve native editable-widget conventions in Settings, rename fields, search fields, and other text inputs.

## Observed repository evidence

Inspected in the current working tree; line numbers can move, so use symbols and nearby text when implementing:

- `src/term/widget.rs`: hover tooltip near line 1414 says `Ctrl+click to open` unconditionally.
- `src/term/widget.rs`: `click.connect_pressed` near lines 2053–2064 checks `CONTROL_MASK` before `open_hovered_link()`.
- `src/term/widget.rs`: `drag.connect_drag_begin` near lines 2128–2137 also uses `CONTROL_MASK` to suppress selection dragging when opening links.
- `src/term/widget.rs`: `click.connect_released` forwards mouse releases to mouse-reporting applications. Review press/release ownership together; a consumed link-open press must not leak an unmatched release to a child application.
- `src/platform/mod.rs`: `Platform::cmd_shortcuts()` already describes macOS Command shortcuts as GTK `<Meta>`. Reuse the existing platform boundary instead of scattering OS checks through event handlers. Verify actual GDK modifier events on macOS rather than assuming Command is always `SUPER_MASK` or copying browser `metaKey` logic.
- `src/keybindings.rs` and `src/app/settings_window/shortcuts.rs`: existing shortcut definitions and rebinding UI are the starting points for keyboard behavior and presentation.
- `README.md`: Default shortcuts already documents Command-based macOS actions and Linux defaults. However, the clickable-links feature still advertises Ctrl+click without an OS distinction.
- `docs/macos-port.md`: documents the platform abstraction, macOS validation constraints, and the main-thread GTK GUI-test harness.

There are already unrelated/uncommitted edits in `src/app/panes/tabs.rs`, `src/app/settings_window/form.rs`, `src/app/settings_window/mod.rs`, `src/settings.rs`, and `src/term/widget.rs`. Recheck the diff before working. Do not overwrite, revert, or attribute those edits to this handoff.

## Priority 1: native terminal link activation

1. Define one platform-aware link activation policy using the existing platform layer. Derive the event predicate and displayed modifier hint from the same policy.
2. Apply it consistently to link activation, hover guidance, drag suppression, and gesture ownership. Changing only the press handler leaves misleading help and conflicting selection behavior.
3. On macOS, Command+left-click opens the hovered URL through Soi's existing link-opening path. On Linux, Control+left-click continues to do so.
4. Keep ordinary click, text selection, double/triple-click selection, scrolling, and context menus working. Verify macOS Control-click follows the chosen context-menu/terminal policy rather than being accidentally treated as Command-click.
5. Preserve the existing real-target preview, URI validation, launch/error handling, and security boundaries for terminal-supplied links. Do not add shell-string execution to open a URL.
6. Cover both detected plain-text URLs and OSC 8 hyperlinks, including cases where displayed text differs from the destination.
7. Retaining Control-click as an optional macOS compatibility gesture is a proposal decision, not an assumption. Check for conflicts with context menus and mouse-reporting applications before retaining it. Extra modifiers must not accidentally activate links.

## Priority 2: audit native interaction consistency

Inventory existing behavior before proposing any broad remapping. Preserve already-correct macOS defaults; this is not a request to replace the shortcut system.

- **GUI actions:** tabs, windows, workspaces, pane focus/splits, search, zoom, settings, and quit. Check actual dispatch, menus, tooltips, and Settings labels agree.
- **Clipboard:** distinguish terminal selection/clipboard from editable GTK fields. Preserve bracketed paste and multiline-paste safeguards. Preserve Linux primary-selection/middle-click behavior where supported; do not force it onto macOS as the default model.
- **Terminal ownership:** keep shell Control+C, Control+D, Control+Z, Control+R, Escape, and application-specific input intact. Option/Alt terminal encoding and Kitty keyboard/mouse reporting must not be globally rewritten into GUI shortcuts.
- **Focus:** Soi shortcuts must not unexpectedly fire while typing in rename/search/settings fields. A handled host action must not also reach the child PTY.
- **Platform selection:** use the OS running Soi, not the OS reported by a container, SSH host, or foreground application. A Linux container inside Soi on macOS still uses macOS host gestures.
- **User overrides:** preserve explicit custom bindings and disabled actions across upgrades. Reset should restore defaults for the current host OS. Do not silently rewrite stored custom bindings; propose migration behavior if storage cannot distinguish defaults from overrides.
- **Desktop conventions:** avoid grabbing Linux window-manager shortcuts unnecessarily. Do not make Linux imitate macOS or treat every Linux desktop as identical.

If any audit item needs a new setting, changes existing defaults, or affects persisted bindings, list the affected actions/files and obtain approval before implementing that expansion.

## Acceptance criteria and validation

Run an explicit matrix on both macOS and Linux; do not claim Linux GUI behavior is verified by a macOS-only build.

| Check | Expected result |
| --- | --- |
| macOS Command+left-click on a plain URL | Opens once using the existing safe link-opening path; hint says Command/⌘ |
| Linux Control+left-click on a plain URL | Opens once; hint says Ctrl |
| URL shown by Tha Security | Works without modifying Tha or its shortcut configuration |
| OSC 8 link with a different visible label | Actual target remains inspectable; activation uses the real validated destination |
| Ordinary click and click-drag | No browser launch; selection/focus retain intended behavior |
| Link gesture followed by release/drag | No unintended selection and no unmatched mouse event delivered to the child |
| Mouse-reporting application, including alternate screen | Host link gesture and app mouse input have clearly tested ownership; Shift override still works |
| macOS Control-click and non-link modified clicks | Follow the explicitly chosen native policy; do not accidentally launch links |
| Copy/paste in terminal and GTK text fields | Each uses its native contextual shortcuts; paste safeguards remain active |
| Shell Control+C/D/Z/R and terminal Option/Alt input | Reach the foreground program unless a deliberate, documented user binding overrides them |
| Custom binding, disabled binding, reset, restart | Overrides persist; reset uses current-platform defaults; presentation matches dispatch |
| Menus, tooltips, Settings, README | Show the same effective gesture/action for the current platform |

Suggested automated coverage:

- Unit-test the policy for both OS variants, modifier combinations, buttons, and effective binding labels without requiring two build hosts.
- Add regression tests for link press/release/drag ownership, dispatch-versus-label parity, and untouched terminal encodings.
- Use the existing dedicated GUI harness for event/focus integration. Check native Command event mapping on a real macOS session and Linux behavior in an actual supported desktop session.

Use the repository's existing validation commands, subject to required native dependencies:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
make test-gui
```

Deliver a per-platform pass/fail/not-run report, document any compatibility decision, and update the existing README/macOS docs where behavior changes. Keep unavailable-host checks explicitly not run rather than relaxing acceptance criteria.

## Scope and next step

Start with the link modifier defect, propose the exact implementation diff, then perform the broader audit as separately approved work. Keep the solution in Soi; no dependency on Tha changes. This handoff does not resolve or resume Tha's separate, paused shortcut-consolidation implementation.
