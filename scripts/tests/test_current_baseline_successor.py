"""Regression checks for the evidence-only, version-scoped P0 successor.

Source-scope tests run in a receipt-free cold capsule. Historical build and
release evidence is tested only where all pinned external receipts exist.
"""
from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = ROOT.parent
SPEC = importlib.util.spec_from_file_location(
    "current_baseline_successor", ROOT / "scripts/check-current-baseline-successor.py")
assert SPEC and SPEC.loader
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def evidence_path(relative: str) -> Path:
    prefix = "src/" if relative.startswith("docs/") else ""
    return WORKSPACE / (prefix + relative)


def read(relative: str) -> dict:
    return json.loads(evidence_path(relative).read_text())


class SourceScopeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.base = [
            read(CHECK.DOCUMENT.as_posix()),
            read(CHECK.EXPECTED_PINS["predecessor"][0]),
            read(CHECK.LEDGER),
        ]

    def cases(self) -> list[dict]:
        return copy.deepcopy(self.base)

    def assert_rejected(self, cases: list[dict], expected: str) -> None:
        errors = CHECK.validate_source_scope(*cases)
        self.assertTrue(any(expected in error for error in errors), errors)

    def source_fixture(self, root: Path) -> None:
        for relative in [CHECK.DOCUMENT.as_posix(),
                         CHECK.EXPECTED_PINS["predecessor"][0],
                         CHECK.LEDGER, CHECK.ACTIVE_LEDGER]:
            destination = root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, destination)

    def test_source_scope_is_valid_but_awards_no_pass(self) -> None:
        self.assertEqual(CHECK.validate_source_scope(*self.base), [])
        self.assertEqual(CHECK.validate_active_scope(read(CHECK.ACTIVE_LEDGER)), [])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.source_fixture(root)
            self.assertFalse((root.parent / "build").exists())
            result = CHECK.check_source_only(root)
            self.assertTrue(result["valid"], result["errors"])
            self.assertEqual(result["evidence_scope"], "SOURCE_ONLY_NO_BUILD_ATTESTATION")
            self.assertEqual(result["functional_passes_awarded"], 0)
            with self.assertRaises((OSError, ValueError)):
                CHECK.check(root)

    def test_source_only_rejects_partial_external_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.source_fixture(root)
            receipt = root.parent / CHECK.EXPECTED_PINS["restore_receipt"][0]
            receipt.parent.mkdir(parents=True)
            receipt.write_text("{}\n")
            result = CHECK.check_source_only(root)
            self.assertFalse(result["valid"])
            self.assertTrue(any("external evidence" in error for error in result["errors"]))

    def test_source_only_checks_predecessor_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.source_fixture(root)
            predecessor = root / CHECK.EXPECTED_PINS["predecessor"][0]
            with predecessor.open("a") as stream:
                stream.write("\n")
            self.assertIn("predecessor evidence bytes changed",
                          CHECK.check_source_only(root)["errors"])

    def test_source_only_rejects_escaping_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.source_fixture(root)
            predecessor = root / CHECK.EXPECTED_PINS["predecessor"][0]
            predecessor.unlink()
            predecessor.symlink_to(ROOT / CHECK.EXPECTED_PINS["predecessor"][0])
            with self.assertRaisesRegex(ValueError, "unsafe or absent evidence"):
                CHECK.check_source_only(root)

    def test_active_ledger_cannot_promote_i677_receipt(self) -> None:
        active = read(CHECK.ACTIVE_LEDGER)
        for row in active["leaves"]:
            if row["id"] == "P0-1.current-build-baseline":
                row["acceptance_status"] = "PASS"
        self.assertIn("active successor P0 scope or no-pass boundary changed",
                      CHECK.validate_active_scope(active))

    def test_cannot_promote_formal_history(self) -> None:
        cases = self.cases()
        for row in cases[2]["leaves"]:
            if row["id"] == "P0-1.baseline-280":
                row["acceptance_status"] = "PASS"
        self.assert_rejected(cases, "formal 115-leaf ledger")

    def test_cannot_award_current_track(self) -> None:
        cases = self.cases()
        cases[0]["scope_mapping"][0]["functional_pass_awarded"] = True
        self.assert_rejected(cases, "mapping status drift")

    def test_cannot_freeze_denominator_or_add_fields(self) -> None:
        cases = self.cases()
        cases[0]["ledger_integration"]["overall_denominator_frozen"] = True
        self.assert_rejected(cases, "ledger accounting drift")
        cases = self.cases()
        cases[0]["unexpected"] = "PASS"
        self.assert_rejected(cases, "successor fields drift")

    def test_external_receipt_pin_cannot_drift_in_source_scope(self) -> None:
        cases = self.cases()
        cases[0]["scope_mapping"][1]["receipt"]["sha256"] = "0" * 64
        self.assert_rejected(cases, "software_receipt pin drift")

    def test_historical_manifest_cannot_repoint_to_current_release(self) -> None:
        cases = self.cases()
        cases[0]["current_build"]["release_manifest"]["path"] = CHECK.ARCHIVED_RELEASE_MANIFEST
        self.assert_rejected(cases, "release_manifest pin drift")


class FullHistoricalEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        external = [evidence_path(CHECK.evidence_location(name))
                    for name in CHECK.EXTERNAL_PINS]
        present = [path.exists() or path.is_symlink() for path in external]
        if not any(present):
            raise unittest.SkipTest("historical i677 build/dist receipts absent in cold source capsule")
        if not all(present):
            raise AssertionError("partial historical i677 external evidence: "
                                 + ", ".join(str(path) for path, exists in zip(external, present)
                                             if not exists))
        cls.base = [
            read(CHECK.DOCUMENT.as_posix()),
            read(CHECK.EXPECTED_PINS["predecessor"][0]),
            read(CHECK.LEDGER),
            read(CHECK.EXPECTED_PINS["restore_receipt"][0]),
            read(CHECK.EXPECTED_PINS["software_receipt"][0]),
            read(CHECK.EXPECTED_PINS["cold_audit"][0]),
            read(CHECK.EXPECTED_PINS["artifact_audit"][0]),
            read(CHECK.evidence_location("release_manifest")),
        ]

    def cases(self) -> list[dict]:
        return copy.deepcopy(self.base)

    def assert_rejected(self, cases: list[dict], expected: str) -> None:
        errors = CHECK.validate(*cases)
        self.assertTrue(any(expected in error for error in errors), errors)

    def test_actual_evidence_passes_historical_full_check(self) -> None:
        self.assertEqual(CHECK.validate(*self.base), [])
        result = CHECK.check(ROOT)
        self.assertTrue(result["valid"], result["errors"])
        self.assertEqual(result["evidence_scope"], "PINNED_I677_FULL_HISTORICAL_ATTESTATION")

    def test_versioned_manifest_preserves_exact_i677_bytes(self) -> None:
        relative, digest = CHECK.EXPECTED_PINS["release_manifest"]
        self.assertEqual(relative, "dist/release-manifest.json")
        self.assertEqual(CHECK.evidence_location("release_manifest"),
                         "dist/archive/Switch-3.48-macOS27-manifest.json")
        self.assertEqual(CHECK.sha256(evidence_path(CHECK.evidence_location("release_manifest"))), digest)
        self.assertEqual(self.base[0]["current_build"]["release_manifest"],
                         {"path": relative, "sha256": digest})

    def test_restored_source_must_match_all_three_digests_and_modes(self) -> None:
        cases = self.cases()
        cases[3]["source_digests"]["restored"] = "0" * 64
        self.assert_rejected(cases, "current-source restore receipt drift")
        cases = self.cases()
        cases[3]["file_modes_verified"] = 3927
        self.assert_rejected(cases, "current-source restore receipt drift")

    def test_manifest_rows_are_checked(self) -> None:
        cases = self.cases()
        cases[5]["source"][0]["mode"] = 0
        self.assert_rejected(cases, "full cold source manifest drift")

    def test_cannot_claim_product_or_device_acceptance(self) -> None:
        cases = self.cases()
        cases[4]["product_acceptance_complete"] = True
        self.assert_rejected(cases, "current-build software receipt drift")
        cases = self.cases()
        cases[7]["acceptance"]["device_acceptance"] = "PASS"
        self.assert_rejected(cases, "release manifest or DMG binding drift")


if __name__ == "__main__":
    unittest.main()
