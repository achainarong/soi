"""Regression checks for the release gate, independent of GTK or a display."""

import io
import os
import sys
import unittest
from unittest.mock import Mock

import test_release as release


class ReleaseTests(unittest.TestCase):
    def command(self, code, **kwargs):
        log = io.StringIO()
        release.check_command([sys.executable, "-c", code], os.environ.copy(),
                              log, **kwargs)
        return log.getvalue()

    def test_expected_nonzero_exit_and_output(self):
        self.assertIn("usage", self.command("print('usage'); raise SystemExit(2)",
                                           expected=2, contains="usage"))

    def test_unexpected_exit_fails(self):
        with self.assertRaisesRegex(RuntimeError, "got 7"):
            self.command("raise SystemExit(7)")

    def test_missing_output_fails(self):
        with self.assertRaisesRegex(RuntimeError, "Missing expected output"):
            self.command("pass", contains="usage")

    def test_command_timeout_fails_and_reaps(self):
        with self.assertRaisesRegex(RuntimeError, "timed out"):
            self.command("import time; time.sleep(60)", timeout=0.1)

    def test_gui_crash_fails_even_if_window_probe_succeeds(self):
        process = Mock()
        process.poll.return_value = 7
        process.returncode = 7
        with self.assertRaisesRegex(RuntimeError, "GUI exited"):
            release.wait_for_window(process, lambda: True)

    def test_no_window_times_out(self):
        process = Mock()
        process.poll.return_value = None
        with self.assertRaisesRegex(RuntimeError, "No stable visible"):
            release.wait_for_window(process, lambda: False, timeout=0.01)

    def test_visible_live_window_passes(self):
        process = Mock()
        process.poll.return_value = None
        release.wait_for_window(process, lambda: True, settle=0)


if __name__ == "__main__":
    unittest.main()
