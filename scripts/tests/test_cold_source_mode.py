"""Cold source-only gate must be tied to an actual audited capsule."""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "cold_source_mode", ROOT / "scripts/check-cold-source-mode.py")
assert SPEC and SPEC.loader
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class ColdSourceModeTests(unittest.TestCase):
    def fixture(self, directory: str) -> tuple[Path, Path]:
        workspace = Path(directory)
        original = workspace / "src"
        original.mkdir()
        capsule = workspace / "build/clean-build/probe"
        source = capsule / "src"
        (source / "scripts").mkdir(parents=True)
        entries = []
        for relative in CHECK.REQUIRED_SCRIPTS:
            target = source / relative
            shutil.copy2(ROOT / relative, target)
            entries.append({"path": relative,
                            "sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
                            "bytes": target.stat().st_size,
                            "mode": stat.S_IMODE(target.stat().st_mode)})
        (capsule / "audit.json").write_text(json.dumps({
            "schema": 1, "capsule": str(capsule),
            "source_root": str(original), "source": entries}) + "\n")
        return source, original

    def test_audited_capsule_source_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, _ = self.fixture(directory)
            self.assertEqual(CHECK.check(source), [])
            result = subprocess.run(["python3", str(ROOT / "scripts/check-cold-source-mode.py"),
                                     "--source-root", str(source)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_primary_workspace_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            _, primary = self.fixture(directory)
            self.assertIn("requires build/clean-build", " ".join(CHECK.check(primary)))
            (primary / "scripts").mkdir()
            for relative in (*CHECK.REQUIRED_SCRIPTS, "scripts/workspace-env.sh"):
                shutil.copy2(ROOT / relative, primary / relative)
            result = subprocess.run(["/bin/sh", str(primary / CHECK.REQUIRED_SCRIPTS[0])],
                                    env={**os.environ, "SWITCH_COLD_SOURCE_ONLY": "1"},
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires build/clean-build", result.stdout + result.stderr)
            self.assertNotIn("[checks]", result.stdout + result.stderr)

    def test_marker_must_bind_capsule_and_original_source(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, _ = self.fixture(directory)
            marker = source.parent / "audit.json"
            original = marker.read_text()
            for key, value in (("capsule", "/tmp/unrelated"),
                               ("source_root", "/tmp/unrelated/src")):
                audit = json.loads(original)
                audit[key] = value
                marker.write_text(json.dumps(audit))
                self.assertIn("identity", " ".join(CHECK.check(source)))
            marker.write_text(original)
            self.assertEqual(CHECK.check(source), [])

    def test_marker_and_script_bytes_are_required(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, _ = self.fixture(directory)
            marker = source.parent / "audit.json"
            original = marker.read_text()
            marker.unlink()
            self.assertIn("audit.json", " ".join(CHECK.check(source)))
            marker.write_text(original)
            with (source / CHECK.REQUIRED_SCRIPTS[0]).open("a") as stream:
                stream.write("\n")
            self.assertIn("source entry differs", " ".join(CHECK.check(source)))

    def test_symlinked_source_and_missing_manifest_entry_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, _ = self.fixture(directory)
            link = source.parent / "source-link"
            link.symlink_to(source, target_is_directory=True)
            self.assertIn("symlink", " ".join(CHECK.check(link)))
            marker = source.parent / "audit.json"
            audit = json.loads(marker.read_text())
            audit["source"] = audit["source"][:1]
            marker.write_text(json.dumps(audit))
            self.assertIn("unique safe source entry", " ".join(CHECK.check(source)))


if __name__ == "__main__":
    unittest.main()
