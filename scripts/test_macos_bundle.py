"""Packaging regression checks; no Homebrew files or installed apps are modified."""

from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch

import macos_bundle as bundle


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def app(self, path, identifier="dev.soi.Soi"):
        contents = path / "Contents"
        contents.mkdir(parents=True)
        (contents / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": identifier,
        }))
        return path

    def test_minimum_os_uses_highest_slice(self):
        with patch.object(bundle, "run", return_value="minos 12.0\nminos 27.0\n"):
            self.assertEqual(bundle.minimum_macos(Path("image")), (27, 0, 0))
        with patch.object(bundle, "run", return_value="cmd LC_VERSION_MIN_MACOSX\ncmdsize 16\nversion 10.15\n"):
            self.assertEqual(bundle.minimum_macos(Path("image")), (10, 15, 0))

    def test_rpath_resolves_relative_to_original_library(self):
        library = self.root / "libcommon.dylib"
        library.touch()
        with patch.object(bundle, "run", return_value="cmd LC_RPATH\ncmdsize 40\npath @loader_path/ (offset 12)"):
            resolved = bundle.resolve_dependency("@rpath/libcommon.dylib", self.root / "libdecoder.dylib")
        self.assertEqual(resolved, library)

    def test_unresolved_rpath_fails_closed(self):
        with patch.object(bundle, "run", return_value=""):
            with self.assertRaisesRegex(ValueError, "Cannot resolve"):
                bundle.resolve_dependency("@rpath/missing.dylib", self.root / "image")

    def test_recursive_copy_handles_cycles_and_does_not_edit_sources(self):
        contents = self.root / "Contents"
        contents.mkdir()
        a, b = self.root / "a.dylib", self.root / "b.dylib"
        a.write_bytes(b"original a")
        b.write_bytes(b"original b")
        libraries = bundle.Libraries(contents)
        with patch.object(bundle, "unsigned_copy", side_effect=bundle.shutil.copy2), \
             patch.object(bundle, "minimum_macos", return_value=(12, 0, 0)), \
             patch.object(bundle, "dependencies", side_effect=lambda p: [str(b if p == a else a)]), \
             patch.object(bundle, "run"):
            libraries.copy(a)
        self.assertEqual(set(libraries.sources), {"a.dylib", "b.dylib"})
        self.assertEqual(a.read_bytes(), b"original a")
        self.assertEqual(b.read_bytes(), b"original b")

    def test_conflicting_library_names_are_rejected(self):
        contents = self.root / "Contents"
        contents.mkdir()
        libraries = bundle.Libraries(contents)
        source = self.root / "lib.dylib"
        source.touch()
        other = self.root / "other/lib.dylib"
        other.parent.mkdir()
        other.touch()
        libraries.sources[source.name] = source
        with self.assertRaisesRegex(ValueError, "Conflicting library"):
            libraries.copy(other)

    def test_verification_rejects_external_libraries(self):
        contents = self.root / "Contents"
        contents.mkdir()
        libraries = bundle.Libraries(contents)
        with patch.object(bundle, "dependencies", return_value=["/opt/homebrew/lib/missing.dylib"]):
            with self.assertRaisesRegex(ValueError, "External dependency"):
                libraries.verify(contents / "soi")

    def test_verification_rejects_missing_bundled_libraries(self):
        contents = self.root / "Contents"
        contents.mkdir()
        libraries = bundle.Libraries(contents)
        with patch.object(bundle, "dependencies", return_value=["@loader_path/missing.dylib"]):
            with self.assertRaisesRegex(ValueError, "Missing bundled"):
                libraries.verify(contents / "soi")

    def test_publish_replaces_only_soi_and_removes_backup_after_success(self):
        stage = self.root / "stage"
        stage.mkdir()
        old = self.app(self.root / "soi.app")
        (old / "old").touch()
        new = self.app(stage / "soi.app")
        bundle.publish(new, old)
        self.assertFalse((old / "old").exists())
        self.assertFalse((self.root / "stage-previous.app").exists())

    def test_publish_refuses_unrelated_app_and_symlink(self):
        new = self.app(self.root / "new.app")
        other = self.app(self.root / "other.app", "another.app")
        with self.assertRaisesRegex(ValueError, "Not a Soi"):
            bundle.publish(new, other)
        link = self.root / "link.app"
        link.symlink_to(other)
        with self.assertRaisesRegex(ValueError, "symlink"):
            bundle.publish(new, link)
        self.assertTrue(new.exists())
        self.assertTrue(other.exists())

    def test_publish_restores_old_bundle_on_failure(self):
        stage = self.root / "stage"
        stage.mkdir()
        old = self.app(self.root / "soi.app")
        (old / "old").touch()
        new = self.app(stage / "soi.app")
        rename = Path.rename

        def fail_publish(source, destination):
            if source == new:
                raise OSError("simulated rename failure")
            return rename(source, destination)

        with patch.object(Path, "rename", fail_publish):
            with self.assertRaisesRegex(OSError, "simulated"):
                bundle.publish(new, old)
        self.assertTrue((old / "old").is_file())
        self.assertTrue(new.exists())

    def test_failed_rollback_preserves_backup_outside_staging(self):
        stage = self.root / "stage"
        stage.mkdir()
        old = self.app(self.root / "soi.app")
        (old / "old").touch()
        new = self.app(stage / "soi.app")
        backup = self.root / "stage-previous.app"
        rename = Path.rename

        def fail_both(source, destination):
            if source in (new, backup):
                raise OSError("simulated failure")
            return rename(source, destination)

        with patch.object(Path, "rename", fail_both):
            with self.assertRaisesRegex(RuntimeError, "previous app preserved"):
                bundle.publish(new, old)
        bundle.shutil.rmtree(stage)
        self.assertTrue((backup / "old").is_file())


if __name__ == "__main__":
    unittest.main()
