"""Negative fixtures for the versioned, non-scoring current baseline decision."""
import copy
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "current_baseline_decision", ROOT / "scripts/check-current-baseline-decision.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
HISTORICAL_LEDGER = Path("docs/optimization/all-rust/acceptance-ledger-2026-10-19-iteration-663.json")


class CurrentBaselineDecisionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.decision = json.loads((ROOT / checker.DECISION).read_text())
        cls.ledger = json.loads((ROOT / HISTORICAL_LEDGER).read_text())
        cls.active = json.loads((ROOT / checker.LEDGER).read_text())
        cls.baselines = json.loads((ROOT / checker.BASELINES).read_text())

    def setUp(self):
        self.data = copy.deepcopy(self.decision)
        self.old = copy.deepcopy(self.ledger)
        self.provenance = copy.deepcopy(self.baselines)

    def reject(self, marker):
        errors = checker.validate(self.data, self.old, self.provenance)
        self.assertTrue(any(marker in error for error in errors), errors)

    def copy_source_fixture(self, root):
        for relative in (checker.DECISION, checker.LEDGER, HISTORICAL_LEDGER,
                         checker.BASELINES, checker.SNAPSHOT):
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, target)

    def test_adopted_policy_keeps_all_acceptance_unfrozen(self):
        self.assertEqual(checker.validate(self.data, self.old, self.provenance), [])
        self.assertEqual(checker.validate_active_scope(self.active), [])
        self.assertFalse(self.data["accounting"]["overall_denominator_frozen"])
        self.assertIsNone(self.data["accounting"]["overall_percentage"])

    def test_missing_direct_user_decision_is_rejected(self):
        self.data["authority"]["decision"] = "inferred by agent"
        self.reject("user authority drift")

    def test_old_280_leaf_cannot_become_pass(self):
        row = next(row for row in self.old["leaves"] if row["id"] == "P0-1.baseline-280")
        row["acceptance_status"] = "PASS"
        self.reject("historical ledger P0 status changed")

    def test_old_leaf_cannot_become_na_in_sidecar(self):
        self.data["historical_facts"]["old_leaf_dispositions"][0]["current_scope_disposition"] = "NOT_APPLICABLE"
        self.reject("historical P0 leaves were hidden or promoted")

    def test_old_leaf_cannot_be_removed(self):
        self.data["historical_facts"]["old_leaf_dispositions"].pop()
        self.reject("historical P0 leaves were hidden or promoted")

    def test_old_provenance_cannot_be_relabeled_recovered(self):
        self.provenance["roles"]["final_280"]["recovery_state"] = "RESTORED"
        self.reject("historical baseline unavailability changed")

    def test_new_current_leaf_is_not_functional_pass(self):
        self.data["current_scope"]["old_to_new_leaf_map"][0]["functional_acceptance_status"] = "PASS"
        self.reject("current scope mapping or unreviewed instances drift")

    def test_current_scope_cannot_drop_untested_others(self):
        self.data["current_scope"]["all_other_requirement_leaves_retained_without_automatic_pass"] = False
        self.reject("current scope mapping or unreviewed instances drift")

    def test_current_release_candidate_cannot_claim_an_unbuilt_artifact(self):
        self.data["current_release_candidate"]["artifact_status"] = "PASS"
        self.reject("current release candidate identity or validation boundary drift")

    def test_baseline_recovery_cannot_be_claimed_from_current_cut(self):
        self.data["historical_facts"]["legacy_equivalence_proven"] = True
        self.reject("historical 2.80 equivalence falsely claimed")

    def test_forged_source_digest_is_rejected(self):
        self.data["verified_software_cut"]["build_source_identity_sha256"] = "0" * 64
        self.reject("verified source or cold-build identity drift")

    def test_forged_artifact_audit_is_rejected(self):
        self.data["verified_software_cut"]["artifact_audit"]["sha256"] = "0" * 64
        self.reject("verified source or cold-build identity drift")

    def test_forged_app_binary_is_rejected(self):
        self.data["verified_software_cut"]["app"]["executable_sha256"] = "0" * 64
        self.reject("verified app identity drift")

    def test_forged_gate_result_is_rejected(self):
        self.data["verified_software_cut"]["software_gate"]["status"] = "failed"
        self.reject("software gate evidence drift")

    def test_forged_denominator_is_rejected(self):
        self.data["accounting"]["overall_denominator_frozen"] = True
        self.data["accounting"]["overall_percentage"] = 100
        self.reject("current scope falsely claims product acceptance")

    def test_hidden_extra_pass_field_is_rejected(self):
        self.data["current_scope"]["product_pass"] = True
        self.reject("current scope mapping or unreviewed instances drift")

    def test_source_cut_cannot_claim_post_decision_source_identity(self):
        self.data["current_scope"]["source_cut_relation"] = "Current source is identical to final-b"
        self.reject("current scope mapping or unreviewed instances drift")

    def test_source_only_check_needs_no_sibling_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            self.assertFalse((root.parent / "build").exists())
            self.assertTrue(checker.check(root)["valid"])

    def test_source_only_check_detects_snapshot_drift(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            with (root / checker.SNAPSHOT).open("a") as stream:
                stream.write("\n")
            self.assertIn("selective source snapshot bytes changed", checker.check(root)["errors"])

    def test_source_only_check_rejects_escaping_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            decision = root / checker.DECISION
            decision.unlink()
            decision.symlink_to(ROOT / checker.DECISION)
            with self.assertRaisesRegex(ValueError, "source evidence path is unsafe"):
                checker.check(root)

    def test_current_ledger_provenance_refresh_does_not_change_historical_cut(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            current = json.loads((root / checker.LEDGER).read_text())
            current["review_update"]["iteration"] = 665
            (root / checker.LEDGER).write_text(json.dumps(current))
            self.assertTrue(checker.check(root)["valid"])
            self.assertEqual((root / HISTORICAL_LEDGER).read_bytes(),
                             (ROOT / HISTORICAL_LEDGER).read_bytes())
            self.assertEqual((root / checker.DECISION).read_bytes(),
                             (ROOT / checker.DECISION).read_bytes())

    def test_historical_ledger_tamper_is_rejected_after_current_metadata_refresh(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            with (root / HISTORICAL_LEDGER).open("a") as stream:
                stream.write("\n")
            self.assertIn("historical ledger bytes changed", checker.check(root)["errors"])

    def test_historical_ledger_is_required_without_current_file_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            (root / HISTORICAL_LEDGER).unlink()
            with self.assertRaises(FileNotFoundError):
                checker.check(root)

    def test_historical_ledger_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            historical = root / HISTORICAL_LEDGER
            historical.unlink()
            historical.symlink_to(ROOT / HISTORICAL_LEDGER)
            with self.assertRaisesRegex(ValueError, "source evidence path is unsafe"):
                checker.check(root)

    def test_current_ledger_p0_promotion_is_rejected_even_with_valid_historical_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "src"
            self.copy_source_fixture(root)
            current = json.loads((root / checker.LEDGER).read_text())
            row = next(row for row in current["leaves"] if row["id"] == "P0-1.current-build-baseline")
            row["acceptance_status"] = "PASS"
            (root / checker.LEDGER).write_text(json.dumps(current))
            self.assertIn("active current-baseline P0 scope or no-pass boundary changed", checker.check(root)["errors"])

    def test_current_ledger_cannot_restore_old_leaf_ids(self):
        current = copy.deepcopy(self.active)
        current["leaves"][0]["id"] = "P0-1.working-baseline"
        self.assertIn("active current-baseline P0 scope or no-pass boundary changed",
                      checker.validate_active_scope(current))


if __name__ == "__main__":
    unittest.main()
