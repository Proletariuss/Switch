"""Exercise the real source snapshot helper without a usable Go compiler."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[1] / "capture-optimization-baseline.rb"
BUILD = SOURCE.parents[1].parent / "build/optimization/baseline-helper-tests"
BUILD.mkdir(parents=True, exist_ok=True)


class SourceBaselineTests(unittest.TestCase):
    def test_native_capture_verification_and_drift_without_go(self):
        with tempfile.TemporaryDirectory(dir=BUILD) as directory:
            workspace = Path(directory)
            source = workspace / "src"
            (source / "scripts").mkdir(parents=True)
            script = source / "scripts" / SOURCE.name
            shutil.copy2(SOURCE, script)
            (source / "VERSION").write_text("1.0\n")
            (source / "BUILD_NUMBER").write_text("1\n")
            sample = source / "sample.txt"
            sample.write_text("committed baseline\n")
            marker = workspace / "go-was-invoked"
            tools = workspace / "tools"
            tools.mkdir()
            go = tools / "go"
            go.write_text('#!/bin/sh\nprintf invoked > "$SNAPSHOT_GO_MARKER"\nexit 97\n')
            go.chmod(0o700)
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                               SNAPSHOT_GO_MARKER=str(marker), RUSTUP_TOOLCHAIN="1.98.0")

            def run(*arguments):
                return subprocess.run(arguments, cwd=source, env=environment,
                                      capture_output=True, text=True, timeout=60)

            for arguments in [
                ("git", "init", "--quiet"),
                ("git", "add", "."),
                ("git", "-c", "user.name=Snapshot fixture", "-c", "user.email=fixture@invalid",
                 "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                 "commit", "--quiet", "-m", "fixture"),
            ]:
                result = run(*arguments)
                self.assertEqual(result.returncode, 0, result.stderr)
            # The snapshot must preserve the actual dirty and untracked inputs.
            sample.write_text("dirty working source\n")
            (source / "new.txt").write_text("untracked source\n")
            destination = workspace / "build/optimization/capture"
            result = run("ruby", str(script), "capture", str(destination))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(marker.exists(), "snapshot required the retired Go compiler")
            manifest = json.loads((destination / "manifest.json").read_text())
            self.assertIn("rustc", manifest["toolchain"])
            self.assertNotIn("go", manifest["toolchain"])
            self.assertIn("new.txt", {entry["path"] for entry in manifest["files"]})
            with tarfile.open(destination / "source.tar.gz") as archive:
                self.assertEqual(archive.extractfile("sample.txt").read(), b"dirty working source\n")
                self.assertEqual(archive.extractfile("new.txt").read(), b"untracked source\n")
            result = run("ruby", str(script), "verify", str(destination))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(json.loads(result.stdout)["archive_intact"])
            sample.write_text("changed after capture\n")
            result = run("ruby", str(script), "verify", str(destination))
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("sample.txt", json.loads(result.stdout)["changed_or_missing_files"])
            self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
