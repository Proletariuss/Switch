"""Negative checks for the adopted but unaccepted current P0-1 baseline scope."""
from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "p0_1_current_baseline_activation",
    ROOT / "scripts/check-p0-1-current-baseline-activation.py")
assert SPEC and SPEC.loader
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class P01ActivationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.original = CHECK.load_documents(ROOT)

    def documents(self) -> dict:
        return copy.deepcopy(self.original)

    def reject(self, docs: dict, marker: str) -> None:
        errors = CHECK.validate(docs)
        self.assertTrue(any(marker in item for item in errors), errors)

    def source_fixture(self, root: Path) -> None:
        for relative in [*(path for path, _ in CHECK.FILES.values()),
                         CHECK.ACTIVE, CHECK.ENTRY, CHECK.PROTOCOL]:
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, target)

    def test_actual_scope_is_active_but_unaccepted(self) -> None:
        self.assertEqual(CHECK.validate(self.original), [])
        result = CHECK.check(ROOT)
        self.assertTrue(result["valid"], result["errors"])
        self.assertTrue(result["active"])
        self.assertEqual(result["catalog_leaves"], 115)
        self.assertFalse(result["overall_denominator_frozen"])
        self.assertEqual(result["functional_passes_awarded"], 0)

    def test_historical_leaf_cannot_be_promoted(self) -> None:
        docs = self.documents()
        docs["historical_i677"]["leaves"][1]["acceptance_status"] = "PASS"
        self.reject(docs, "historical P0 leaves changed or promoted")

    def test_active_leaf_cannot_be_promoted(self) -> None:
        docs = self.documents()
        docs["active"]["leaves"][1]["acceptance_status"] = "PASS"
        self.reject(docs, "active catalog count or PASS drift")

    def test_adopted_rule_cannot_expand_to_clean_install(self) -> None:
        docs = self.documents()
        docs["activation"]["replacement_rules"][1]["scenario_class_ids"].append("CLEAN")
        self.reject(docs, "replacement rules or scenario scope drift")

    def test_old_leaf_cannot_reenter_active_ledger(self) -> None:
        docs = self.documents()
        docs["active"]["leaves"][0]["id"] = CHECK.OLD_IDS[0]
        self.reject(docs, "active P0 leaf identity drift")

    def test_active_leaf_cannot_claim_i677_as_current_target_pass(self) -> None:
        docs = self.documents()
        docs["active"]["leaves"][0]["evidence"]["normal"]["status"] = "PASS"
        self.reject(docs, "active P0 leaf content differs")

    def test_predecessor_sha_cannot_be_rebound(self) -> None:
        docs = self.documents()
        docs["activation"]["historical_i677_ledger"]["sha256"] = "0" * 64
        self.reject(docs, "activation predecessor or candidate pin drift")

    def test_candidate_cannot_be_relabelled_as_active(self) -> None:
        docs = self.documents()
        docs["inactive_candidate"]["candidate_scope_change"]["status"] = "ACTIVE"
        self.reject(docs, "inactive candidate drift")

    def test_entry_mapping_cannot_retain_old_leaf(self) -> None:
        docs = self.documents()
        docs["entry"]["required_leaf_ids"][0] = CHECK.OLD_IDS[0]
        self.reject(docs, "active entry-map requirement set drift")

    def test_protocol_cannot_restore_old_active_rule(self) -> None:
        docs = self.documents()
        old_id = CHECK.OLD_IDS[0]
        docs["protocol"]["P0_normal_rules"][old_id] = docs["protocol"]["historical_P0_normal_rules"]["archived_rules"][old_id]
        self.reject(docs, "active scope protocol P0 rule set drift")

    def test_fabricated_overall_denominator_is_rejected(self) -> None:
        docs = self.documents()
        docs["active"]["freeze"]["overall_denominator_frozen"] = True
        docs["active"]["freeze"]["overall_percentage"] = 100
        self.reject(docs, "active scope activation or denominator drift")

    def test_version_scoped_i677_receipt_cannot_claim_new_version(self) -> None:
        docs = self.documents()
        docs["activation"]["historical_limits"]["new_target_version_needs_matching_receipts"] = False
        self.reject(docs, "historical or device/product limitation drift")

    def test_other_113_leaf_objects_are_byte_identical(self) -> None:
        before = ROOT / CHECK.FILES["pre_activation_i680"][0]
        active = ROOT / CHECK.ACTIVE
        self.assertEqual(CHECK.raw_leaf_objects(before)[2:], CHECK.raw_leaf_objects(active)[2:])
        self.assertEqual(CHECK.other_leaf_digest(active), CHECK.OTHER_113_SHA)
        with tempfile.TemporaryDirectory() as directory:
            modified = Path(directory) / "ledger.json"
            original = active.read_bytes()
            leaf = CHECK.raw_leaf_objects(active)[2]
            changed = leaf.replace(b'"title":', b'"title" :', 1)
            self.assertNotEqual(leaf, changed)
            modified.write_bytes(original.replace(leaf, changed, 1))
            self.assertEqual(json.loads(modified.read_bytes()), self.original["active"])
            self.assertNotEqual(CHECK.other_leaf_digest(modified), CHECK.OTHER_113_SHA)

    def test_immutable_i677_archive_tamper_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.source_fixture(root)
            archived = root / CHECK.FILES["historical_i677"][0]
            with archived.open("a") as stream:
                stream.write("\n")
            result = CHECK.check(root)
            self.assertIn("historical_i677 immutable source bytes changed", result["errors"])

    def test_missing_historical_archive_cannot_fall_back_to_current(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.source_fixture(root)
            (root / CHECK.FILES["historical_i677"][0]).unlink()
            with self.assertRaises(FileNotFoundError):
                CHECK.check(root)


if __name__ == "__main__":
    unittest.main()
