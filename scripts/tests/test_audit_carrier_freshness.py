import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "carrier_freshness", Path(__file__).resolve().parents[1] / "audit-carrier-freshness.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)
TEST_RUNTIME = Path(__file__).resolve().parents[3] / "build/runtime/carrier-freshness-tests"
TEST_RUNTIME.mkdir(parents=True, exist_ok=True)


class CarrierFreshnessTests(unittest.TestCase):
    def test_add_remove_and_content_change_are_all_visible(self):
        self.assertEqual(AUDIT.compare({"old": "1", "same": "2", "change": "3"},
                                       {"new": "1", "same": "2", "change": "4"}),
                         {"added": ["new"], "removed": ["old"], "changed": ["change"]})

    def test_lock_rejects_duplicate_omitted_or_wrong_commit_entries(self):
        lock = {"repo": "iniwex5/ios-carrier-profiles", "sha": "a" * 40,
                "license": {"spdx_id": "MIT"}, "files": [{"path": "LICENSE", "sha256": "abc"}]}
        AUDIT.validate_lock(lock, "a" * 40, {"LICENSE": "abc"})
        for mutate in (lambda x: x["files"].append(x["files"][0]),
                       lambda x: x.update(sha="b" * 40), lambda x: x.update(files=[])):
            changed = json.loads(json.dumps(lock))
            mutate(changed)
            with self.assertRaises(ValueError):
                AUDIT.validate_lock(changed, "a" * 40, {"LICENSE": "abc"})

    def test_exact_git_revision_ignores_modified_working_tree(self):
        with tempfile.TemporaryDirectory(dir=TEST_RUNTIME) as directory:
            repo = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.DEVNULL)
            git("init")
            for path, raw in {"LICENSE": b"MIT", "profiles/a.yaml": b"id: old\n",
                              "selectors/45403.yaml": b"plmn: 45403\n"}.items():
                target = repo / path
                target.parent.mkdir(exist_ok=True)
                target.write_bytes(raw)
            git("add", ".")
            git("-c", "user.name=Carrier Audit Test", "-c", "user.email=offline@example.invalid",
                "commit", "-m", "fixture")
            revision = git("rev-parse", "HEAD").decode().strip()
            (repo / "profiles/a.yaml").write_text("id: modified\n")
            source = AUDIT.upstream_files(repo, revision)
            self.assertEqual(source["profiles/a.yaml"], AUDIT.digest(b"id: old\n"))
            with self.assertRaises(ValueError):
                AUDIT.upstream_files(repo, "HEAD")

    def test_local_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory(dir=TEST_RUNTIME) as directory:
            root = Path(directory)
            (root / "profiles").mkdir()
            (root / "selectors").mkdir()
            (root / "outside.yaml").write_text("id: outside\n")
            (root / "profiles/a.yaml").symlink_to(root / "outside.yaml")
            with self.assertRaises(ValueError):
                AUDIT.local_files(root)

    def test_compatibility_profiles_rules_and_attribution_are_frozen(self):
        root = Path(__file__).resolve().parents[2]
        raw = (root / "rust/testdata/carrier-history/carrier-registry.json").read_bytes()
        overlay = json.loads((root / "data/carriers/compatibility/registry-overlay.json").read_text())
        lock = json.loads((root / "docs/optimization/all-rust/carrier-reference-locks" /
                           ("ios-carrier-profiles-" + overlay["source_revision"] + "-manifest.json")).read_text())
        AUDIT.validate_compatibility(overlay, raw, lock)
        for field in ("profile", "rule", "hash"):
            changed = json.loads(json.dumps(overlay))
            if field == "profile":
                next(iter(changed["profiles"].values()))["bundle"] = "Unreviewed.bundle"
            elif field == "rule":
                next(iter(changed["selectors"].values()))["profiles"][0]["selectors"][0]["raw"] = "unreviewed"
            else:
                changed["source_files"][0]["sha256"] = "0" * 64
            with self.subTest(field=field), self.assertRaises(ValueError):
                AUDIT.validate_compatibility(changed, raw, lock)

    def test_historical_capture_input_hashes_are_unchanged(self):
        root = Path(__file__).resolve().parents[2] / "rust/testdata/carrier-history"
        manifest = json.loads((root / "manifest.json").read_text())
        for row in manifest["files"]:
            raw = (root / row["path"]).read_bytes()
            self.assertEqual(len(raw), row["bytes"])
            self.assertEqual(AUDIT.digest(raw), row["sha256"])


if __name__ == "__main__":
    unittest.main()
