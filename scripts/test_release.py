#!/usr/bin/env python3
"""Exercise a packaged soi executable, including a visible GUI startup."""

import argparse
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time


def stop(process):
    # AppImages and terminal shells can have children: clean up the whole group.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)


def check_command(command, env, log, expected=0, contains=None, timeout=30):
    log.write(f"$ {shlex.join(map(str, command))}\n")
    log.flush()
    process = subprocess.Popen(command, env=env, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               text=True, start_new_session=True)
    try:
        try:
            output, _ = process.communicate("", timeout=timeout)
        except subprocess.TimeoutExpired as error:
            stop(process)
            output, _ = process.communicate(timeout=5)
            log.write(output)
            raise RuntimeError(f"Command timed out after {timeout}s") from error
        log.write(output)
        log.flush()
        if process.returncode != expected:
            raise RuntimeError(f"Expected exit {expected}, got {process.returncode}")
        if contains is not None and contains not in output:
            raise RuntimeError(f"Missing expected output: {contains!r}")
    finally:
        stop(process)


def window_probe(root):
    if sys.platform != "darwin":
        return ["xdotool", "search", "--onlyvisible", "--class", "[Ss]oi"]
    # CoreGraphics lists windows without AppleScript accessibility automation.
    source = root / "windows.swift"
    source.write_text('''import CoreGraphics
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows {
    if let layer = window[kCGWindowLayer as String] as? Int, layer == 0,
       let pid = window[kCGWindowOwnerPID as String] as? Int {
        print(pid)
    }
}
''')
    executable = root / "windows"
    subprocess.run(["xcrun", "swiftc", str(source), "-o", str(executable)],
                   check=True, timeout=120)
    return [str(executable)]


def has_window(probe, process):
    result = subprocess.run(probe, capture_output=True, text=True, timeout=5)
    if result.returncode not in (0, 1):
        raise RuntimeError(f"Window probe failed: {result.stderr}")
    for value in result.stdout.split():
        if sys.platform == "darwin":
            pid = int(value)
        else:
            owner = subprocess.run(["xdotool", "getwindowpid", value],
                                   capture_output=True, text=True, timeout=5)
            if owner.returncode != 0:
                continue
            pid = int(owner.stdout.strip())
        try:
            if os.getpgid(pid) == process.pid:
                return True
        except ProcessLookupError:
            pass
    return False


def wait_for_window(process, visible, timeout=45, settle=3):
    deadline = time.monotonic() + timeout
    shown = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"GUI exited during startup: {process.returncode}")
        if visible():
            if shown is None:
                shown = time.monotonic()
            if time.monotonic() - shown >= settle:
                return
        else:
            shown = None
        time.sleep(0.2)
    raise RuntimeError(f"No stable visible soi window within {timeout}s")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--logs", type=Path, default=Path("release-test-logs"))
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.logs.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="soi-release-") as directory:
        root = Path(directory)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("SOI_", "XDG_", "GTK_", "GDK_", "DYLD_", "LD_"))}
        for key, child in [("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
                           ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"),
                           ("XDG_CACHE_HOME", "cache"), ("XDG_RUNTIME_DIR", "runtime")]:
            path = root / child
            path.mkdir(mode=0o700)
            env[key] = str(path)
        env.update(SHELL="/bin/bash", APPIMAGE_EXTRACT_AND_RUN="1")
        if sys.platform != "darwin":
            env.update(GDK_BACKEND="x11", GSK_RENDERER="cairo")
        with (args.logs / "commands.log").open("w") as log:
            check_command([str(binary), "status"], env, log, expected=2,
                          contains="usage: soi status")
            check_command([str(binary), "status", "invalid"], env, log, expected=2,
                          contains="usage: soi status")
            check_command([str(binary), "claude-hook"], env, log)
            check_command([str(binary), "install-agents"], env, log,
                          contains="not set up here, skipped")
        probe = window_probe(root)
        with (args.logs / "gui.log").open("w") as log:
            process = subprocess.Popen([str(binary)], env=env, cwd=root,
                                       stdin=subprocess.DEVNULL, stdout=log,
                                       stderr=subprocess.STDOUT, start_new_session=True)
            try:
                wait_for_window(process, lambda: has_window(probe, process))
            finally:
                stop(process)
    print("PASS: packaged commands and visible GUI startup")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
