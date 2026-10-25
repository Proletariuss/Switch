"""Mutate the real acceptance bundle; false completeness must fail closed."""
import copy
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("acceptance_ledger", ROOT / "scripts/check-acceptance-ledger.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class AcceptanceLedgerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = {k: json.loads((ROOT / checker.BASE / v).read_text())
                        for k, v in checker.FILES.items()}
        cls.rt = checker.runtime_rows((ROOT / checker.BASE / "runtime-capability-inventory.md").read_text())
        cls.qmi_review = json.loads((ROOT / checker.P0_QMI_REVIEW).read_text())

    def setUp(self):
        self.data = copy.deepcopy(self.original)

    def reject(self, text):
        result = checker.validate_bundle(self.data, self.rt)
        self.assertFalse(result["metadata_consistent"])
        self.assertTrue(any(text in error for error in result["errors"]), result["errors"])

    def test_current_bundle_is_pending_not_product_pass(self):
        result = checker.validate_bundle(self.data, self.rt)
        self.assertTrue(result["metadata_consistent"], result["errors"])
        self.assertFalse(result["overall_denominator_frozen"])
        self.assertIsNone(result["overall_percentage"])
        self.assertFalse(result["functional_acceptance_awarded"])

    def test_current_baseline_scope_is_adopted_without_functional_pass(self):
        rows = {row["id"]: row for row in self.data["ledger"]["leaves"]}
        self.assertEqual(set(rows) & checker.P0_CURRENT_BASELINE_LEAVES,
                         checker.P0_CURRENT_BASELINE_LEAVES)
        self.assertFalse({"P0-1.working-baseline", "P0-1.baseline-280"} & set(rows))
        self.assertTrue(all(rows[leaf]["acceptance_status"] == "NOT_REVIEWED"
                            for leaf in checker.P0_CURRENT_BASELINE_LEAVES))
        self.assertIsNone(self.data["ledger"]["freeze"]["overall_denominator"])

    def test_old_p0_leaf_cannot_reenter_active_ledger(self):
        self.data["ledger"]["leaves"][0]["id"] = "P0-1.working-baseline"
        self.reject("current-baseline P0-1 scope replacement differs")

    def test_p0_activation_authority_cannot_be_removed(self):
        self.data["ledger"]["scope_activation"]["decision"]["sha256"] = "0" * 64
        self.reject("P0-1 activation authority or no-pass boundary changed")

    def test_current_p0_normal_predicate_must_match_adopted_rule(self):
        self.data["protocol"]["P0_normal_rules"]["P0-1.current-build-baseline"][
            "normal_required_condition_unchanged"] += " forged"
        self.reject("P0 normal predicate differs from adopted rule")

    def test_frozen_denominator_does_not_unlock_product_final(self):
        result = {
            "metadata_consistent": True,
            "overall_denominator_frozen": True,
            "functional_acceptance_awarded": False,
            "overall_product_complete": False,
        }
        self.assertTrue(checker.acceptance_gate(result, require_frozen=True))
        self.assertFalse(checker.acceptance_gate(
            result, require_product_complete=True))

    def test_product_final_requires_all_explicit_acceptance_predicates(self):
        complete = {
            "metadata_consistent": True,
            "overall_denominator_frozen": True,
            "functional_acceptance_awarded": True,
            "overall_product_complete": True,
        }
        self.assertTrue(checker.acceptance_gate(
            complete, require_product_complete=True))
        for field in ("metadata_consistent", "overall_denominator_frozen",
                      "functional_acceptance_awarded", "overall_product_complete"):
            with self.subTest(field=field):
                incomplete = dict(complete)
                incomplete[field] = False
                self.assertFalse(checker.acceptance_gate(
                    incomplete, require_product_complete=True))

    def test_current_software_scope_cannot_claim_device_or_carrier_acceptance(self):
        for field, value in (("connect_dji_module", True), ("connect_reader", True),
                             ("physical_or_carrier_acceptance", "VERIFIED"),
                             ("physical_or_carrier_leaves_promoted", True),
                             ("new_functional_passes", 1), ("overall_percentage", 100)):
            with self.subTest(field=field):
                self.data = copy.deepcopy(self.original)
                self.data["ledger"]["review_update"]["software_acceptance_scope"][field] = value
                self.reject("current software-only scope changed")

    def test_adopted_environment_policy_cannot_revert_to_pending_decision(self):
        rows = {row["id"]: row for row in self.data["ledger"]["applicability_matrix"]}
        rows["MEASURE"]["required_resolution"] = "采样冷定义和预算待定；仅daemon/Release耗时不代整机。"
        self.reject("MEASURE: adopted protocol is described as undecided")

    def test_adopted_environment_policy_remains_execution_pending(self):
        self.data["protocol"]["policy_decisions"]["final_soak"]["status"] = "RULE_NOT_DECIDED"
        self.reject("LONGRUN: adopted protocol is described as undecided")

    def test_dropped_route(self):
        self.data["mapping"]["API_entries"].pop()
        self.reject("dropped or added catalog entry")

    def test_duplicate_swift_index(self):
        rows = self.data["mapping"]["Swift_call_entries"]
        rows[1]["original_array_index"] = rows[0]["original_array_index"]
        self.reject("duplicate or missing source index")

    def test_boolean_original_index(self):
        self.data["mapping"]["API_entries"][0]["original_array_index"] = False
        self.reject("invalid original index")

    def test_boolean_swift_identity(self):
        self.data["mapping"]["Swift_call_entries"][1]["catalog_key"]["array_index"] = True
        self.reject("non-integer Swift identity")

    def test_changed_head_parent(self):
        row = next(r for r in self.data["mapping"]["API_entries"] if r.get("derived_from"))
        row["derived_from"] = "GET /not-the-parent"
        self.reject("HEAD alias changed")

    def test_catalog_identity_drift(self):
        self.data["mapping"]["RT_entries"][0]["original_record_sha256"] = "0" * 64
        self.reject("original catalog record changed")

    def test_primary_scenario_cannot_expand_leaf(self):
        leaves = {l["id"]: l for l in self.data["ledger"]["leaves"]}
        row = next(r for r in self.data["mapping"]["API_entries"]
                   if r.get("primary_leaf_id") and not r.get("primary_by_behavior_branch"))
        allowed = leaves[row["primary_leaf_id"]]["applicability"]["scenario_class_ids"]
        other = next(c["id"] for c in self.data["ledger"]["applicability_matrix"] if c["id"] not in allowed)
        row["applicable_scenario_class_ids"].append(other)
        self.reject("entry expands primary/conditional scenario scope")

    def test_conditional_scenario_cannot_expand_leaf(self):
        row = next(r for r in self.data["mapping"]["API_entries"] if r.get("primary_by_behavior_branch"))
        row["primary_by_behavior_branch"][0]["scenario_class_ids"].append("NOT_A_CLASS")
        self.reject("branch expands leaf scenario scope")

    def test_nearby_candidate_is_not_typed_owner(self):
        self.data["mapping"]["API_entries"][0]["owner"] = "nearby handler candidate"
        self.reject("untyped owner")

    def test_exact_owner_requires_source_proof(self):
        self.data["mapping"]["API_entries"][0]["owner"] = {
            "resolution": "EXACT", "claim": "not actually verified",
            "certifies_complete_graph": False, "candidates": ["adjacent match arm"]}
        self.reject("EXACT needs pinned source facts")

    def test_empty_source_pins(self):
        self.data["mapping"]["current_source_file_pins"] = []
        self.reject("empty mapping source pins")

    def test_raw_source_pin_drift(self):
        self.data["mapping"]["current_source_file_pins"][0]["sha256"] = "0" * 64
        self.assertTrue(checker.validate_source_pins(self.data["mapping"], ROOT))

    def test_identical_pin_can_belong_to_both_review_groups(self):
        errors = checker.validate_source_pins(self.data["mapping"], ROOT)
        self.assertFalse(any("duplicate source pin" in error or "conflicting source pin" in error
                             for error in errors), errors)

    def test_conflicting_cross_group_pin_is_rejected(self):
        current_files = {
            pin["file"] for pin in self.data["mapping"]["current_source_file_pins"]
        }
        pin = next(
            pin for pin in self.data["mapping"]["supplementary_reviewed_source_file_pins"]
            if pin["file"] in current_files
        )
        pin["sha256"] = "0" * 64
        errors = checker.validate_source_pins(self.data["mapping"], ROOT)
        self.assertTrue(any("conflicting source pin across review groups" in error for error in errors), errors)

    def test_duplicate_pin_within_one_group_is_rejected(self):
        pins = self.data["mapping"]["supplementary_reviewed_source_file_pins"]
        pins.append(copy.deepcopy(pins[0]))
        errors = checker.validate_source_pins(self.data["mapping"], ROOT)
        self.assertTrue(any("duplicate source pin within supplementary_reviewed_source_file_pins" in error
                            for error in errors), errors)

    def test_frozen_cut_flag_must_match_hashes(self):
        pin = self.data["mapping"]["current_source_file_pins"][0]
        pin["matches_frozen_cut"] = not pin["matches_frozen_cut"]
        self.assertTrue(any("matches_frozen_cut disagrees" in error
                            for error in checker.validate_source_pins(self.data["mapping"], ROOT)))

    def test_source_hash_change_requires_audited_provenance(self):
        pin = self.data["mapping"]["current_source_file_pins"][0]
        pin["frozen_cut_sha256"] = "0" * 64
        pin["matches_frozen_cut"] = False
        pin["sha256"] = checker.file_digest(ROOT / pin["file"])
        pin.pop("source_update", None)
        self.assertTrue(any("lacks update provenance" in error
                            for error in checker.validate_source_pins(self.data["mapping"], ROOT)))

    def test_reviewed_source_fragment_must_match_declared_line(self):
        source_ref = self.data["mapping"]["reviewed_resolution_entries"][0]["source_refs"][0]
        source_ref["line"] += 1
        errors = checker.source_ref_errors(self.data["mapping"], ROOT)
        self.assertTrue(any("source fragment differs at its declared line" in error for error in errors), errors)

    def test_current_source_reference_requires_current_pin(self):
        source_ref = self.data["mapping"]["reviewed_resolution_entries"][0]["source_refs"][0]
        source_ref["sha256"] = "0" * 64
        errors = checker.source_ref_errors(self.data["mapping"], ROOT)
        self.assertTrue(any("does not use the current pin" in error for error in errors), errors)

    def test_historical_original_source_ref_is_not_rebound(self):
        row = next(row for row in self.data["mapping"]["API_entries"]
                   if row["entry_id"] == "POST /api/sms/forwarding/test")
        exit_ref = row["exit"]["source_refs"][0]
        exit_ref["original_source_ref"]["line"] = -7
        errors = checker.source_ref_errors(self.data["mapping"], ROOT)
        self.assertFalse(any("original_source_ref" in error for error in errors), errors)

    def test_runtime_map_current_anchor_is_verified(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        node = next(node for node in runtime_map["historical_source_nodes"]
                    if node["current_binding"].get("current_anchor"))
        node["current_binding"]["current_anchor"]["line"] += 1
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("current anchor fragment/range is stale" in error for error in errors), errors)

    def test_runtime_map_accepts_verified_historical_fragment_relations(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertFalse(errors, errors)

    def test_runtime_current_binding_tally_matches_historical_rows(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        runtime_map["accounting_not_acceptance"]["current_binding_status_counts"]["EXACT_WHOLE_FILE_RAW_MATCH"] += 1
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("current binding status counts differ" in error for error in errors), errors)

    def test_runtime_adoption_tally_split_matches_sha_pinned_candidate(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        counts = runtime_map["accounting_not_acceptance"]["current_node_binding_status_counts"]
        counts["UNIQUE_EXACT_FRAGMENT_REBOUND"] += 1
        counts["EXACT_WHOLE_FILE_RAW_MATCH"] -= 1
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("root-adoption binding tally disagrees with reviewed candidate" in error
                            for error in errors), errors)

    def test_runtime_adoption_candidate_hash_is_verified(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        runtime_map["root_adoption"]["candidate_sha256"] = "0" * 64
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("root-adoption candidate identity or SHA pin differs" in error
                            for error in errors), errors)

    def test_runtime_map_source_copy_needs_no_sibling_build_reports(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        baseline_errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        source_paths = set()
        for node in runtime_map["historical_source_nodes"]:
            anchor = node.get("current_binding", {}).get("current_anchor")
            if anchor:
                source_paths.add(checker.resolve_map_source(ROOT, anchor["file"]))
                historical = node["historical"]
                source_paths.add(checker.resolve_map_source(ROOT, historical["file"]))
                source_paths.add(checker.resolve_map_source(
                    ROOT, historical.get("source_record", {}).get("file", historical["file"])))
        for node in runtime_map["new_current_replacement_nodes"]:
            source_paths.add(checker.resolve_map_source(ROOT, node["file"]))
        snapshot = runtime_map["current_source_refresh"]["source_snapshot"]["path"]
        source_paths.add(ROOT / snapshot)
        for entry in json.loads((ROOT / snapshot).read_text())["files"]:
            source_paths.add(ROOT / entry["path"])
        with tempfile.TemporaryDirectory() as capsule:
            cold_root = Path(capsule) / "src"
            for source in source_paths:
                target = cold_root / source.relative_to(ROOT)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
            self.assertFalse((Path(capsule) / "build/reports").exists())
            self.assertEqual(checker.validate_runtime_source_map(runtime_map, cold_root), baseline_errors)

    def test_runtime_accounting_cannot_claim_scoring_or_denominator(self):
        original = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        for key, forged in (("new_requirement_ids", 1), ("new_scored_instances", True),
                            ("new_functional_PASS_instances", 1), ("overall_denominator", 115),
                            ("overall_percentage", 0)):
            with self.subTest(key=key):
                runtime_map = copy.deepcopy(original)
                runtime_map["accounting_not_acceptance"][key] = forged
                errors = checker.validate_runtime_source_map(runtime_map, ROOT)
                self.assertTrue(any("runtime accounting claims scored units" in error
                                    for error in errors), errors)

    def test_runtime_whole_file_relation_cannot_be_forged(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        node = next(node for node in runtime_map["historical_source_nodes"]
                    if node["current_binding"].get("status") == "EXACT_WHOLE_FILE_RAW_MATCH"
                    and node["current_binding"]["current_anchor"].get("historical_fragment_relation")
                    == "SYMBOL_SUBSTRING_WITHIN_EXACT_SOURCE_RANGE")
        node["current_binding"]["current_anchor"]["historical_fragment_relation"] = "EXACT_RANGE_EQUAL"
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("exact-range relation contradicts historical fragment" in error
                            for error in errors), errors)

    def test_runtime_whole_file_relation_must_be_known(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        node = next(node for node in runtime_map["historical_source_nodes"]
                    if node["current_binding"].get("status") == "EXACT_WHOLE_FILE_RAW_MATCH")
        node["current_binding"]["current_anchor"]["historical_fragment_relation"] = "UNVERIFIED"
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("unknown historical fragment relation" in error for error in errors), errors)

    def test_runtime_historical_symbol_must_be_on_its_declared_line(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        node = next(node for node in runtime_map["historical_source_nodes"]
                    if node.get("current_binding", {}).get("status") == "EXACT_WHOLE_FILE_RAW_MATCH"
                    and node["current_binding"]["current_anchor"].get("historical_fragment_relation")
                    == "SYMBOL_SUBSTRING_WITHIN_EXACT_SOURCE_RANGE")
        anchor = node["current_binding"]["current_anchor"]
        path = checker.resolve_map_source(ROOT, anchor["file"])
        lines = path.read_text().splitlines()
        start = anchor["line"]
        node["historical"]["end_line"] = start + 1
        node["historical"]["line"] = start + 1
        node["historical"]["source_record"]["end_line"] = start + 1
        node["historical"]["source_record"]["line"] = start + 1
        anchor["end_line"] = start + 1
        anchor["exactfragment"] = "\n".join(lines[start - 1:start + 1]) + "\n"
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("historical symbol is absent from its declared source line range" in error
                            for error in errors), errors)

    def test_runtime_whole_file_relation_paths_must_match(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        node = next(node for node in runtime_map["historical_source_nodes"]
                    if node.get("current_binding", {}).get("status") == "EXACT_WHOLE_FILE_RAW_MATCH")
        node["historical"]["source_record"]["file"] = "rust/crates/proletariusd/src/main.rs"
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("whole-file relation paths do not identify the same source" in error
                            for error in errors), errors)

    def test_runtime_map_whole_file_claim_cannot_hide_drift(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        node = next(node for node in runtime_map["historical_source_nodes"]
                    if node["current_binding"].get("status") == "EXACT_WHOLE_FILE_RAW_MATCH")
        node["current_binding"]["current_anchor"]["raw_sha256"] = "0" * 64
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("current source hash is stale" in error for error in errors), errors)

    def _observed_catalog_pins(self):
        pins = {}
        for key, name in checker.FILES.items():
            if key in {"protocol", "baselines", "mapping", "inventory"}:
                pins[str(checker.BASE / name)] = checker.file_digest(ROOT / checker.BASE / name)
        runtime_path = ROOT / checker.RUNTIME_MAP
        pins[checker.RUNTIME_MAP] = checker.file_digest(runtime_path)
        runtime_map = json.loads(runtime_path.read_text())
        snapshot_ref = runtime_map["current_source_refresh"]["source_snapshot"]
        pins[snapshot_ref["path"]] = checker.file_digest(ROOT / snapshot_ref["path"])
        return pins, snapshot_ref

    def test_adopted_entry_mapping_pin_drift_is_detected(self):
        pins, snapshot_ref = self._observed_catalog_pins()
        catalog = copy.deepcopy(self.data["ledger"]["source_catalog"])
        catalog["entry_mapping"]["sha256"] = "0" * 64
        errors = checker.validate_adopted_document_pins(catalog, pins, snapshot_ref)
        self.assertIn("adopted document pin drift: entry_mapping", errors)

    def test_adopted_api_inventory_pin_drift_is_detected(self):
        pins, snapshot_ref = self._observed_catalog_pins()
        catalog = copy.deepcopy(self.data["ledger"]["source_catalog"])
        catalog["api-contract_inventory"]["sha256"] = "0" * 64
        errors = checker.validate_adopted_document_pins(catalog, pins, snapshot_ref)
        self.assertIn("adopted document pin drift: api-contract_inventory", errors)

    def test_adopted_runtime_mapping_pin_drift_is_detected(self):
        pins, snapshot_ref = self._observed_catalog_pins()
        catalog = copy.deepcopy(self.data["ledger"]["source_catalog"])
        catalog["runtime_owner_format_exit_mapping"]["sha256"] = "0" * 64
        errors = checker.validate_adopted_document_pins(catalog, pins, snapshot_ref)
        self.assertIn("adopted document pin drift: runtime_owner_format_exit_mapping", errors)

    def test_adopted_snapshot_pin_drift_is_detected(self):
        pins, snapshot_ref = self._observed_catalog_pins()
        catalog = copy.deepcopy(self.data["ledger"]["source_catalog"])
        catalog["current_source_snapshot"]["sha256"] = "0" * 64
        errors = checker.validate_adopted_document_pins(catalog, pins, snapshot_ref)
        self.assertIn("adopted document pin drift: current_source_snapshot", errors)

    def test_successor_snapshot_keeps_exact_immutable_i691_predecessor(self):
        expected = checker.CURRENT_SOURCE_SNAPSHOT
        predecessor = ROOT / expected["previous_path"]
        self.assertEqual(expected["iteration"], 764)
        self.assertEqual(json.loads(predecessor.read_text())["iteration"], 691)
        self.assertEqual(checker.file_digest(predecessor), expected["previous_sha256"])
        current = json.loads((ROOT / expected["path"]).read_text())
        previous = json.loads(predecessor.read_text())
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        current["previous_snapshot"]["sha256"] = "0" * 64
        self.assertIn("current source refresh: snapshot identity or predecessor drift",
                      checker.validate_current_source_refresh(current, previous, self.data["mapping"], runtime))

    def test_qmi_successor_cannot_restore_obsolete_default_qmi_claim(self):
        review = copy.deepcopy(self.qmi_review)
        self.assertEqual(review["swift_boundary"]["launch_anchor_id"], "swift_default_at_launch")
        anchor = next(a for a in review["anchors"] if a["id"] == "swift_default_at_launch")
        anchor["exact_fragment"] = anchor["exact_fragment"].replace('"-backend=at"', '"-backend=qmi"')
        self.assertIn("P0 QMI static review: typed anchor semantic drift: swift_default_at_launch",
                      checker.validate_p0_qmi_review(review, ROOT))

    def test_active_ancestry_keeps_declared_predecessor_raw_identities(self):
        current = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        self.assertEqual(checker.validate_immutable_source_ancestry(current, ROOT), [])
        self.assertEqual([r["iteration"] for r in current["immutable_ancestry"]], [691, 690, 689, 688, 687, 686, 685, 684, 683, 682, 681, 680, 677, 676, 675, 672, 671, 670, 669, 668, 666])
        current["immutable_ancestry"][-1]["sha256"] = "0" * 64
        self.assertIn("current source refresh: immutable ancestry declaration drift",
                      checker.validate_immutable_source_ancestry(current, ROOT))

    def test_active_ancestry_detects_modified_raw_bytes_in_all_predecessors(self):
        current = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        for item in checker.CURRENT_SOURCE_SNAPSHOT["immutable_ancestry"]:
            with self.subTest(iteration=item["iteration"]), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for relative in checker.CURRENT_SOURCE_SNAPSHOT["immutable_documents"]:
                    path = root / relative
                    path.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(ROOT / relative, path)
                path = root / item["path"]
                path.write_bytes(path.read_bytes() + b"\n")
                self.assertIn("current source refresh: immutable ancestry pin drift: " + item["path"],
                              checker.validate_immutable_source_ancestry(current, root))

    def test_active_immutable_document_declaration_cannot_be_changed(self):
        current = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        current["immutable_documents"].pop(next(iter(current["immutable_documents"])))
        self.assertIn("current source refresh: immutable document declaration drift",
                      checker.validate_immutable_source_ancestry(current, ROOT))

    def test_active_immutable_document_raw_bytes_and_presence_are_guarded(self):
        current = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        for relative in checker.CURRENT_SOURCE_SNAPSHOT["immutable_documents"]:
            for missing in (False, True):
                with self.subTest(file=relative, missing=missing), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    for source in checker.CURRENT_SOURCE_SNAPSHOT["immutable_documents"]:
                        path = root / source
                        path.parent.mkdir(parents=True, exist_ok=True)
                        shutil.copy2(ROOT / source, path)
                    path = root / relative
                    if missing:
                        path.unlink()
                        message = "current source refresh: immutable document invalid: "
                    else:
                        path.write_bytes(path.read_bytes() + b"\n")
                        message = "current source refresh: immutable document pin drift: "
                    self.assertIn(message + relative,
                                  checker.validate_immutable_source_ancestry(current, root))

    def test_p0_qmi_static_review_matches_current_source(self):
        self.assertEqual(checker.validate_p0_qmi_review(self.qmi_review, ROOT), [])

    def test_p0_qmi_current_trace_preserves_versioned_historical_evidence(self):
        self.assertEqual(self.qmi_review["iteration"], 764)
        previous = copy.deepcopy(self.qmi_review)
        previous["historical_review"]["sha256"] = "0" * 64
        self.assertIn("P0 QMI static review: historical review identity or pin drift",
                      checker.validate_p0_qmi_review(previous, ROOT))
        previous = copy.deepcopy(self.qmi_review)
        previous["current_source_snapshot"]["path"] = (
            "docs/optimization/all-rust/iteration-646-source-snapshot.json")
        self.assertIn("P0 QMI static review: current source snapshot identity drift",
                      checker.validate_p0_qmi_review(previous, ROOT))

    def test_p0_qmi_static_review_detects_missing_exit_anchor(self):
        review = copy.deepcopy(self.qmi_review)
        review["anchors"] = [anchor for anchor in review["anchors"]
                             if anchor["id"] != "core_close_join"]
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: trace anchor set drift", errors)

    def test_p0_qmi_static_review_detects_source_or_line_drift(self):
        review = copy.deepcopy(self.qmi_review)
        file = "rust/crates/proletarius-qmi-libusb/src/macos.rs"
        review["source_files"][file] = "0" * 64
        next(anchor for anchor in review["anchors"] if anchor["id"] == "usb_ctl_sync")["line"] += 1
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: source file drift: " + file, errors)
        self.assertIn("P0 QMI static review: trace anchor drift: usb_ctl_sync", errors)

    def test_p0_qmi_static_review_rejects_functional_promotion(self):
        review = copy.deepcopy(self.qmi_review)
        review["candidate_semantic_instances"][0]["acceptance_pass_awarded"] = True
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertTrue(any("candidate instance scope or acceptance drift" in error
                            for error in errors), errors)

    def test_p0_qmi_branch_rejects_unrelated_valid_anchor(self):
        review = copy.deepcopy(self.qmi_review)
        branch = next(item for item in review["candidate_semantic_instances"]
                      if item["id"] == "detach_cleanup_unknown")
        branch["anchor_ids"][0] = "main_qmi_shutdown"
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: candidate instance scope or acceptance drift: "
                      "detach_cleanup_unknown", errors)

    def test_p0_qmi_branch_rejects_placeholder_semantics(self):
        review = copy.deepcopy(self.qmi_review)
        branch = review["candidate_semantic_instances"][0]
        branch["trigger"] = "x"
        branch["static_expected_path"] = "x"
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertTrue(any("candidate instance scope or acceptance drift" in error
                            for error in errors), errors)

    def test_p0_qmi_roles_reject_placeholder_semantics(self):
        review = copy.deepcopy(self.qmi_review)
        review["producer_writer_exit"] = {role: "x" for role in
                                          ("producer", "format", "writer", "exit")}
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: producer/format/writer/exit semantic drift", errors)

    def test_p0_qmi_rejects_unpaired_public_denial(self):
        review = copy.deepcopy(self.qmi_review)
        review["route_auth_pairs"]["public_and_rs_denied"] = ["main_socket_auth"]
        anchor = next(anchor for anchor in review["anchors"]
                      if anchor["id"] == "main_public_private_reject")
        anchor["line"] += 1
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: route authentication pairing drift", errors)
        self.assertIn("P0 QMI static review: route anchor semantic drift: main_public_private_reject", errors)

    def test_p0_qmi_typed_routes_must_remain_complete(self):
        review = copy.deepcopy(self.qmi_review)
        review["typed_operations"] = [operation for operation in review["typed_operations"]
                                      if operation["id"] != "operator"]
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: typed operation set drift", errors)

    def test_p0_qmi_set_ims_unknown_result_cannot_be_promoted(self):
        review = copy.deepcopy(self.qmi_review)
        update = next(operation for operation in review["typed_operations"]
                      if operation["id"] == "set-ims")
        update["typed_unknown"] = "Modem mutation confirmed"
        update["acceptance_pass_awarded"] = True
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: typed operation scope or acceptance drift: set-ims",
                      errors)

    def test_p0_qmi_operator_cannot_hide_nas_name_fallback(self):
        review = copy.deepcopy(self.qmi_review)
        operator = next(operation for operation in review["typed_operations"]
                        if operation["id"] == "operator")
        operator["response"] = "200 proves UIM and NAS both succeeded"
        operator["anchor_ids"].remove("nas_name_writer")
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: typed operation scope or acceptance drift: operator",
                      errors)

    def test_p0_qmi_unresolved_cannot_be_replaced_by_false_acceptance(self):
        review = copy.deepcopy(self.qmi_review)
        review["unresolved"] = ["All resolved; physical device accepted."]
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: unresolved boundary drift", errors)

    def test_p0_qmi_source_refresh_cannot_award_acceptance_or_invent_new_files(self):
        for mutation in (lambda r: r["source_refresh"].update(functional_acceptance_awarded=True),
                         lambda r: r["source_refresh"].update(newly_reviewed_source_files=[
                             "rust/crates/proletarius-qmi-services/src/uim.rs"]),
                         lambda r: r["source_refresh"].update(review="All nine files are snapshot pinned and physically accepted.")):
            review = copy.deepcopy(self.qmi_review)
            mutation(review)
            errors = checker.validate_p0_qmi_review(review, ROOT)
            self.assertIn("P0 QMI static review: source refresh scope or acceptance drift", errors)

    def test_current_snapshot_cannot_drop_reviewed_sources_or_invent_pass(self):
        current = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        previous = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["previous_path"]).read_text())
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        self.assertEqual(checker.validate_current_source_refresh(
            current, previous, self.data["mapping"], runtime), [])
        mutated = copy.deepcopy(current)
        removed = min(checker.CURRENT_SOURCE_SNAPSHOT["added_files"] or
                      checker.CURRENT_SOURCE_SNAPSHOT["changed_files"])
        mutated["files"] = [row for row in mutated["files"] if row["path"] != removed]
        errors = checker.validate_current_source_refresh(
            mutated, previous, self.data["mapping"], runtime)
        self.assertTrue(any("selective source inventory" in error or
                            "reviewed changed/added file set" in error for error in errors), errors)
        mutated = copy.deepcopy(current)
        mutated["scope_metadata_binding"]["new_functional_passes"] = 1
        errors = checker.validate_current_source_refresh(
            mutated, previous, self.data["mapping"], runtime)
        self.assertIn("current source refresh: snapshot claims unsupported acceptance", errors)

    def test_current_map_refresh_cannot_hide_changed_source_or_award_pass(self):
        current = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        previous = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["previous_path"]).read_text())
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        runtime["current_source_refresh"]["changed_source_files"].pop()
        runtime["current_source_refresh"]["functional_acceptance_awarded"] = True
        errors = checker.validate_current_source_refresh(
            current, previous, self.data["mapping"], runtime)
        self.assertIn("current source refresh: runtime scope or acceptance drift", errors)

    def test_current_snapshot_and_maps_keep_the_user_software_only_boundary(self):
        current = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        previous = json.loads((ROOT / checker.CURRENT_SOURCE_SNAPSHOT["previous_path"]).read_text())
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        changed = copy.deepcopy(current)
        changed["software_acceptance_scope"]["connect_reader"] = True
        self.assertIn("current source refresh: software-only scope drift",
                      checker.validate_current_source_refresh(changed, previous, self.data["mapping"], runtime))
        runtime["current_source_refresh"]["software_acceptance_scope"]["new_functional_passes"] = 1
        self.assertIn("current source refresh: runtime scope or acceptance drift",
                      checker.validate_current_source_refresh(current, previous, self.data["mapping"], runtime))

    def test_p0_qmi_snapshot_cannot_claim_independent_uim_service_files(self):
        review = copy.deepcopy(self.qmi_review)
        review["source_pin_coverage"]["snapshot_source_files"].append(
            "rust/crates/proletarius-qmi-services/src/uim.rs")
        review["source_pin_coverage"]["independent_sha256_source_files"] = []
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: dual source pin coverage drift", errors)

    def test_p0_qmi_operator_uim_request_and_fallback_anchors_fail_closed(self):
        for anchor_id in ("uim_file_attributes", "uim_read_record", "uim_read_transparent",
                          "uim_ad_read_fallback", "uim_mnc_length_default"):
            review = copy.deepcopy(self.qmi_review)
            operator = next(op for op in review["typed_operations"] if op["id"] == "operator")
            operator["anchor_ids"].remove(anchor_id)
            errors = checker.validate_p0_qmi_review(review, ROOT)
            self.assertIn("P0 QMI static review: typed operation scope or acceptance drift: operator",
                          errors)
            review = copy.deepcopy(self.qmi_review)
            next(a for a in review["anchors"] if a["id"] == anchor_id)["line"] += 1
            errors = checker.validate_p0_qmi_review(review, ROOT)
            self.assertIn(f"P0 QMI static review: typed anchor semantic drift: {anchor_id}", errors)

    def test_p0_qmi_operator_ef_ad_unknown_cannot_be_promoted(self):
        review = copy.deepcopy(self.qmi_review)
        operator = next(op for op in review["typed_operations"] if op["id"] == "operator")
        operator["typed_unknown"] = "200 proves carrier identity"
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: typed operation scope or acceptance drift: operator",
                      errors)

    def test_p0_qmi_typed_source_anchor_and_swift_boundary_fail_closed(self):
        review = copy.deepcopy(self.qmi_review)
        next(anchor for anchor in review["anchors"]
             if anchor["id"] == "ims_set_writer")["line"] += 1
        review["swift_boundary"]["direct_qmi_rpc_calls"] = ["unreviewed caller"]
        errors = checker.validate_p0_qmi_review(review, ROOT)
        self.assertIn("P0 QMI static review: typed anchor semantic drift: ims_set_writer", errors)
        self.assertIn("P0 QMI static review: trace anchor drift: ims_set_writer", errors)
        self.assertIn("P0 QMI static review: Swift producer boundary drift", errors)

    def test_runtime_replacement_fragment_is_verified(self):
        runtime_map = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        runtime_map["new_current_replacement_nodes"][0]["line"] += 1
        errors = checker.validate_runtime_source_map(runtime_map, ROOT)
        self.assertTrue(any("replacement fragment/range is stale" in error for error in errors), errors)

    def test_source_path_escape(self):
        with self.assertRaises(ValueError):
            checker.safe_source(ROOT, "../outside-workspace")

    def test_facet_pass_cannot_hide_in_partial_leaf(self):
        leaf = self.data["ledger"]["leaves"][0]
        leaf["acceptance_status"] = "PARTIAL"
        leaf["evidence"]["normal"].update(status="PASS", evidence_refs=["invented"])
        self.reject("facet PASS requires")

    def test_software_subset_needs_partial_boundary(self):
        leaf = next(l for l in self.data["ledger"]["leaves"]
                    if l["evidence"]["normal"]["status"] == "CANDIDATE_SOFTWARE_SUBSET")
        leaf["acceptance_status"] = "NOT_REVIEWED"
        self.reject("software subset needs evidence and a PARTIAL leaf")

    def test_unfrozen_n_cannot_be_leaf_count(self):
        self.data["ledger"]["freeze"]["overall_denominator"] = 115
        self.reject("unfrozen denominator/percentage published")

    def test_requirements_cannot_be_dropped_from_both_files(self):
        leaf = self.data["ledger"]["leaves"].pop()["id"]
        self.data["mapping"]["required_leaf_ids"].remove(leaf)
        self.reject("retained predicates/classes/policies changed")

    def test_unknown_policy_fact_must_still_block_scope(self):
        policy = next(p for p in self.data["ledger"]["unresolved_policies"]
                      if self.data["protocol"]["policy_decisions"][p["id"]]["remaining_facts"])
        policy["blocks_overall_denominator"] = False
        self.reject("missing facts hidden from denominator")

    def test_empty_protocol_is_not_defined_scope(self):
        self.data["protocol"]["scenario_protocols"]["SW"]["finite_protocol_definition"] = ""
        self.reject("empty finite protocol")

    def test_nine_esim_later_auth_request_reopens_6982_without_claiming_success(self):
        protocol = self.data["protocol"]["scenario_protocols"]["NINE_ESIM"]
        scope = self.data["ledger"]["applicability_matrix"]
        nine_esim = next(row for row in scope if row["id"] == "NINE_ESIM")
        leaves = {row["id"] for row in self.data["ledger"]["leaves"]}
        history = self.data["protocol"].get("user_scope_update_history", [])

        self.assertIn("including 6982 diagnosis", protocol["finite_protocol_definition"])
        self.assertEqual(protocol["current_user_scope_decision"], checker.NINE_ESIM_CURRENT_SCOPE)
        self.assertEqual(nine_esim["current_user_scope_decision"], checker.NINE_ESIM_CURRENT_SCOPE)
        self.assertIn("6982", nine_esim["committed_requirement_scope"])
        self.assertIn("不绕过认证", nine_esim["required_resolution"])
        self.assertTrue({
            "P2B-2.ble-password",
            "P2B-2.bf71",
            "P2B-6.lpa-read",
            "P2B-6.lpa-manage",
            "P2B-7.lpa-auth-bpp",
            "P2B-7.lpa-install",
        }.issubset(leaves))
        self.assertTrue(any(
            row.get("scenario_class_id") == "NINE_ESIM"
            and "6982" in row.get("excluded_member", "")
            and row.get("status") == "HISTORICAL_SUPERSEDED"
            and row.get("superseded_by_iteration") == 652
            for row in history
        ))
        self.assertFalse(self.data["ledger"]["freeze"]["overall_denominator_frozen"])
        self.assertEqual(self.data["ledger"]["accounting"]["new_acceptance_passes_this_document"], 0)

    def test_nine_esim_superseded_exclusion_cannot_be_reactivated(self):
        for current in (self.data["protocol"]["scenario_protocols"]["NINE_ESIM"],
                        next(row for row in self.data["ledger"]["applicability_matrix"]
                             if row["id"] == "NINE_ESIM")):
            current["current_user_scope_decision"]["earlier_exclusion_current"] = True
        self.reject("NINE_ESIM: later user scope decision drift")

    def test_nine_esim_auth_tests_cannot_claim_bypass_or_6982_success(self):
        original = copy.deepcopy(self.data)
        for field in ("authentication_bypass_allowed", "6982_fixed_claimed"):
            with self.subTest(field=field):
                self.data = copy.deepcopy(original)
                self.data["protocol"]["scenario_protocols"]["NINE_ESIM"][
                    "current_user_scope_decision"][field] = True
                self.reject("NINE_ESIM: later user scope decision drift")

    def test_nine_esim_scope_history_cannot_hide_the_later_request(self):
        self.data["protocol"]["user_scope_update_history"] = [
            row for row in self.data["protocol"]["user_scope_update_history"]
            if row["iteration"] != 652]
        self.reject("NINE_ESIM: later direct user scope provenance is missing")

    def test_policy_copies_cannot_disagree(self):
        self.data["protocol"]["policy_decisions"]["evs_io"]["decision"] = "changed without ledger review"
        self.reject("ledger/protocol policy drift")

    def test_invalid_scope_pointer(self):
        self.data["ledger"]["applicability_matrix"][0]["scope_protocol_ref"] = "acceptance-scope-protocol.json#/not-present"
        self.reject("invalid scope protocol reference")

    def test_existing_wrong_class_pointer(self):
        self.data["ledger"]["applicability_matrix"][0]["scope_protocol_ref"] = "acceptance-scope-protocol.json#/scenario_protocols/USB"
        self.reject("scope reference targets a different identity")

    def test_existing_wrong_policy_pointer(self):
        self.data["ledger"]["unresolved_policies"][0]["scope_protocol_ref"] = "acceptance-scope-protocol.json#/policy_decisions/evs_io"
        self.reject("scope reference targets a different identity")

    def test_pass_predicate_is_pinned(self):
        self.data["ledger"]["leaves"][0]["pass_predicate"] = "weakened"
        self.reject("retained predicates/classes/policies changed")

    def test_seed_cannot_close_switch_time_baseline(self):
        self.data["baselines"]["roles"]["seed_263"]["may_close_leaf_ids"] = ["P0-1.working-baseline"]
        self.reject("seed must not close")

    def test_seed_cannot_substitute_for_280(self):
        self.data["baselines"]["roles"]["seed_263"]["cannot_substitute_for"] = []
        self.reject("seed substitution boundary")

    def test_incomplete_historical_baseline_is_not_complete(self):
        self.data["baselines"]["overall_historical_baseline_complete"] = True
        self.reject("historical baseline declared complete")

    def test_successor_retains_all_normative_and_historical_payloads(self):
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        self.assertEqual(checker.validate_source_successor_conservation(self.data["ledger"], runtime, ROOT), [])

    def test_successor_rejects_changed_historical_payload(self):
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        runtime["historical_source_nodes"][0]["historical"]["raw_sha256"] = "0" * 64
        self.assertIn("source successor: immutable payload drift: historical_node_identity_records",
                      checker.validate_source_successor_conservation(self.data["ledger"], runtime, ROOT))

    def test_successor_rejects_leaf_evidence_rewording_without_new_review(self):
        ledger = copy.deepcopy(self.data["ledger"])
        ledger["leaves"][0]["evidence"]["normal"]["status"] = "NOT_REVIEWED_SUCCESSOR_MUTATION"
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        self.assertIn("source successor: immutable payload drift: leaves",
                      checker.validate_source_successor_conservation(ledger, runtime, ROOT))

    def test_successor_rejects_lost_superseded_full_object(self):
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        row = next(n for n in runtime["superseded_current_replacement_nodes"] if n.get("superseded_at_iteration") == 764)
        row["superseded_prior_object"]["raw_sha256"] = "0" * 64
        self.assertTrue(any("replacement supersession drift" in e for e in
                            checker.validate_source_successor_conservation(self.data["ledger"], runtime, ROOT)))

    def test_successor_rejects_independent_node_acceptance(self):
        runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
        row = next(n for n in runtime["new_current_replacement_nodes"] if n["node_id"].startswith("current764::"))
        row["acceptance_awarded"] = True
        self.assertTrue(any("independent current node drift" in e for e in
                            checker.validate_source_successor_conservation(self.data["ledger"], runtime, ROOT)))

    def test_successor_rejects_changed_event_document_raw_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            relative = checker.SOURCE_SUCCESSOR_CONSERVATION["path"]
            destination = root / relative
            destination.parent.mkdir(parents=True)
            destination.write_bytes((ROOT / relative).read_bytes() + b"\n")
            runtime = json.loads((ROOT / checker.RUNTIME_MAP).read_text())
            self.assertEqual(checker.validate_source_successor_conservation(self.data["ledger"], runtime, root),
                             ["source successor: conservation document pin drift"])

    def test_qmi_successor_rejects_receive_dependency_drift(self):
        review = copy.deepcopy(self.qmi_review)
        review["receive_policy_dependency"]["sha256"] = "0" * 64
        self.assertIn("P0 QMI static review: receive policy dependency declaration drift",
                      checker.validate_p0_qmi_review(review, ROOT))



if __name__ == "__main__":
    unittest.main()
