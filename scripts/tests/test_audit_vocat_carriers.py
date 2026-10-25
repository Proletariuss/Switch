"""Reference audit regressions; no network, SIM, daemon or real identifiers."""
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "audit_vocat", Path(__file__).resolve().parents[1] / "audit-vocat-carriers.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


class CarrierReferenceAuditTests(unittest.TestCase):
    def test_two_and_three_digit_mnc_stay_distinct(self):
        rows = AUDIT.synthetic_probes([{"id": "width", "match": {"home_plmns": ["51566", "515066"]}}])
        self.assertEqual([r["sim"]["mnc"] for r in rows], ["66", "066"])

    def test_each_match_is_conjunctive_and_values_expand(self):
        rows = AUDIT.synthetic_probes([{"id": "scoped", "match": {
            "home_plmns": ["23410"], "gid1_prefixes": ["01", "02"], "spns": ["A", "B"]},
            "match_any": [{"home_plmns": ["23415"], "spns": ["C"]}]}])
        self.assertEqual(len(rows), 5)
        self.assertEqual({(r["sim"].get("gid1"), r["sim"]["spn"]) for r in rows[:4]},
                         {("01", "A"), ("01", "B"), ("02", "A"), ("02", "B")})
        self.assertEqual(rows[-1]["match_index"], 1)

    def test_unscoped_spn_is_explicitly_synthetic(self):
        row = AUDIT.synthetic_probes([{"id": "unscoped", "match": {"spns": ["Brand"]}}])[0]
        self.assertTrue(row["unscoped"])
        self.assertEqual(row["sim"], {"mcc": "999", "mnc": "42", "spn": "Brand"})

    def test_unknown_fields_and_invalid_plmns_fail_closed(self):
        for match in [{"unknown": ["x"]}, {"home_plmns": ["２３４１０"]}, {"home_plmns": ["2341"]}]:
            with self.subTest(match=match), self.assertRaises(ValueError):
                AUDIT.synthetic_probes([{"id": "bad", "match": match}])

    def test_probe_budget_is_bounded(self):
        with self.assertRaisesRegex(ValueError, "budget"):
            AUDIT.synthetic_probes([{"id": "large", "match": {"spns": [str(n) for n in range(10001)]}}])

    def test_effective_endpoint_obeys_override_order(self):
        cfg = {"ims": {"epdg": {"candidates": [{"address": "base.invalid"}]}}}
        AUDIT.merge(cfg, {"ims": {"epdg": {"candidates": [{"address": "plmn.invalid"}]}}})
        AUDIT.merge(cfg, {"ims": {"epdg": {"address": "selector.invalid"}}})
        self.assertEqual(AUDIT.epdg({"mcc": "234", "mnc": "10"}, cfg), "selector.invalid")

    def test_fallback_is_accepted_not_rejected(self):
        counts = AUDIT.replay_counts([
            {"matched": True, "rejected": False, "runtime_profile": {"id": "known"}},
            {"matched": False, "rejected": False, "runtime_profile": {"id": "auto"}},
            {"rejected": True}])
        self.assertEqual(counts, {"total": 3, "matched": 1, "fallback": 1, "rejected": 1,
                                  "runtime_profile_present": 2, "runtime_profile_absent": 1})


if __name__ == "__main__":
    unittest.main()
