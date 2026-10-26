#!/usr/bin/env python3
"""Pure fixtures; real hdiutil/codesign acceptance runs only in the DMG job."""
import copy
import importlib.util
import json
import os
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("package_dmg", Path(__file__).with_name("package-dmg.py"))
dmg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dmg)

class DMGFixtures(unittest.TestCase):
    def rows(self):
        return [{"path": "Switch.app", "kind": "directory", "mode": 0o755},
                {"path": "Switch.app/file", "kind": "file", "mode": 0o755, "size": 3,
                 "sha256": dmg.digest(b"app")}]

    def files(self):
        source = {"commit": "a" * 40, "tree": "b" * 40, "version": "3.52", "build": "352",
                  "source_digest": "c" * 64, "manifest_sha256": "d" * 64, "app_archive_sha256": "e" * 64,
                  "app_tree_sha256": "f" * 64, "release_id": 42, "tag": "v3.52"}
        name = "Switch_3.52.dmg"
        proof = {"schema_version": 1, "kind": "release-dmg-derivative", "source": source,
                 "packaging": {"commit": "1" * 40, "run_id": "123", "run_attempt": "1",
                               "run_url": "https://github.com/Proletariuss/Switch/actions/runs/123",
                               "workflow": ".github/workflows/package-dmg.yml",
                               "reusable_workflow": ".github/workflows/package-dmg.yml"},
                 "dmg": {"name": name, "size": 3, "sha256": dmg.digest(b"dmg")},
                 "validation": {"input_archive": True, "mounted_inventory": True, "signature": True,
                                "readonly_mount": True, "app_recompiled": False},
                 "acceptance": {"signature": "ad-hoc", "notarized": False,
                                "macos27_runtime": "UNVERIFIED", "device_acceptance": "UNVERIFIED"}}
        files = {name: b"dmg", name + ".sha256": (dmg.digest(b"dmg") + "  " + name + "\n").encode(),
                 "dmg-provenance.json": json.dumps(proof).encode()}
        return source, files, proof

    def test_valid_inventory(self):
        self.assertEqual(len(dmg.check_inventory(self.rows())), 2)

    def test_duplicate(self):
        rows = self.rows()
        with self.assertRaises(RuntimeError):
            dmg.check_inventory(rows + [rows[1]])

    def test_unsafe_paths(self):
        for name in ("/Switch.app/file", "Switch.app/../file", "Switch.app//file", "Switch.app/./file", "Else.app/file"):
            rows = self.rows()
            rows[1]["path"] = name
            with self.subTest(name=name), self.assertRaises(RuntimeError):
                dmg.check_inventory(rows)

    def test_symlink_escape(self):
        for target in ("/etc/passwd", "../../outside"):
            rows = self.rows()
            rows[1] = {"path": "Switch.app/link", "kind": "symlink", "mode": 0o755, "target": target}
            with self.subTest(target=target), self.assertRaises(RuntimeError):
                dmg.check_inventory(rows)

    def test_symlink_parent(self):
        rows = self.rows() + [{"path": "Switch.app/link", "kind": "symlink", "mode": 0o755, "target": "file"},
                             {"path": "Switch.app/link/child", "kind": "file", "mode": 0o644, "size": 1}]
        with self.assertRaises(RuntimeError):
            dmg.check_inventory(rows)

    def test_internal_symlink(self):
        rows = self.rows() + [{"path": "Switch.app/link", "kind": "symlink", "mode": 0o755, "target": "file"}]
        self.assertEqual(len(dmg.check_inventory(rows)), 3)

    def test_resolved_internal_link(self):
        with tempfile.TemporaryDirectory() as folder:
            app = Path(folder) / "Switch.app"
            app.mkdir()
            (app / "file").write_bytes(b"accepted")
            os.symlink("file", app / "link")
            dmg.check_resolved_links(app)

    def test_resolved_symlink_chain_escape(self):
        with tempfile.TemporaryDirectory() as folder:
            app = Path(folder) / "Switch.app"
            app.mkdir()
            (Path(folder) / "outside").write_bytes(b"outside")
            os.symlink(".", app / "a")
            os.symlink("a/../outside", app / "b")
            with self.assertRaises(RuntimeError):
                dmg.check_resolved_links(app)

    def test_resolved_dangling_or_cyclic_link(self):
        for target in ("missing", "link"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as folder:
                app = Path(folder) / "Switch.app"
                app.mkdir()
                os.symlink(target, app / "link")
                with self.assertRaises(RuntimeError):
                    dmg.check_resolved_links(app)

    def test_expansion_bound(self):
        rows = self.rows()
        rows[1]["size"] = dmg.LIMIT + 1
        with self.assertRaises(RuntimeError):
            dmg.check_inventory(rows)

    def test_exact_derivative(self):
        source, files, proof = self.files()
        self.assertEqual(dmg.validate_derivative(files, source), proof)

    def test_changed_dmg(self):
        source, files, _ = self.files()
        files["Switch_3.52.dmg"] = b"bad"
        with self.assertRaises(RuntimeError):
            dmg.validate_derivative(files, source)

    def test_changed_checksum(self):
        source, files, _ = self.files()
        files["Switch_3.52.dmg.sha256"] = b"unbound"
        with self.assertRaises(RuntimeError):
            dmg.validate_derivative(files, source)

    def test_source_controller_separate(self):
        source, files, proof = self.files()
        self.assertNotEqual(proof["source"]["commit"], proof["packaging"]["commit"])
        wrong = copy.deepcopy(source)
        wrong["commit"] = proof["packaging"]["commit"]
        with self.assertRaises(RuntimeError):
            dmg.validate_derivative(files, wrong)

    def test_scope_rejects_recompile_claim(self):
        source, files, proof = self.files()
        proof["validation"]["app_recompiled"] = True
        files["dmg-provenance.json"] = json.dumps(proof).encode()
        with self.assertRaises(RuntimeError):
            dmg.validate_derivative(files, source)

    def test_extra_asset(self):
        source, files, _ = self.files()
        files["unexpected"] = b"x"
        with self.assertRaises(RuntimeError):
            dmg.validate_derivative(files, source)

    def test_release_matches_original_workflow_artifacts(self):
        manifest = {"run": {"id": "123", "attempt": "1"}, "commit": "a" * 40,
                    "tree": "b" * 40, "version": "3.52", "build": "352"}
        original = {"asset": b"accepted"}
        class API:
            def pages(self, path, key=None):
                return []
            def artifact(self, artifacts, name, run_id, commit):
                return {}
        with patch.object(dmg.transport, "validate_payload", return_value=(original, manifest)):
            dmg.bind_original_release(API(), manifest, original)

    def test_self_consistent_rewritten_release_rejected(self):
        manifest = {"run": {"id": "123", "attempt": "1"}, "commit": "a" * 40,
                    "tree": "b" * 40, "version": "3.52", "build": "352"}
        class API:
            def pages(self, path, key=None):
                return []
            def artifact(self, artifacts, name, run_id, commit):
                return {}
        with patch.object(dmg.transport, "validate_payload", return_value=({"asset": b"original"}, manifest)):
            with self.assertRaises(RuntimeError):
                dmg.bind_original_release(API(), manifest, {"asset": b"rewritten"})

    def test_success_booleans_without_job_evidence_rejected(self):
        _, files, proof = self.files()
        class API:
            def api(self, method, path):
                return {"head_sha": "1" * 40, "path": ".github/workflows/package-dmg.yml",
                        "repository": {"full_name": dmg.REPO}, "event": "workflow_dispatch", "run_attempt": 1}
            def pages(self, path, key=None):
                return []
            def artifact(self, artifacts, name, run_id, commit):
                return files
        with patch.dict(dmg.os.environ, {"GITHUB_SHA": "1" * 40}), self.assertRaises(RuntimeError):
            dmg.verify_controller(API(), proof, False, files)

if __name__ == "__main__":
    unittest.main()
