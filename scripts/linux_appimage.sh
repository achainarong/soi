#!/bin/sh
# Builds a self-contained AppImage of soi for this machine's architecture:
# soi with GTK 4 and the libraries it needs, so it runs without the
# system's GTK. Uses linuxdeploy and its GTK plugin, downloaded once into
# target/appimage/tools.
#
#   scripts/linux_appimage.sh BINARY OUTPUT
set -eu

binary=$(realpath "$1")
output=$(realpath -m "$2")
root=$(cd "$(dirname "$0")/.." && pwd)
arch=$(uname -m)
work=$root/target/appimage
appdir=$work/AppDir
tools=$work/tools

# Pinned, and checked, so a build means the same thing tomorrow.
linuxdeploy_release=1-alpha-20251107-1
gtk_plugin_commit=7a3fbc31a9e5075073ff8790f26effbac5f84453
case "$arch" in
    x86_64) linuxdeploy_sha256=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d ;;
    aarch64) linuxdeploy_sha256=620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff ;;
    *) echo "no AppImage for $arch" >&2; exit 1 ;;
esac
gtk_plugin_sha256=b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171

fetch() { # URL FILE SHA256
    if ! echo "$3  $2" | sha256sum -c --status 2>/dev/null; then
        curl -fsSL -o "$2.part" "$1"
        echo "$3  $2.part" | sha256sum -c --quiet
        mv "$2.part" "$2"
    fi
    chmod +x "$2"
}
mkdir -p "$tools/gtk-plugin"
linuxdeploy=$tools/linuxdeploy-$linuxdeploy_release-$arch.AppImage
fetch "https://github.com/linuxdeploy/linuxdeploy/releases/download/$linuxdeploy_release/linuxdeploy-$arch.AppImage" \
    "$linuxdeploy" "$linuxdeploy_sha256"
# linuxdeploy finds its plugins on PATH, by name.
fetch "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/$gtk_plugin_commit/linuxdeploy-plugin-gtk.sh" \
    "$tools/gtk-plugin/linuxdeploy-plugin-gtk.sh" "$gtk_plugin_sha256"

rm -rf "$appdir"
mkdir -p "$appdir"
sed -e 's|@BINDIR@/||' -e 's|@ICON@|dev.soi.Soi|' \
    "$root/data/dev.soi.Soi.desktop.in" > "$work/dev.soi.Soi.desktop"

# linuxdeploy is an AppImage itself; this runs it without FUSE (as in CI).
export APPIMAGE_EXTRACT_AND_RUN=1
PATH="$tools/gtk-plugin:$PATH" DEPLOY_GTK_VERSION=4 "$linuxdeploy" \
    --appdir "$appdir" \
    --executable "$binary" \
    --desktop-file "$work/dev.soi.Soi.desktop" \
    --icon-file "$root/data/dev.soi.Soi.svg" \
    --plugin gtk

# The plugin's start-up hook points GTK at the bundled files, but also
# forces X11 and the Adwaita theme, and asks D-Bus for the color scheme on
# every start. soi keeps only the paths, in its own AppRun, which lists
# them for soi to give programs it starts the values from before.
hook=$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh
paths=$(grep '^export ' "$hook" | grep -v -e '^export APPDIR=' -e GTK_THEME -e GDK_BACKEND)
names=$(printf '%s\n' "$paths" | sed 's/^export \([A-Za-z_]*\)=.*/\1/' | tr '\n' ' ')
rm -rf "$appdir/apprun-hooks" "$appdir/AppRun" "$appdir/AppRun.wrapped"
{
    cat <<'EOF'
#!/bin/sh
# Starts soi with the GTK bundled in this AppImage (see
# scripts/linux_appimage.sh and platform::set_gtk_env).
APPDIR=$(dirname "$(readlink -f "$0")")
EOF
    echo "export SOI_BUNDLE_ENV=\"${names}APPDIR APPIMAGE ARGV0 OWD\""
    cat <<'EOF'
for name in $SOI_BUNDLE_ENV; do
    case $name in APPDIR|APPIMAGE|ARGV0|OWD) continue ;; esac
    eval "[ -z \"\${$name+set}\" ] || export SOI_OUTER_$name=\"\$$name\""
done
EOF
    printf '%s\n' "$paths"
    echo 'exec "$APPDIR/usr/bin/soi" "$@"'
} > "$appdir/AppRun"
chmod +x "$appdir/AppRun"

cd "$work"
LDAI_OUTPUT=$output OUTPUT=$output "$linuxdeploy" --appdir "$appdir" --output appimage
echo "Built $output"
