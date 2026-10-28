#!/usr/bin/env python3
"""Build a relocatable, ad-hoc-signed macOS bundle without changing Homebrew."""

import argparse
import json
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    return subprocess.check_output([str(arg) for arg in args], text=True).strip()


def system_library(path):
    return path.startswith(("/usr/lib/", "/System/Library/"))


def dependencies(path):
    lines = run("otool", "-L", path).splitlines()[1:]
    return [line.strip().split(" (compatibility version", 1)[0] for line in lines]


def writable_copy(source, destination):
    # Keep the signature: removing it leaves a gap in __LINKEDIT that
    # install_name_tool rejects. build() re-signs every image afterwards.
    shutil.copy2(source, destination)
    destination.chmod(0o755)


def minimum_macos(path):
    commands = run("otool", "-l", path)
    versions = re.findall(r"\bminos (\d+(?:\.\d+)+)", commands)
    versions += re.findall(r"cmd LC_VERSION_MIN_MACOSX\s+cmdsize \d+\s+version (\d+(?:\.\d+)+)", commands)
    return max((version_tuple(v) for v in versions), default=(12, 0, 0))


def version_tuple(version):
    parts = tuple(map(int, version.split(".")))
    return (parts + (0, 0, 0))[:3]


def resolve_dependency(dependency, source):
    if dependency.startswith("@loader_path/"):
        return source.parent / dependency.removeprefix("@loader_path/")
    if dependency.startswith("/"):
        return Path(dependency)
    if dependency.startswith("@rpath/"):
        rpaths = re.findall(
            r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset \d+\)",
            run("otool", "-l", source),
        )
        for rpath in rpaths:
            if not rpath.startswith(("/", "@loader_path/")):
                continue
            base = resolve_dependency(rpath.rstrip("/") + "/", source)
            candidate = base / dependency.removeprefix("@rpath/")
            if candidate.is_file():
                return candidate
    raise ValueError(f"Cannot resolve dependency {dependency} in {source}")


class Libraries:
    def __init__(self, contents):
        self.frameworks = contents / "Frameworks"
        self.frameworks.mkdir()
        self.sources = {}
        self.minimum = (12, 0, 0)

    def copy(self, source):
        source = Path(source).resolve(strict=True)
        name = source.name
        if name in self.sources:
            if self.sources[name] != source:
                raise ValueError(f"Conflicting library name: {name}")
            return name
        self.sources[name] = source
        destination = self.frameworks / name
        writable_copy(source, destination)
        self.rewrite(source, destination, "@loader_path/")
        # Loader modules are Mach-O bundles, not dylibs, and have no LC_ID_DYLIB.
        if source.suffix == ".dylib":
            run("install_name_tool", "-id", f"@loader_path/{name}", destination)
        return name

    def rewrite(self, source, destination, prefix):
        self.minimum = max(self.minimum, minimum_macos(source))
        for dependency in dependencies(source):
            if system_library(dependency):
                continue
            resolved = resolve_dependency(dependency, source)
            # otool -L lists a dylib's own install name too.
            if resolved.resolve() == source.resolve():
                continue
            name = self.copy(resolved)
            run("install_name_tool", "-change", dependency, prefix + name, destination)

    def verify(self, executable):
        for image in [executable, *self.frameworks.iterdir()]:
            for dependency in dependencies(image):
                if system_library(dependency):
                    continue
                if dependency.startswith("@loader_path/"):
                    target = image.parent / dependency.removeprefix("@loader_path/")
                elif dependency.startswith("@executable_path/"):
                    target = executable.parent / dependency.removeprefix("@executable_path/")
                else:
                    raise ValueError(f"External dependency left in {image}: {dependency}")
                if not target.is_file():
                    raise ValueError(f"Missing bundled dependency: {target}")

    def licenses(self, resources):
        # Preserve the notices shipped by each Homebrew keg we copied from.
        kegs = set()
        for source in self.sources.values():
            keg = next((p for p in source.parents if (p / "INSTALL_RECEIPT.json").is_file()), None)
            if keg is None or keg in kegs:
                continue
            kegs.add(keg)
            output = resources / "licenses" / f"{keg.parent.name}-{keg.name}"
            output.mkdir(parents=True)
            for pattern in ["COPYING*", "LICENSE*", "LICENCE*", "NOTICE*"]:
                for notice in keg.glob(pattern):
                    if notice.is_file():
                        shutil.copy2(notice, output / notice.name)
        (resources / "dependencies.json").write_text(json.dumps(
            {name: str(source) for name, source in sorted(self.sources.items())}, indent=2
        ) + "\n")


def resources(contents, libraries):
    output = contents / "Resources"
    output.mkdir()
    prefix = Path(run("pkg-config", "--variable=prefix", "gtk4"))
    schemas = prefix / "share/glib-2.0/schemas"
    if not schemas.is_dir():
        raise ValueError(f"Missing GTK schemas: {schemas}")
    shutil.copytree(schemas, output / "share/glib-2.0/schemas")
    run("glib-compile-schemas", "--strict", output / "share/glib-2.0/schemas")

    cache = Path(run("pkg-config", "--variable=gdk_pixbuf_cache_file", "gdk-pixbuf-2.0"))
    blocks = []
    for block in cache.read_text().split("\n\n"):
        lines = [line for line in block.splitlines() if line and not line.startswith("#")]
        if not lines:
            continue
        module = json.loads(lines[0])
        name = libraries.copy(module)
        # dyld expands this relative to soi-bin, even after the app is moved.
        lines[0] = json.dumps(f"@executable_path/../Frameworks/{name}")
        blocks.append("\n".join(lines))
    (output / "loaders.cache").write_text("\n\n".join(blocks) + "\n")

    # Pango normally uses CoreText; keep fontconfig's fallback independent of brew.
    (output / "fonts.conf").write_text('''<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>/System/Library/Fonts</dir>
  <dir>/Library/Fonts</dir>
  <dir>~/Library/Fonts</dir>
  <dir prefix="xdg">fonts</dir>
  <cachedir prefix="xdg">fontconfig</cachedir>
</fontconfig>
''')
    iconset = output / "soi.iconset"
    iconset.mkdir()
    for size in [16, 32, 128, 256, 512]:
        for scale, suffix in [(1, ""), (2, "@2x")]:
            run("rsvg-convert", "-w", size * scale, "-h", size * scale,
                ROOT / "data/dev.soi.Soi.svg", "-o", iconset / f"icon_{size}x{size}{suffix}.png")
    run("iconutil", "-c", "icns", iconset, "-o", output / "soi.icns")
    shutil.rmtree(iconset)
    return output


LAUNCHER = '''#!/bin/sh
set -eu
# Resolve the command-line symlink as well as a Finder launch.
launcher=$0
while [ -L "$launcher" ]; do
    link=$(/usr/bin/readlink "$launcher")
    case "$link" in
        /*) launcher=$link ;;
        *) launcher=$(dirname "$launcher")/$link ;;
    esac
done
macos=$(CDPATH= cd -- "$(dirname "$launcher")" && pwd -P)
resources=$macos/../Resources
# Programs soi starts get these back as they were (see platform::set_gtk_env).
export SOI_BUNDLE_ENV="XDG_DATA_DIRS GSETTINGS_SCHEMA_DIR GDK_PIXBUF_MODULE_FILE FONTCONFIG_FILE"
for name in $SOI_BUNDLE_ENV; do
    eval "[ -z \\"\\${$name+set}\\" ] || export SOI_OUTER_$name=\\"\\$$name\\""
done
export XDG_DATA_DIRS="$resources/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export GSETTINGS_SCHEMA_DIR="$resources/share/glib-2.0/schemas"
export GDK_PIXBUF_MODULE_FILE="$resources/loaders.cache"
export FONTCONFIG_FILE="$resources/fonts.conf"
exec "$macos/soi-bin" "$@"
'''


def build(binary, bundle, version):
    contents = bundle / "Contents"
    macos = contents / "MacOS"
    macos.mkdir(parents=True)
    executable = macos / "soi-bin"
    writable_copy(binary, executable)
    libraries = Libraries(contents)
    libraries.rewrite(binary, executable, "@executable_path/../Frameworks/")
    output = resources(contents, libraries)
    libraries.licenses(output)
    launcher = macos / "soi"
    launcher.write_text(LAUNCHER)
    launcher.chmod(0o755)
    info = plistlib.loads((ROOT / "data/Info.plist.in").read_bytes())
    info["CFBundleShortVersionString"] = info["CFBundleVersion"] = version
    info["LSMinimumSystemVersion"] = ".".join(map(str, libraries.minimum))
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    libraries.verify(executable)
    for image in [*libraries.frameworks.iterdir(), executable]:
        run("codesign", "--force", "--sign", "-", image)
    run("codesign", "--force", "--sign", "-", bundle)
    run("codesign", "--verify", "--deep", "--strict", bundle)


def publish(staged, destination):
    # Only replace an existing Soi bundle, never an arbitrary directory/symlink.
    if destination.is_symlink():
        raise ValueError(f"Refusing to replace a symlink: {destination}")
    # Outside the staging directory: even failed rollback must survive its cleanup.
    backup = staged.parent.with_name(staged.parent.name + "-previous.app")
    if backup.exists():
        raise ValueError(f"Backup already exists: {backup}")
    replaced = False
    if destination.exists():
        info = plistlib.loads((destination / "Contents/Info.plist").read_bytes())
        if info.get("CFBundleIdentifier") != "dev.soi.Soi":
            raise ValueError(f"Not a Soi bundle: {destination}")
    try:
        if destination.exists():
            destination.rename(backup)
            replaced = True
        staged.rename(destination)
    except BaseException:
        if replaced and not destination.exists():
            try:
                backup.rename(destination)
            except OSError as error:
                raise RuntimeError(f"Rollback failed; previous app preserved at {backup}") from error
        raise
    if replaced:
        try:
            shutil.rmtree(backup)
        except OSError:
            print(f"Warning: previous app remains at {backup}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/soi")
    parser.add_argument("--output", type=Path, default=ROOT / "target/soi.app")
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        if sys.platform != "darwin":
            raise ValueError("macOS packaging requires macOS and Xcode command-line tools")
        destination = args.output.absolute()
        if destination.suffix != ".app":
            raise ValueError("--output must end in .app")
        destination.parent.mkdir(parents=True, exist_ok=True)
        # Same filesystem as the destination, so final renames are atomic.
        with tempfile.TemporaryDirectory(prefix=".soi-build-", dir=destination.parent) as temp:
            staged = Path(temp) / "soi.app"
            build(args.binary.resolve(strict=True), staged, args.version)
            publish(staged, destination)
        print(f"Built {destination}")
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"macOS bundle failed: {error}\n")


if __name__ == "__main__":
    main()
