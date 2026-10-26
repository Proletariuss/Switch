#!/usr/bin/env python3
"""Pure deletion-plan fixtures; no network or real Release mutations."""
import copy
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

    def test_failure_is_not_successful_publish_evidence(self):
        jobs = [{"name": "dmg / DMG publish", "conclusion": "failure",
                 "steps": [{"name": "Publish verified DMG derivative", "conclusion": "failure"}]}]
        with self.assertRaises(RuntimeError):
            compact.job_succeeded(jobs, "DMG publish", "Publish verified DMG derivative")

if __name__ == "__main__":
    unittest.main()
