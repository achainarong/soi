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
