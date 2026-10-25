#!/usr/bin/env python3
"""Read-only, fail-closed check for the inactive P0-1 v3 replacement candidate."""
from __future__ import annotations

import argparse
from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
BASE = Path("docs/optimization/all-rust")
# The v3 predecessor is the exact i677 ledger cut.  The active ledger is
# allowed to advance after this proposal was reviewed, so read the immutable
# archived bytes rather than silently repinning a later active file.
OLD = BASE / "acceptance-ledger-2026-10-19-iteration-677.json"
OLD_AT_CUT = BASE / "acceptance-ledger-2026-10-19.json"
NEW = BASE / "acceptance-ledger-candidate-v3-2026-10-23.json"
CHANGE = BASE / "p0-1-current-baseline-replacement-candidate-v3.json"
DECISION = BASE / "current-baseline-scope-decision.json"
SUCCESSOR = BASE / "current-baseline-scope-successor-2026-10-23.json"
PINS = {
    OLD: "e72a76fb9f8deca8916fccd25ed7d4aab4bcb9572175dd9be1bcbaf4115e673b",
    CHANGE: "e94602140a3bca1152ca047d1a10ad16e082847175ecc6aa9e9b05bad02ddfc7",
    DECISION: "b8b1446f1a46f1cdb63258e48a72fc5bc1037bdd816041b0ea546a3fb8a9b13e",
    SUCCESSOR: "33bccc9fbaed31e0f41d2dd0f56459100b2621d5314966ef49a97396041883e6",
}
EXPECTED_PAIRS = (
    ("P0-1.working-baseline", "P0-1.current-source-baseline", "SW"),
    ("P0-1.baseline-280", "P0-1.current-build-baseline", "DELIVERY"),
)
BUILD_AXIS_REASONS = {
    "cancel": "本叶核冷构建、软件门禁与交付产物的同源性；构建中断或失败由normal判据拒绝，不涉及用户业务取消、设备操作或租约取消。",
    "partial": "本叶要求完整构建及身份核对；不完整或损坏产物不能满足normal判据。本叶不处理业务数据的局部写入或rename提交，故不另设partial栏。",
    "durable": "本叶核发布清单、文件指纹及可复验构建收据，均归normal完整性判据；不承载密钥、journal或业务数据的持久提交协议，故不另设durable栏。",
    "release": "本叶核构建与交付产物身份；构建进程完成属于normal门禁。本叶不持有运行中设备、业务worker或lease，其释放由相应运行时需求叶验收。",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_path(root: Path, relative: Path) -> Path:
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("unsafe document path: " + str(relative))
    path = root / relative
    resolved = path.resolve(strict=True)
    if path.is_symlink() or not path.is_file() or root.resolve() not in resolved.parents:
        raise ValueError("absent or unsafe document path: " + str(relative))
    return path


def raw_leaf_objects(path: Path, count: int) -> list[bytes]:
    """Extract exact JSON object spans under the top-level leaves array."""
    raw = path.read_bytes()
    text = raw.decode("utf-8")
    marker = re.search(r'(?m)^  "leaves": \[$', text)
    if marker is None:
        raise ValueError("missing top-level leaves array: " + str(path))
    decoder = json.JSONDecoder()
    offset = marker.end()
    rows = []
    for _ in range(count):
        while offset < len(text) and text[offset] in " \t\r\n,":
            offset += 1
        if offset >= len(text) or text[offset] != "{":
            raise ValueError("missing leaf object: " + str(path))
        value, end = decoder.raw_decode(text, offset)
        if not isinstance(value, dict):
            raise ValueError("non-object leaf: " + str(path))
        rows.append(text[offset:end].encode("utf-8"))
        offset = end
    while offset < len(text) and text[offset] in " \t\r\n":
        offset += 1
    if offset >= len(text) or text[offset] != "]":
        raise ValueError("unexpected extra leaf object: " + str(path))
    return rows


def expected_leaf(old_leaf: dict, row: dict, change_path: Path, authority: dict) -> dict:
    leaf = copy.deepcopy(old_leaf)
    leaf["id"] = row["new_id"]
    leaf["title"] = row["new_title"]
    leaf["requirement_sources"] = [
        {"path": "src/" + authority[key]["path"],
         "sha256": authority[key]["sha256"], "clause": clause}
        for key, clause in (("decision_file", "current_scope.old_to_new_leaf_map"),
                            ("evidence_successor", "scope_mapping evidence-only; no acceptance"))
    ]
    leaf["evidence"]["normal"]["required_condition"] = row["required_condition"]
    for axis, facet in leaf["evidence"].items():
        if axis != "normal":
            if row["new_id"] == "P0-1.current-build-baseline":
                facet["not_applicable_reason"] = BUILD_AXIS_REASONS[axis]
            else:
                facet["not_applicable_reason"] = facet["not_applicable_reason"].replace(
                    "“实际工作区切换基线”", "“" + row["new_title"] + "”")
    leaf["status_reason"] = (
        "候选范围尚待正式审议；现有软件收据只作限定版本证据，不授功能PASS；旧历史事实另存且未通过。")
    leaf["normal_review_rule_ref"] = (
        str(change_path) + "#/proposed_normal_rules/" + row["new_id"])
    return leaf


def validate(old: dict, candidate: dict, change: dict,
             decision: dict, successor: dict) -> list[str]:
    errors: list[str] = []

    def need(ok: bool, reason: str) -> None:
        if not ok:
            errors.append(reason)

    need(set(change) == {"schema", "change_id", "status", "authority",
                         "predecessor_ledger", "candidate_ledger", "old_to_new_leaf_map",
                         "proposed_normal_rules", "accounting_impact", "historical_limits"}
         and change.get("schema") == "switch.p0-1-current-baseline-replacement-candidate.v3"
         and change.get("change_id") == "p0-1-current-baseline-2026-10-23"
         and change.get("status") == "CANDIDATE_NOT_ACTIVE_NO_ACCEPTANCE",
         "candidate mapping identity or activity drift")
    authority = change.get("authority", {})
    need(authority == {
        "kind": "direct_user_instruction", "decision": "改用当前可验证基线",
        "historical_archive_available": False,
        "decision_file": {"path": str(DECISION), "sha256": PINS[DECISION]},
        "evidence_successor": {"path": str(SUCCESSOR), "sha256": PINS[SUCCESSOR]},
    }, "user authority or predecessor pin drift")
    need(change.get("predecessor_ledger") == {
        "path": str(OLD), "sha256": PINS[OLD],
        "path_at_cut": str(OLD_AT_CUT)}, "historical ledger pin drift")
    target = change.get("candidate_ledger", {})
    need(set(target) == {"path", "sha256", "activation"}
         and target.get("path") == str(NEW)
         and target.get("activation") == "NOT_ACTIVE"
         and isinstance(target.get("sha256"), str)
         and len(target["sha256"]) == 64, "candidate target or activation drift")
    need(decision.get("status") == "ADOPTED_SCOPE_DECISION_NO_FUNCTIONAL_ACCEPTANCE"
         and decision.get("authority", {}).get("decision") == "改用当前可验证基线"
         and decision.get("historical_facts", {}).get("two_eighty_source_or_switch_time_restore_proven") is False,
         "adopted decision scope drift")
    need(successor.get("ledger_integration", {}).get("mode") == "EVIDENCE_ONLY_NO_FORMAL_LEAF_REWRITE"
         and successor.get("ledger_integration", {}).get("current_p0_functional_passes_awarded") == 0
         and successor.get("limitations", {}).get("legacy_equivalence_or_upgrade_proven") is False,
         "software successor scope drift")

    rows = change.get("old_to_new_leaf_map", [])
    need(isinstance(rows, list) and len(rows) == 2, "replacement mapping cardinality drift")
    rules = change.get("proposed_normal_rules", {})
    old_rows = old.get("leaves", [])
    new_rows = candidate.get("leaves", [])
    need(len(old_rows) == len(new_rows) == 115, "115-leaf catalog drift")
    expected = copy.deepcopy(old)
    expected["schema"] = "switch-acceptance-ledger-candidate-v3"
    expected["version"] = "current-baseline-p0-1-candidate-v3"
    expected["date"] = "2026-10-23"
    expected["purpose"] = (
        "Proposed versioned P0-1 replacement using the user-adopted current verifiable baseline. "
        "This is an inactive review candidate: no functional PASS, frozen semantic instance denominator, "
        "or overall percentage is awarded.")
    expected["freeze"]["known_requirement_set"] = "CURRENT_BASELINE_P0_1_CANDIDATE_REVIEW_PENDING"
    expected["freeze"]["known_requirement_set_note"] = (
        "A one-to-one candidate replaces two historical P0-1 leaf IDs; 49 parents, 115 catalog leaves "
        "and 21 classes remain. Scored semantic instances and product acceptance remain unresolved.")
    expected["freeze"]["reason"] = (
        "Candidate P0-1 predicates and applicability await formal review; historical 2.80 and "
        "switch-time facts remain unverified. Other support membership, full producer/writer/exit "
        "mapping, and reviewed semantic instances remain unresolved.")
    expected["candidate_scope_change"] = {
        "status": "INACTIVE_REVIEW_CANDIDATE",
        "mapping_ref": CHANGE.name,
        "predecessor_ledger_sha256": PINS[OLD],
        "decision_sha256": PINS[DECISION],
        "evidence_successor_sha256": PINS[SUCCESSOR],
        "old_leaf_results_not_carried_forward": True,
        "functional_pass_awarded": False,
        "active_entry_mapping_unchanged": True,
        "overall_denominator_unchanged_unfrozen": True,
    }
    if isinstance(rows, list) and len(rows) == 2 and len(old_rows) == 115:
        need(set(rules) == {pair[1] for pair in EXPECTED_PAIRS},
             "proposed rule set drift")
        for index, (row, (old_id, new_id, scenario)) in enumerate(zip(rows, EXPECTED_PAIRS)):
            need(row.get("old_id") == old_id and row.get("new_id") == new_id
                 and row.get("old_status") == row.get("new_status") == "NOT_REVIEWED"
                 and row.get("old_disposition") == "HISTORICAL_UNVERIFIABLE_RETAINED_NO_PASS"
                 and row.get("scenario_class_ids_before") == [scenario]
                 and row.get("scenario_class_ids_after") == [scenario]
                 and bool(row.get("new_title")) and bool(row.get("required_condition"))
                 and bool(row.get("software_evidence")),
                 "replacement identity, scope or status drift: " + old_id)
            if new_id == "P0-1.current-build-baseline":
                need(row.get("non_normal_not_applicable_reasons") == BUILD_AXIS_REASONS,
                     "current-build facet rationale drift")
            else:
                need("non_normal_not_applicable_reasons" not in row,
                     "unexpected source facet rationale override")
            rule = rules.get(new_id, {})
            need(rule.get("existing_leaf_id") == new_id
                 and rule.get("normal_required_condition") == row.get("required_condition")
                 and rule.get("formal_status") == "NOT_REVIEWED"
                 and bool(rule.get("boundary")),
                 "candidate normal rule drift: " + new_id)
            need(old_rows[index].get("id") == old_id
                 and old_rows[index].get("acceptance_status") == "NOT_REVIEWED"
                 and old_rows[index].get("applicability", {}).get("scenario_class_ids") == [scenario],
                 "historical leaf status or scope drift: " + old_id)
            expected["leaves"][index] = expected_leaf(
                old_rows[index], row, CHANGE, authority)
    need(candidate == expected,
         "candidate ledger differs from the explicit two-leaf replacement")
    need(set(row.get("id") for row in new_rows) ==
         (set(row.get("id") for row in old_rows) - {p[0] for p in EXPECTED_PAIRS}) |
         {p[1] for p in EXPECTED_PAIRS}, "candidate leaf set drift")
    need(Counter(row.get("acceptance_status") for row in old_rows) ==
         Counter(row.get("acceptance_status") for row in new_rows) ==
         Counter({"PARTIAL": 5, "NOT_REVIEWED": 110}),
         "functional status was promoted or removed")
    need(candidate.get("freeze", {}).get("overall_denominator_frozen") is False
         and candidate.get("freeze", {}).get("overall_denominator") is None
         and candidate.get("freeze", {}).get("overall_percentage") is None
         and candidate.get("accounting", {}).get("new_acceptance_passes_this_document") == 0,
         "candidate denominator or PASS drift")
    need(change.get("accounting_impact") == {
        "operation": "ONE_TO_ONE_LEAF_ID_REPLACEMENT_CANDIDATE",
        "parent_categories_before_after": [49, 49],
        "leaf_catalog_rows_before_after": [115, 115],
        "scenario_classes_before_after": [21, 21],
        "functional_passes_awarded": 0,
        "status_counts_before_after": {"PARTIAL": [5, 5], "NOT_REVIEWED": [110, 110]},
        "scored_unit_impact": "UNDETERMINED_NO_FROZEN_SEMANTIC_INSTANCE_REGISTRY",
        "overall_denominator_frozen": False,
        "overall_denominator": None,
        "overall_percentage": None,
        "overall_product_complete": False,
        "other_113_leaves_unchanged": True,
    }, "candidate accounting impact drift")
    need(change.get("historical_limits") == {
        "old_leaves_retained_in_predecessor": True,
        "historical_2_80_source_or_switch_time_restored": False,
        "legacy_equivalence_or_upgrade_proven": False,
        "device_card_carrier_qmi_mbim_acceptance": "UNVERIFIED",
        "rendered_gui_and_physical_audio_acceptance": "UNVERIFIED",
        "external_delivery_and_legacy_rollback": "UNVERIFIED",
    }, "historical or physical limits drift")
    return errors


def check(root: Path) -> dict:
    root = root.resolve(strict=True)
    paths = {relative: source_path(root, relative) for relative in (*PINS, NEW)}
    errors = [str(relative) + " bytes changed" for relative, digest in PINS.items()
              if sha256(paths[relative]) != digest]
    old, candidate, change, decision, successor = (
        json.loads(paths[relative].read_text())
        for relative in (OLD, NEW, CHANGE, DECISION, SUCCESSOR))
    errors.extend(validate(old, candidate, change, decision, successor))
    if sha256(paths[NEW]) != change.get("candidate_ledger", {}).get("sha256"):
        errors.append("candidate ledger digest does not match the mapping")
    old_raw = raw_leaf_objects(paths[OLD], 115)
    candidate_raw = raw_leaf_objects(paths[NEW], 115)
    if old_raw[2:] != candidate_raw[2:]:
        errors.append("other 113 leaf JSON objects changed at byte level")
    return {"candidate": str(NEW), "active": False, "catalog_leaves": len(candidate.get("leaves", [])),
            "functional_passes_awarded": 0, "overall_denominator_frozen": False,
            "valid": not errors, "errors": errors}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        result = check(args.root)
    except (OSError, ValueError, TypeError, KeyError, json.JSONDecodeError) as error:
        print("P0-1 candidate invalid: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result["valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
