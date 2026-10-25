"""Offline source-preflight fixtures; no upstream checkout or network."""

import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "verify-carrier-source-local.py"
SPEC = importlib.util.spec_from_file_location("local_carrier_gate", SCRIPT)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)
TEST_RUNTIME = Path(__file__).resolve().parents[3] / "build/runtime/local-carrier-gate-tests"
TEST_RUNTIME.mkdir(parents=True, exist_ok=True)
REVISION = "a" * 40


def fixture(root):
    data = root / "data/carriers"
    manifest_dir = root / "docs/optimization/all-rust/carrier-reference-locks"
    for name in ("profiles", "selectors", "compatibility", "user_templates"):
        (data / name).mkdir(parents=True)
    manifest_dir.mkdir(parents=True)
    contents = {
        "LICENSE": b"MIT fixture\n",
        "profiles/a.yaml": b"id: a\n",
        "selectors/12345.yaml": b"plmn: 12345\n",
        "README.md": b"upstream fixture\n",
        "index.yaml": b"profiles: []\n",
        "inventory.yaml": b"count: 1\n",
    }
    for path, raw in contents.items():
        if path in ("README.md", "index.yaml", "inventory.yaml"):
            continue
        installed = (data / "ios-carrier-profiles-MIT.txt") if path == "LICENSE" else data / path
        installed.write_bytes(raw)
    (data / "README.md").write_text("local notes\n")
    (data / "SOURCES.md").write_text("local provenance\n")
    (data / "user_templates/3_hk.json").write_text("{}\n")
    (data / "compatibility/registry-overlay.json").write_text(json.dumps({
        "schema_version": 1, "upstream_revision": REVISION,
    }))
    lock = {
        "repo": "iniwex5/ios-carrier-profiles", "sha": REVISION,
        "license": {"key": "mit", "name": "MIT License", "spdx_id": "MIT",
                    "url": "https://api.github.com/licenses/mit", "node_id": "MIT-fixture"},
        "files": [{"path": path, "sha256": GATE.FRESHNESS.digest(raw)}
                  for path, raw in contents.items()],
    }
    lock_path = manifest_dir / f"ios-carrier-profiles-{REVISION}-manifest.json"
    lock_path.write_text(json.dumps(lock))
    return data, lock, lock_path


class LocalCarrierGateTests(unittest.TestCase):
    def test_current_corpus_matches_without_upstream_git(self):
        report = GATE.verify()
        self.assertTrue(report["exact_match"])
        self.assertEqual(report["revision"],
                         "09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25")
        self.assertEqual((report["profiles"], report["selectors"], report["files"]),
                         (676, 853, 1530))

    def test_fixture_bytes_and_complete_inventory(self):
        with tempfile.TemporaryDirectory(dir=TEST_RUNTIME) as directory:
            root = Path(directory)
            data, _, _ = fixture(root)
            self.assertEqual(GATE.verify(root)["files"], 3)
            for path in ("ios-carrier-profiles-MIT.txt", "profiles/a.yaml", "selectors/12345.yaml"):
                original = (data / path).read_bytes()
                (data / path).write_bytes(original + b"changed")
                with self.subTest(path=path), self.assertRaisesRegex(ValueError, "differ"):
                    GATE.verify(root)
                (data / path).write_bytes(original)
            (data / "profiles/a.yaml").unlink()
            with self.assertRaisesRegex(ValueError, "differ"):
                GATE.verify(root)

    def test_extra_files_and_symlinks_are_rejected(self):
        for kind in ("extra-profile", "extra-selector", "extra-root", "extra-support",
                     "profile-link", "license-link", "directory-link"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(dir=TEST_RUNTIME) as directory:
                root = Path(directory)
                data, _, _ = fixture(root)
                if kind == "extra-profile":
                    (data / "profiles/extra.yaml").write_text("id: extra\n")
                elif kind == "extra-selector":
                    (data / "selectors/54321.yaml").write_text("plmn: 54321\n")
                elif kind == "extra-root":
                    (data / "unreviewed.yaml").write_text("extra\n")
                elif kind == "extra-support":
                    (data / "compatibility/extra.json").write_text("{}\n")
                elif kind == "profile-link":
                    (data / "profiles/a.yaml").unlink()
                    (data / "profiles/a.yaml").symlink_to(data / "selectors/12345.yaml")
                elif kind == "license-link":
                    (data / "ios-carrier-profiles-MIT.txt").unlink()
                    (data / "ios-carrier-profiles-MIT.txt").symlink_to(data / "README.md")
                else:
                    (data / "profiles/a.yaml").unlink()
                    (data / "profiles").rmdir()
                    (data / "profiles").symlink_to(data / "selectors")
                with self.assertRaises(ValueError):
                    GATE.verify(root)

    def test_lock_schema_spdx_paths_hashes_and_duplicates_fail_closed(self):
        with tempfile.TemporaryDirectory(dir=TEST_RUNTIME) as directory:
            root = Path(directory)
            _, lock, lock_path = fixture(root)
            mutations = {
                "spdx": lambda x: x["license"].update(spdx_id="Apache-2.0"),
                "repo": lambda x: x.update(repo="other/repository"),
                "revision": lambda x: x.update(sha="b" * 40),
                "duplicate": lambda x: x["files"].append(copy.deepcopy(x["files"][0])),
                "traversal": lambda x: x["files"][1].update(path="profiles/../a.yaml"),
                "extra-path": lambda x: x["files"][1].update(path="other/a.yaml"),
                "bad-hash": lambda x: x["files"][1].update(sha256="A" * 64),
                "missing-license": lambda x: x["files"].pop(0),
                "extra-field": lambda x: x.update(unreviewed=True),
                "row-field": lambda x: x["files"][1].update(bytes=6),
            }
            for kind, mutate in mutations.items():
                changed = copy.deepcopy(lock)
                mutate(changed)
                lock_path.write_text(json.dumps(changed))
                with self.subTest(kind=kind), self.assertRaises(ValueError):
                    GATE.verify(root)
            lock_path.write_text(json.dumps(lock))
            lock_path.write_text(lock_path.read_text().replace('"sha":', '"sha":"bad", "sha":', 1))
            with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
                GATE.verify(root)

    def test_overlay_revision_and_lock_symlink_are_rejected(self):
        with tempfile.TemporaryDirectory(dir=TEST_RUNTIME) as directory:
            root = Path(directory)
            data, _, lock_path = fixture(root)
            overlay = data / "compatibility/registry-overlay.json"
            overlay.write_text(json.dumps({"schema_version": 1, "upstream_revision": "../bad"}))
            with self.assertRaisesRegex(ValueError, "revision"):
                GATE.verify(root)
            overlay.write_text(json.dumps({"schema_version": 1, "upstream_revision": REVISION}))
            copied = lock_path.with_suffix(".saved")
            copied.write_bytes(lock_path.read_bytes())
            lock_path.unlink()
            lock_path.symlink_to(copied)
            with self.assertRaisesRegex(ValueError, "regular"):
                GATE.verify(root)


if __name__ == "__main__":
    unittest.main()
