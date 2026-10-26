#!/usr/bin/env python3
"""Pure deletion-plan fixtures; no network or real Release mutations."""
import copy
import json
import tempfile
from types import SimpleNamespace
from unittest import mock
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("compact_release", Path(__file__).with_name("compact-release.py"))
compact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compact)
VERSION, COMMIT = "3.52", "a" * 40

def fixture():
    names = sorted(compact.retained_names(VERSION) | compact.audit_names(VERSION, COMMIT))
    records = [{"name": name, "id": index + 10, "size": 3, "sha256": compact.digest(b"raw")}
               for index, name in enumerate(names)]
    keep = [row for row in records if row["name"] in compact.retained_names(VERSION)]
    remove = [row for row in records if row["name"] in compact.audit_names(VERSION, COMMIT)]
    return keep, remove

class API:
    def __init__(self, records):
        self.rows = [{"name": row["name"], "id": row["id"], "size": row["size"],
                      "digest": "sha256:" + row["sha256"], "state": "uploaded"} for row in records]
        self.deleted = []
    def pages(self, path):
        return copy.deepcopy(self.rows)
    def api(self, method, path, **kwargs):
        if method == "GET":
            return b"raw"
        if method != "DELETE":
            raise AssertionError("unexpected mutation")
        asset_id = int(path.rsplit("/", 1)[1])
        self.deleted.append(asset_id)
        self.rows = [row for row in self.rows if row["id"] != asset_id]

class CompactFixtures(unittest.TestCase):
    def test_exact_five_deleted_two_unchanged(self):
        keep, remove = fixture()
        api = API(keep + remove)
        compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(set(api.deleted), {row["id"] for row in remove})
        self.assertEqual({row["id"] for row in api.rows}, {row["id"] for row in keep})

    def test_partial_delete_resume(self):
        keep, remove = fixture()
        api = API(keep + remove[2:])
        compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(set(api.deleted), {row["id"] for row in remove[2:]})

    def test_completed_is_read_only(self):
        keep, remove = fixture()
        api = API(keep)
        compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(api.deleted, [])

    def test_replaced_audit_id_refuses_before_any_deletion(self):
        keep, remove = fixture()
        api = API(keep + remove)
        api.rows[-1]["id"] += 1000
        with self.assertRaises(RuntimeError):
            compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(api.deleted, [])

    def test_replaced_audit_hash_refuses(self):
        keep, remove = fixture()
        api = API(keep + remove)
        api.rows[-1]["digest"] = "sha256:" + "f" * 64
        with self.assertRaises(RuntimeError):
            compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(api.deleted, [])

    def test_retained_id_change_refuses(self):
        keep, remove = fixture()
        api = API(keep + remove)
        api.rows[0]["id"] += 100
        with self.assertRaises(RuntimeError):
            compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(api.deleted, [])

    def test_unknown_attachment_refuses(self):
        keep, remove = fixture()
        api = API(keep + remove)
        api.rows.append({"name": "user-extra.txt", "id": 99, "size": 3, "digest": "sha256:" + "f" * 64, "state": "uploaded"})
        with self.assertRaises(RuntimeError):
            compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(api.deleted, [])

    def test_duplicate_attachment_refuses(self):
        keep, remove = fixture()
        api = API(keep + remove)
        api.rows.append(copy.deepcopy(api.rows[0]))
        with self.assertRaises(RuntimeError):
            compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove)
        self.assertEqual(api.deleted, [])

    def test_plan_cannot_delete_retained_asset(self):
        keep, remove = fixture()
        api = API(keep + remove)
        with self.assertRaises(RuntimeError):
            compact.remove_planned(api, 42, VERSION, COMMIT, keep, remove + [keep[0]])
        self.assertEqual(api.deleted, [])

    def test_actual_retained_bytes_must_equal_original_artifact(self):
        keep, _ = fixture()
        api = API(keep)
        rows = compact.asset_map(api.rows)
        with self.assertRaises(RuntimeError):
            compact.verify_retained(api, rows, {row["name"]: b"other" for row in keep}, VERSION)

    def test_prepare_requires_complete_five_or_completed_marker(self):
        keep, remove = fixture()
        self.assertEqual(compact.require_snapshot_state(compact.asset_map(API(keep + remove).rows), "", VERSION, COMMIT),
                         compact.audit_names(VERSION, COMMIT))
        for count in range(1, 5):
            with self.subTest(remaining=count), self.assertRaises(RuntimeError):
                compact.require_snapshot_state(compact.asset_map(API(keep + remove[:count]).rows), "", VERSION, COMMIT)
        with self.assertRaises(RuntimeError):
            compact.require_snapshot_state(compact.asset_map(API(keep).rows), "", VERSION, COMMIT)
        body = "<!-- switch-compact:v1 -->complete snapshot link<!-- /switch-compact:v1 -->"
        self.assertEqual(compact.require_snapshot_state(compact.asset_map(API(keep).rows), body, VERSION, COMMIT), set())
        with self.assertRaises(RuntimeError):
            compact.require_snapshot_state(compact.asset_map(API(keep).rows), body + body, VERSION, COMMIT)

    def backup_gate(self, mismatch):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder)
            plan = {"schema_version": 1, "kind": "compact-release-plan", "controller": "c" * 40,
                    "run_id": "123", "run_attempt": "1", "version": VERSION, "app_sha": COMMIT}
            (directory / "cleanup-plan.json").write_text(json.dumps(plan))
            (directory / "packaging-proof.json").write_bytes(b"proof")
            saved = {p.name: p.read_bytes() for p in directory.iterdir()}
            class BackupAPI:
                def __init__(self):
                    self.mutations = []
                def pages(self, path, key=None):
                    if path.endswith("/jobs"):
                        return [{"name": "Release compact verify", "conclusion": "failure",
                                 "steps": [{"name": "Verify retained DMG and preserve audit files", "conclusion": "failure"}]}]
                    return []
                def artifact(self, artifacts, name, run_id, commit):
                    return saved | ({"packaging-proof.json": b"changed"} if mismatch else {})
                def api(self, method, path, *args, **kwargs):
                    if method != "GET":
                        self.mutations.append((method, path))
                    raise AssertionError("backup gate must precede further API operations")
            api = BackupAPI()
            args = SimpleNamespace(directory=directory, version=VERSION, app_sha=COMMIT)
            with mock.patch.dict(compact.os.environ, {"GITHUB_SHA": "c" * 40, "GITHUB_RUN_ID": "123"}):
                with mock.patch.object(compact, "original_dmg") as downstream:
                    with self.assertRaises(RuntimeError):
                        compact.prune(api, args)
                    downstream.assert_not_called()
            self.assertEqual(api.mutations, [])

    def test_snapshot_readback_mismatch_prevents_any_delete(self):
        self.backup_gate(True)

    def test_failed_verify_job_prevents_any_delete(self):
        self.backup_gate(False)

    def test_failure_is_not_successful_publish_evidence(self):
        jobs = [{"name": "dmg / DMG publish", "conclusion": "failure",
                 "steps": [{"name": "Publish verified DMG derivative", "conclusion": "failure"}]}]
        with self.assertRaises(RuntimeError):
            compact.job_succeeded(jobs, "DMG publish", "Publish verified DMG derivative")

if __name__ == "__main__":
    unittest.main()
