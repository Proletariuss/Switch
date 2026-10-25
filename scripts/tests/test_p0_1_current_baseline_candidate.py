"""Negative checks for the inactive, versioned P0-1 replacement candidate."""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "p0_1_current_baseline_candidate",
    ROOT / "scripts/check-p0-1-current-baseline-candidate.py")
assert SPEC and SPEC.loader
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def read(path: Path) -> dict:
    return json.loads((ROOT / path).read_text())


class CandidateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.original = [read(path) for path in (
            CHECK.OLD, CHECK.NEW, CHECK.CHANGE, CHECK.DECISION, CHECK.SUCCESSOR)]

    def documents(self) -> list[dict]:
        return copy.deepcopy(self.original)

    def reject(self, documents: list[dict], fragment: str) -> None:
        errors = CHECK.validate(*documents)
        self.assertTrue(any(fragment in error for error in errors), errors)

    def test_candidate_is_valid_but_inactive(self) -> None:
        self.assertEqual(CHECK.validate(*self.original), [])
        result = CHECK.check(ROOT)
        self.assertTrue(result["valid"], result["errors"])
        self.assertFalse(result["active"])
        self.assertEqual(result["catalog_leaves"], 115)
        self.assertFalse(result["overall_denominator_frozen"])

    def test_predecessor_pin_resolves_to_immutable_archive(self) -> None:
        predecessor = self.original[2]["predecessor_ledger"]
        self.assertEqual(predecessor["path"], str(CHECK.OLD))
        self.assertEqual(predecessor["path_at_cut"], str(CHECK.OLD_AT_CUT))
        self.assertEqual(hashlib.sha256((ROOT / predecessor["path"]).read_bytes()).hexdigest(),
                         predecessor["sha256"])
        self.assertNotEqual(hashlib.sha256((ROOT / predecessor["path_at_cut"]).read_bytes()).hexdigest(),
                            predecessor["sha256"])
        docs = self.documents()
        docs[2]["predecessor_ledger"]["path"] = str(CHECK.OLD_AT_CUT)
        self.reject(docs, "historical ledger pin drift")
        docs = self.documents()
        docs[2]["predecessor_ledger"]["path_at_cut"] = str(CHECK.OLD)
        self.reject(docs, "historical ledger pin drift")

    def test_old_historical_leaf_cannot_be_promoted(self) -> None:
        docs = self.documents()
        docs[0]["leaves"][0]["acceptance_status"] = "PASS"
        self.reject(docs, "historical leaf status or scope drift")

    def test_candidate_leaf_cannot_be_promoted(self) -> None:
        docs = self.documents()
        docs[1]["leaves"][0]["acceptance_status"] = "PASS"
        self.reject(docs, "candidate ledger differs")

    def test_other_113_leaves_cannot_change(self) -> None:
        docs = self.documents()
        docs[1]["leaves"][2]["title"] += " changed"
        self.reject(docs, "candidate ledger differs")

    def test_other_113_raw_json_objects_are_byte_identical(self) -> None:
        old = CHECK.raw_leaf_objects(ROOT / CHECK.OLD, 115)
        candidate = CHECK.raw_leaf_objects(ROOT / CHECK.NEW, 115)
        self.assertEqual(old[2:], candidate[2:])
        self.assertEqual(len(candidate[2:]), 113)

    def test_byte_only_change_in_other_leaf_is_visible(self) -> None:
        original = (ROOT / CHECK.NEW).read_bytes()
        third = CHECK.raw_leaf_objects(ROOT / CHECK.NEW, 115)[2]
        changed = third.replace(b'"title":', b'"title" :', 1)
        self.assertNotEqual(third, changed)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "candidate.json"
            path.write_bytes(original.replace(third, changed, 1))
            self.assertEqual(json.loads(path.read_bytes()), self.original[1])
            self.assertNotEqual(CHECK.raw_leaf_objects(ROOT / CHECK.OLD, 115)[2:],
                                CHECK.raw_leaf_objects(path, 115)[2:])

    def test_build_non_normal_axes_have_specific_reasons(self) -> None:
        leaf = self.original[1]["leaves"][1]
        row = self.original[2]["old_to_new_leaf_map"][1]
        for axis, reason in CHECK.BUILD_AXIS_REASONS.items():
            self.assertEqual(leaf["evidence"][axis]["not_applicable_reason"], reason)
            self.assertEqual(row["non_normal_not_applicable_reasons"][axis], reason)
            self.assertNotIn("既有基线/轨迹/快照", reason)
        docs = self.documents()
        docs[2]["old_to_new_leaf_map"][1]["non_normal_not_applicable_reasons"]["cancel"] = "旧快照理由"
        self.reject(docs, "current-build facet rationale drift")

    def test_mapping_cannot_expand_scenario_scope(self) -> None:
        docs = self.documents()
        docs[2]["old_to_new_leaf_map"][1]["scenario_class_ids_after"] = ["DELIVERY", "CLEAN"]
        self.reject(docs, "replacement identity, scope or status drift")

    def test_mapping_cannot_activate_or_award_pass(self) -> None:
        docs = self.documents()
        docs[2]["candidate_ledger"]["activation"] = "ACTIVE"
        self.reject(docs, "candidate target or activation drift")
        docs = self.documents()
        docs[2]["accounting_impact"]["functional_passes_awarded"] = 1
        self.reject(docs, "candidate accounting impact drift")

    def test_denominator_cannot_be_frozen(self) -> None:
        docs = self.documents()
        docs[1]["freeze"]["overall_denominator_frozen"] = True
        docs[1]["freeze"]["overall_denominator"] = 115
        self.reject(docs, "candidate denominator or PASS drift")

    def test_historical_upgrade_cannot_be_claimed(self) -> None:
        docs = self.documents()
        docs[2]["historical_limits"]["legacy_equivalence_or_upgrade_proven"] = True
        self.reject(docs, "historical or physical limits drift")

    def test_normal_rule_and_mapping_must_agree(self) -> None:
        docs = self.documents()
        docs[2]["proposed_normal_rules"]["P0-1.current-source-baseline"][
            "normal_required_condition"] = "Git HEAD is enough"
        self.reject(docs, "candidate normal rule drift")

    def test_old_to_new_identity_cannot_swap(self) -> None:
        docs = self.documents()
        docs[2]["old_to_new_leaf_map"][0]["old_id"] = "P0-1.baseline-280"
        self.reject(docs, "replacement identity, scope or status drift")

    def test_successor_remains_evidence_only(self) -> None:
        docs = self.documents()
        docs[4]["ledger_integration"]["mode"] = "FORMAL_PASS"
        self.reject(docs, "software successor scope drift")


if __name__ == "__main__":
    unittest.main()
