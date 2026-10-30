# macOS validation and limitations

## Local validation

Validated on Apple Silicon with macOS 27.0.1, Rust 1.96.0 and Homebrew GTK
4.24.1. Intel, older macOS releases and a separate Linux host have not been
validated in this environment.

```sh
brew install rust gtk4 pkg-config librsvg python
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
make test-gui
cargo test --release --locked fuzz_long -- --ignored
python3 -B -m unittest discover -s scripts -p 'test_*.py'
make bundle
```

The 31 GUI checks use a separate executable with `harness = false`: AppKit
requires GTK on the process's main thread. Each check runs in a child with its
own home, XDG directories and short socket directory. A check that hangs is
terminated after 60 seconds. The temporary root is canonicalized because macOS
process APIs return `/private/var` even when `$TMPDIR` names `/var`.

Apple's Bash 3.2 lacks `PS0`. For non-login bash integration, Soi uses a DEBUG
trap fallback only when the user has not already installed one, retaining the
user's prompt command. Explicit login shells (`bash -l`) remain unmodified.

## Bundling

`make bundle` produces `target/soi.app`. `make install AGENTS=0` installs a
bundle in `~/Applications` and a shell-command symlink without changing agent
configuration. `APPDIR` and `PREFIX` override those locations.

The packaging helper copies recursive non-system Mach-O dependencies and
GdkPixbuf loaders, rewrites their references, compiles bundled GTK schemas,
preserves available dependency license notices, and signs libraries before
signing and verifying the bundle. Its launcher selects bundled resources and
also works through the command-line symlink. Packaging stages the replacement
beside the destination and leaves the existing bundle intact on build failure;
publication rejects symlinks and unrelated app bundles.

Local smoke checks relocated the bundle to a path containing spaces, launched
both directly and through a symlink with only system directories on PATH,
and verified that no Homebrew libraries were loaded. The relocated code
signature also verified. A separate clean Mac and Finder/Gatekeeper acceptance
have not been tested.

The minimum macOS version in the generated Info.plist is the highest required
by any bundled image. With the dependencies above, that is **macOS 27**. The
template's baseline is not a promise that a bundle built with newer Homebrew
binaries will work on older systems. Bundles use the build machine's
architecture; they are ad-hoc signed, not notarized. Public distribution must
also satisfy the dependencies' licenses, including any source obligations.

## GTK frame-timing warning

GTK 4.24.1 can emit:

```text
gdk_frame_timings_presented() called on skipped frame.
```

This was reproduced in an otherwise empty GTK window without Soi code. GTK's
macOS backend reports presentation callbacks into its frame-timing state
machine; the diagnostic comes from GTK, not Soi's terminal renderer. The
warning was non-fatal in local app and bundle startup checks. Soi does not
suppress it, patch Homebrew libraries, or force a renderer globally.

For visible text/icon distortion, compare with `GSK_RENDERER=cairo make dev`
after quitting the existing instance. This is a diagnostic workaround, not
a guaranteed fix for GTK's frame-timing warning.

Platform differences remain behind `src/platform/`; Soi's files use XDG paths
on macOS, its socket uses `$TMPDIR`, and its default font is Menlo. See the
README for current Cmd shortcuts and installation details.

## Native terminal links

The link policy uses Command+left-click on macOS and Ctrl+left-click on Linux,
with the real target and matching hint shown on hover. Extra keyboard modifiers
do not activate links. Control-click on macOS is not retained as a link alias;
it follows the existing non-link pointer path, not a newly added context menu.
The host OS selects the gesture, regardless of a child container or SSH host.

Consumed link presses own their drag/release sequence, so mouse-reporting
applications do not receive an unmatched release. Shift selection override
remains available. Link validation and native safe launching are unchanged.

The provisional Command policy uses GDK Meta, matching the existing GTK
accelerator convention. Real Command-click event mapping is still **not
verified** in this change: native pointer automation lacks Accessibility
permission. GTK policy/ownership checks do not substitute for that check or
for a Linux desktop run. Record both before claiming native acceptance.

## Image paste and editable-field focus

Cmd+V is Soi's terminal Paste action; direct Ctrl+V belongs to the terminal
application. Image delegation is enabled by default on macOS and Linux. The profile
setting `image-paste-bridge = true` (Settings ▸ Terminal ▸ Delegate image paste
to terminal apps) sends one encoded Ctrl+V to the focused application when the
clipboard offers an image, including image+text offers. Set
`image-paste-bridge = false` to opt out; explicit false values survive upgrades.
It follows the effective Paste binding and does not broadcast the delegated key.
Text-only paste retains its existing safeguards. Editable GUI fields keep their
native cut/copy/paste handling; pending clipboard text is discarded if terminal
focus changes or the terminal exits before the read completes.

The bridge does not read image pixels, create image files, or transport them to
remote/container applications. The application must read the clipboard itself.
In shells Ctrl+V can quote the next key. Selecting terminal text can replace a
copied image because Soi currently copies selections to the standard clipboard.
Disable the bridge to restore text-only terminal paste; resetting shortcuts
does not change the setting.

Automated checks exercise profile updates, format-based dispatch, text safety,
focus, bindings, and isolated broadcast behavior in GTK. They do not establish
image-attachment compatibility for every OpenCode/Claude Code/Pi version or
Linux desktop backend. Those native application checks must be reported
separately, with unavailable hosts marked not run.

## Stale window tiles

GTK's macOS layer (`GdkMacosLayer`) splits the window into tiles and gives
new frame contents only to tiles that overlap the frame's redrawn region.
Other tiles keep showing an older buffer, which can be the wrong size or
scale: pieces of panes appear doubled or stale, and translucent windows show
the desktop where nothing was painted (seen when starting OpenCode in a
translucent window). On macOS, Soi sets `GSK_DEBUG=full-redraw` at
startup (unless `GSK_DEBUG` is already set), so every frame covers every
tile, and removes it from the environment of programs it starts. See
`Platform::gtk_env`.
