#!/usr/bin/env python3
"""Offline synthetic fixtures for the release verifier; never contact GitHub."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("publisher", Path(__file__).with_name("publish-release.py"))
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)
COMMIT, TREE, RUN = "a" * 40, "b" * 40, 12345
VERSION, BUILD = "3.52", "352"

def encode(value):
    return (json.dumps(value, indent=2) + "\n").encode()

def fixture():
    content = b"synthetic offline fixture\n"
    name = "Switch-3.52-candidate-" + COMMIT + "-macOS27-arm64.app.tar.gz"
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        entry = tarfile.TarInfo("Switch.app/Contents/MacOS/Switch")
        entry.mode, entry.size = 0o755, len(content)
        archive.addfile(entry, io.BytesIO(content))
    files = {name: buffer.getvalue()}
    for gate in publisher.GATES:
        files["test-reports/" + gate + ".json"] = encode({
            "gate": gate, "status": "passed", "exit_code": 0, "commit": COMMIT, "run_id": str(RUN), "run_attempt": "1"})
    manifest = {"schema_version": 1, "kind": "ci-app-candidate", "version": VERSION, "build": BUILD,
        "commit": COMMIT, "tree": TREE, "source_digest": "c" * 64, "run": {"id": str(RUN), "attempt": "1"},
        "identity_validation": {"app_version": VERSION, "build_number": BUILD, "backend_version": VERSION + ".0",
                                "source_digest": "c" * 64, "scope": "plist-and-embedded-identity"},
        "acceptance": {"product_acceptance_complete": False, "device_acceptance": "UNVERIFIED",
                       "macos27_runtime": "UNVERIFIED", "signature": "ad-hoc", "notarized": False, "original_v3_51_release": False},
        "artifact": {"name": name, "size": len(files[name]), "sha256": publisher.digest(files[name])},
        "app_inventory": [{"path": "Switch.app/Contents/MacOS/Switch", "kind": "file", "mode": 0o755,
                           "size": len(content), "sha256": publisher.digest(content)}],
        "reports_sha256": {key.split("/", 1)[1]: publisher.digest(value) for key, value in files.items() if key.startswith("test-reports/")}}
    manifest["app_tree_sha256"] = publisher.digest(json.dumps(manifest["app_inventory"], sort_keys=True, separators=(",", ":")).encode())
    seal(files, manifest)
    reports = {"upload-roundtrip.json": encode({"gate": "upload-roundtrip", "status": "passed", "exit_code": 0,
               "commit": COMMIT, "run_id": str(RUN), "run_attempt": "1"}), "upload-roundtrip.log": b"synthetic success\n"}
    return files, manifest, reports

def seal(files, manifest):
    files["candidate-manifest.json"] = encode(manifest)
    name = manifest["artifact"]["name"]
    files["SHA256SUMS"] = (publisher.digest(files[name]) + "  " + name + "\n" +
        publisher.digest(files["candidate-manifest.json"]) + "  candidate-manifest.json\n").encode()

def validate(files, reports, **overrides):
    values = dict(commit=COMMIT, tree=TREE, version=VERSION, build=BUILD, run_id=RUN, attempt=1)
    values.update(overrides)
    return publisher.validate_payload(files, reports, **values)

class ReleaseFixtures(unittest.TestCase):
    def test_valid_four_asset_whitelist_and_deterministic_retry(self):
        files, _, reports = fixture()
        first, _ = validate(files, reports)
        second, _ = validate(files, reports)
        self.assertEqual(first, second)
        self.assertEqual(set(first), {next(name for name in files if name.endswith(".app.tar.gz")),
                                     "candidate-manifest.json", "SHA256SUMS", "test-reports.tar.gz"})
        with tarfile.open(fileobj=io.BytesIO(first["test-reports.tar.gz"])) as archive:
            self.assertIn("test-reports/upload-roundtrip.json", archive.getnames())

    def test_exact_source_tree_version_build_run_attempt(self):
        for field, value in [("commit", "d" * 40), ("tree", "e" * 40), ("version", "3.53"),
                             ("build", "353"), ("run_id", 9), ("attempt", 2)]:
            with self.subTest(field=field):
                files, _, reports = fixture()
                with self.assertRaises(RuntimeError):
                    validate(files, reports, **{field: value})

    def test_app_tamper_is_rejected(self):
        files, manifest, reports = fixture()
        files[manifest["artifact"]["name"]] += b"tampered"
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_archive_mode_is_checked_even_with_consistent_manifest_hashes(self):
        files, manifest, reports = fixture()
        manifest["app_inventory"][0]["mode"] = 0o644
        manifest["app_tree_sha256"] = publisher.digest(json.dumps(manifest["app_inventory"], sort_keys=True, separators=(",", ":")).encode())
        seal(files, manifest)
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_failed_gate_cannot_be_hidden_by_rehashing(self):
        files, manifest, reports = fixture()
        path = "test-reports/rust.json"
        report = json.loads(files[path])
        report.update(status="failed", exit_code=1)
        files[path] = encode(report)
        manifest["reports_sha256"]["rust.json"] = publisher.digest(files[path])
        seal(files, manifest)
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_roundtrip_failure_or_wrong_source_rejected(self):
        for change in [{"status": "failed"}, {"commit": "f" * 40}, {"run_attempt": "2"}]:
            files, _, reports = fixture()
            report = json.loads(reports["upload-roundtrip.json"])
            report.update(change)
            reports["upload-roundtrip.json"] = encode(report)
            with self.assertRaises(RuntimeError):
                validate(files, reports)

    def test_missing_roundtrip_rejected(self):
        files, _, reports = fixture()
        del reports["upload-roundtrip.json"]
        with self.assertRaises(KeyError):
            validate(files, reports)

    def test_unexpected_candidate_file_rejected(self):
        files, _, reports = fixture()
        files["credentials.txt"] = b"synthetic unexpected data"
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_identity_or_acceptance_drift_rejected(self):
        for area, key, value in [("identity_validation", "backend_version", "3.51.0"),
                                 ("acceptance", "product_acceptance_complete", True)]:
            files, manifest, reports = fixture()
            manifest[area][key] = value
            seal(files, manifest)
            with self.assertRaises(RuntimeError):
                validate(files, reports)

    def test_checksum_lines_must_match_exactly(self):
        files, _, reports = fixture()
        files["SHA256SUMS"] += b"extra checksum line\n"
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_same_asset_retry_succeeds_but_conflict_never_replaces(self):
        publisher.same_asset(b"same", b"same")
        for old in [None, b"different", b""]:
            with self.assertRaises(RuntimeError):
                publisher.same_asset(old, b"same")

    def test_zip_traversal_and_duplicates_rejected(self):
        for names in [["../escape"], ["/absolute"], ["same", "same"]]:
            output = io.BytesIO()
            with zipfile.ZipFile(output, "w") as archive:
                for name in names:
                    archive.writestr(name, b"synthetic")
            with self.assertRaises(RuntimeError):
                publisher.checked_zip(output.getvalue())

if __name__ == "__main__":
    unittest.main()
