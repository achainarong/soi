#!/bin/sh
# Installs (or updates) soi from a GitHub release, for this user only:
#
#   curl -fsSL https://raw.githubusercontent.com/achainarong/soi/main/scripts/install.sh | sh
#
# Linux: the AppImage goes to ~/.local/bin/soi, with an app menu entry and
# icon. macOS (Apple Silicon): soi.app goes to ~/Applications, and
# ~/.local/bin/soi links to it, for shells.
#
#   SOI_VERSION=v0.1.18   install that release instead of the latest
#   SOI_BIN_DIR=...       where the soi command goes (default ~/.local/bin)
#   SOI_APP_DIR=...       macOS: where soi.app goes (default ~/Applications)
#
# Settings and layouts (~/.config/soi, ~/.local/state/soi) are left alone.
set -eu

REPO=achainarong/soi
APP_ID=dev.soi.Soi
VERSION=${SOI_VERSION:-latest}
BIN_DIR=${SOI_BIN_DIR:-$HOME/.local/bin}
APP_DIR=${SOI_APP_DIR:-$HOME/Applications}

say() { printf 'soi: %s\n' "$*"; }
die() { printf 'soi: %s\n' "$*" >&2; exit 1; }

if [ "$VERSION" = latest ]; then
    base=https://github.com/$REPO/releases/latest/download
    ref=main
else
    case $VERSION in v*) ;; *) VERSION=v$VERSION ;; esac
    base=https://github.com/$REPO/releases/download/$VERSION
    ref=$VERSION
fi

# fetch URL FILE
fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fL --progress-bar -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --show-progress -O "$2" "$1"
    else
        die "needs curl or wget"
    fi || die "couldn't download $1"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

install_linux() {
    case $(uname -m) in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) die "no Linux release for $(uname -m); build from source instead (see the README)" ;;
    esac

    # The AppImage is built against glibc 2.39.
    glibc=$(ldd --version 2>/dev/null | head -n1 | grep -o '[0-9]*\.[0-9]*$' || true)
    if [ -n "$glibc" ]; then
        major=${glibc%.*} minor=${glibc#*.}
        if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 39 ]; }; then
            die "needs glibc 2.39 or newer, this system has $glibc; build from source instead (see the README)"
        fi
    fi

    say "downloading soi-$arch.AppImage ($VERSION)"
    fetch "$base/soi-$arch.AppImage" "$tmp/soi"
    chmod 755 "$tmp/soi"

    # Moved into place rather than overwritten, so a running soi keeps going.
    mkdir -p "$BIN_DIR"
    mv -f "$tmp/soi" "$BIN_DIR/soi"

    data=${XDG_DATA_HOME:-$HOME/.local/share}
    icon=$data/icons/hicolor/scalable/apps/$APP_ID.svg
    mkdir -p "${icon%/*}" "$data/applications"
    fetch "https://raw.githubusercontent.com/$REPO/$ref/data/$APP_ID.svg" "$tmp/icon.svg" 2>/dev/null &&
        mv -f "$tmp/icon.svg" "$icon"
    # Same as data/dev.soi.Soi.desktop.in; the icon by full path, as make install does.
    cat > "$data/applications/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Soi
GenericName=Terminal
Comment=Terminal workspaces with splits, groups and saved layouts
Exec=$BIN_DIR/soi
Icon=$icon
Terminal=false
Categories=System;TerminalEmulator;GTK;
Keywords=terminal;shell;console;workspace;split;
StartupNotify=true
StartupWMClass=$APP_ID
EOF
    update-desktop-database -q "$data/applications" 2>/dev/null || true
    kbuildsycoca6 --noincremental >/dev/null 2>&1 || true

    if ! command -v fusermount >/dev/null 2>&1 && ! command -v fusermount3 >/dev/null 2>&1; then
        say "FUSE seems to be missing: install it (e.g. fuse2 or libfuse2), or start soi with --appimage-extract-and-run"
    fi
    say "installed $BIN_DIR/soi; start it from your app menu or run: soi"
}

install_macos() {
    # The hardware, not this shell: under Rosetta, uname -m says x86_64.
    [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ] ||
        die "the macOS release is for Apple Silicon only; build from source on Intel (see the README)"

    say "downloading soi-macos-arm64.zip ($VERSION)"
    fetch "$base/soi-macos-arm64.zip" "$tmp/soi.zip"
    ditto -x -k "$tmp/soi.zip" "$tmp/unzipped"
    # soi isn't notarized; without this, Gatekeeper refuses to open it.
    xattr -dr com.apple.quarantine "$tmp/unzipped/soi.app" 2>/dev/null || true

    mkdir -p "$APP_DIR" "$BIN_DIR"
    rm -rf "$APP_DIR/soi.app"
    mv "$tmp/unzipped/soi.app" "$APP_DIR/soi.app"
    ln -sf "$APP_DIR/soi.app/Contents/MacOS/soi" "$BIN_DIR/soi"
    say "installed $APP_DIR/soi.app; start it from Launchpad or run: soi"
}

case $(uname -s) in
    Linux) install_linux ;;
    Darwin) install_macos ;;
    *) die "soi runs on Linux and macOS, not $(uname -s)" ;;
esac

case :$PATH: in
    *:"$BIN_DIR":*) ;;
    *) say "$BIN_DIR isn't on your PATH; add it to use the soi command in a shell" ;;
esac
say "to let Claude Code, OpenCode and Pi work with soi, open Settings ▸ Agents (or run: soi install-agents)"
