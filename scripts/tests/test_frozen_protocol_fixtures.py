#!/usr/bin/env python3
"""Negative checks for losing or weakening independent frozen expectations."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("frozen_oracles", ROOT / "scripts/test-frozen-protocol-fixtures.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FrozenOracles(unittest.TestCase):
    def setUp(self):
        base = ROOT.parent / "build/tmp"
        base.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(prefix="frozen-oracle-", dir=base)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.rows = [{"name": "expected", "value": "independent bytes"}]
        self.write_fixture()

    def write_fixture(self):
        raw = json.dumps(self.rows).encode()
        (self.root / "fixture.json").write_bytes(raw)
        self.manifest = {"schema_version": 1, "capture_iteration": 511, "fixtures": [{
            "file": "fixture.json", "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw),
            "case_count": 1, "provenance": "go-computed", "source_files_sha256": {"old.go": "frozen-source-hash"},
        }]}
        self.write_manifest()

    def write_manifest(self):
        (self.root / "manifest.json").write_text(json.dumps(self.manifest))

    def validate(self):
        return module.validate_manifest(self.root, "manifest.json", {"fixture.json": 1})

    def test_known_frozen_file(self):
        self.assertEqual(self.validate()["fixture.json"], self.rows)

    def test_missing_and_tampered_fixture(self):
        (self.root / "fixture.json").unlink()
        with self.assertRaises(OSError): self.validate()
        (self.root / "fixture.json").write_text("[]")
        with self.assertRaisesRegex(ValueError, "hash/length"): self.validate()

    def test_changed_count_is_not_rebaselined_by_updating_hash(self):
        self.rows = []
        self.write_fixture()
        self.manifest["fixtures"][0]["case_count"] = 0
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "coverage"): self.validate()

    def test_duplicate_missing_and_traversal_entries(self):
        entry = self.manifest["fixtures"][0]
        for entries in [[], [entry, entry], [{**entry, "file": "../fixture.json"}]]:
            self.manifest["fixtures"] = entries
            self.write_manifest()
            with self.assertRaises(ValueError): self.validate()

    def test_provenance_and_schema_required(self):
        self.manifest["schema_version"] = 2
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "schema"): self.validate()
        self.manifest["schema_version"] = 1
        self.manifest["fixtures"][0]["source_files_sha256"] = {}
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "provenance"): self.validate()

    def test_symlink_rejected(self):
        (self.root / "fixture.json").rename(self.root / "saved.json")
        (self.root / "fixture.json").symlink_to("saved.json")
        with self.assertRaisesRegex(ValueError, "symlinks"): self.validate()

    def test_missing_duplicate_or_weaker_protocol_expectation(self):
        data = json.loads((module.FIXTURES / "amr-wb.json").read_text())
        module.validate_protocol("amr-wb.json", data)
        first = data["rows"][0]
        first["expected_prefix"] = first.pop("expected_stdout")[:2]
        with self.assertRaisesRegex(ValueError, "weakened"): module.validate_protocol("amr-wb.json", data)
        first.pop("expected_prefix")
        with self.assertRaisesRegex(ValueError, "missing/ambiguous"): module.validate_protocol("amr-wb.json", data)
        first["expected_stdout"] = "x"
        data["rows"][1]["name"] = first["name"]
        with self.assertRaisesRegex(ValueError, "duplicate"): module.validate_protocol("amr-wb.json", data)

    def test_exact_wire_comparison_and_sdp_capabilities(self):
        module.compare_result("fixture", {"name": "wire", "expected_stdout": "YQ=="}, "YQ==\n")
        with self.assertRaises(AssertionError): module.compare_result("fixture", {"name": "wire", "expected_stdout": "YQ=="}, " YQ==\n")
        data = json.loads((module.FIXTURES / "sdp-admission.json").read_text())
        module.validate_protocol("sdp-admission.json", data)
        data["rows"][0]["args"].pop()
        with self.assertRaisesRegex(ValueError, "explicit"): module.validate_protocol("sdp-admission.json", data)


if __name__ == "__main__":
    unittest.main()
