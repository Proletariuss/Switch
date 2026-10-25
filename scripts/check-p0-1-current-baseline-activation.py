#!/usr/bin/env python3
"""Fail closed on the adopted P0-1 scope replacement, without awarding PASS."""
from __future__ import annotations

from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
BASE = Path("docs/optimization/all-rust")
FILES = {
    "historical_i677": (BASE / "acceptance-ledger-2026-10-19-iteration-677.json",
                        "e72a76fb9f8deca8916fccd25ed7d4aab4bcb9572175dd9be1bcbaf4115e673b"),
    "pre_activation_i680": (BASE / "acceptance-ledger-2026-10-19-iteration-680-pre-activation.json",
                            "33f41e2cf9c5a59b47c4e0fe6b4c3c62f0f6fe0874fb5080e30beb6ecbdb4b07"),
    "inactive_candidate": (BASE / "acceptance-ledger-candidate-v3-2026-10-23.json",
                           "41f3e1869e83346eacfbe095c7c1d3bf15bd28c27795c216364d47e17765ffc5"),
    "adopted_decision": (BASE / "current-baseline-scope-decision.json",
                         "b8b1446f1a46f1cdb63258e48a72fc5bc1037bdd816041b0ea546a3fb8a9b13e"),
    "evidence_successor": (BASE / "current-baseline-scope-successor-2026-10-23.json",
                           "33bccc9fbaed31e0f41d2dd0f56459100b2621d5314966ef49a97396041883e6"),
    "activation": (BASE / "p0-1-current-baseline-activation-2026-10-23.json",
                   "93275630888c7c827b1672cf5678ad1e0c5257adfda2d8fd20cbd89778df4ee4"),
}
ACTIVE = BASE / "acceptance-ledger-2026-10-19.json"
ENTRY = BASE / "entry-acceptance-map.json"
PROTOCOL = BASE / "acceptance-scope-protocol.json"
OLD_IDS = ("P0-1.working-baseline", "P0-1.baseline-280")
NEW_IDS = ("P0-1.current-source-baseline", "P0-1.current-build-baseline")
SCENARIOS = ("SW", "DELIVERY")
OTHER_113_SHA = "e1ed70945cb3565c2f4eb5d0c59f49fa39ef12852a5b39a394fb91860b56ae43"
FORMAL_REASON = (
    "当前可验证基线的范围规则已按用户指令正式采纳；目标版本的源码恢复与同源构建证据仍须逐版审定，"
    "尚未授予功能PASS。旧2.80及切换前历史事实另存且未通过。"
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_path(root: Path, relative: Path) -> Path:
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("unsafe source evidence path: " + str(relative))
    path = root / relative
    resolved = path.resolve(strict=True)
    if path.is_symlink() or not path.is_file() or root.resolve() not in resolved.parents:
        raise ValueError("absent or unsafe source evidence: " + str(relative))
    return path


def raw_leaf_objects(path: Path, count: int = 115) -> list[bytes]:
    text = path.read_text()
    marker = re.search(r'(?m)^  "leaves": \[$', text)
    if marker is None:
        raise ValueError("missing top-level leaves array")
    decoder = json.JSONDecoder()
    offset = marker.end()
    rows = []
    for _ in range(count):
        while offset < len(text) and text[offset] in " \t\r\n,":
            offset += 1
        if offset >= len(text) or text[offset] != "{":
            raise ValueError("missing leaf object")
        row, end = decoder.raw_decode(text, offset)
        if not isinstance(row, dict):
            raise ValueError("non-object leaf")
        rows.append(text[offset:end].encode())
        offset = end
    while offset < len(text) and text[offset] in " \t\r\n":
        offset += 1
    if offset >= len(text) or text[offset] != "]":
        raise ValueError("unexpected extra leaf object")
    return rows


def other_leaf_digest(path: Path) -> str:
    return hashlib.sha256(b"\n".join(raw_leaf_objects(path)[2:])).hexdigest()


def load_documents(root: Path) -> dict:
    docs = {name: json.loads(source_path(root, relative).read_text())
            for name, (relative, _) in FILES.items()}
    docs["active"] = json.loads(source_path(root, ACTIVE).read_text())
    docs["entry"] = json.loads(source_path(root, ENTRY).read_text())
    docs["protocol"] = json.loads(source_path(root, PROTOCOL).read_text())
    return docs


def validate(docs: dict) -> list[str]:
    errors = []

    def require(ok: bool, message: str) -> None:
        if not ok:
            errors.append(message)

    old = docs["historical_i677"]
    before = docs["pre_activation_i680"]
    candidate = docs["inactive_candidate"]
    decision = docs["adopted_decision"]
    successor = docs["evidence_successor"]
    activation = docs["activation"]
    active = docs["active"]
    entry = docs["entry"]
    protocol = docs["protocol"]
    require(decision.get("authority", {}).get("decision") == "改用当前可验证基线"
            and decision.get("historical_facts", {}).get("two_eighty_source_or_switch_time_restore_proven") is False,
            "direct authority or historical unavailable fact drift")
    require(successor.get("ledger_integration", {}).get("mode") == "EVIDENCE_ONLY_NO_FORMAL_LEAF_REWRITE"
            and successor.get("current_build", {}).get("version") == "3.48"
            and successor.get("current_build", {}).get("build_number") == "348"
            and successor.get("ledger_integration", {}).get("current_p0_functional_passes_awarded") == 0,
            "version-scoped software receipt falsely promoted")
    expected_pins = {name: {"path": str(path), "sha256": digest}
                     for name, (path, digest) in FILES.items()}
    require(set(activation) == {
                "schema", "activation_id", "status", "authority", "historical_i677_ledger",
                "pre_activation_i680_ledger", "inactive_v3_candidate", "evidence_only_successor",
                "active_documents", "replacement_rules", "other_113_raw_leaf_objects_sha256",
                "accounting", "historical_limits"}
            and activation.get("schema") == "switch.p0-1-current-baseline-activation.v1"
            and activation.get("status") == "ADOPTED_SCOPE_RULE_NO_FUNCTIONAL_ACCEPTANCE"
            and activation.get("authority") == {
                "kind": "direct_user_instruction", "decision": "同意改用当前可验证基线",
                "decision_file": expected_pins["adopted_decision"]},
            "activation authority or schema drift")
    require({k: activation.get(k, {}).get("sha256") for k in (
                "historical_i677_ledger", "pre_activation_i680_ledger",
                "inactive_v3_candidate", "evidence_only_successor")}
            == {"historical_i677_ledger": expected_pins["historical_i677"]["sha256"],
                "pre_activation_i680_ledger": expected_pins["pre_activation_i680"]["sha256"],
                "inactive_v3_candidate": expected_pins["inactive_candidate"]["sha256"],
                "evidence_only_successor": expected_pins["evidence_successor"]["sha256"]}
            and all(activation.get(key, {}).get("path") == expected_pins[name]["path"]
                    for key, name in (("historical_i677_ledger", "historical_i677"),
                                      ("pre_activation_i680_ledger", "pre_activation_i680"),
                                      ("inactive_v3_candidate", "inactive_candidate"),
                                      ("evidence_only_successor", "evidence_successor")))
            and activation.get("evidence_only_successor", {}).get("version_scope") == "3.48/build348 only",
            "activation predecessor or candidate pin drift")
    require(activation.get("active_documents") == {
                "ledger": str(ACTIVE), "entry_map": str(ENTRY), "scope_protocol": str(PROTOCOL)},
            "activation target document drift")
    expected_rules = [{"historical_leaf_id": old_id, "current_leaf_id": new_id,
                       "scenario_class_ids": [scenario],
                       "normal_required_condition": candidate["leaves"][i]["evidence"]["normal"]["required_condition"],
                       "formal_acceptance_status": "NOT_REVIEWED", "functional_pass_awarded": False}
                      for i, (old_id, new_id, scenario) in enumerate(zip(OLD_IDS, NEW_IDS, SCENARIOS))]
    require(activation.get("replacement_rules") == expected_rules,
            "replacement rules or scenario scope drift")
    require(activation.get("other_113_raw_leaf_objects_sha256") == OTHER_113_SHA,
            "other 113 leaf pin drift")
    require(activation.get("accounting") == {
                "parent_categories": 49, "catalog_leaves": 115, "scenario_classes": 21,
                "status_counts": {"PARTIAL": 5, "NOT_REVIEWED": 110},
                "new_functional_passes": 0, "overall_denominator_frozen": False,
                "overall_denominator": None, "overall_percentage": None,
                "overall_product_complete": False},
            "activation accounting or no-pass boundary drift")
    require(activation.get("historical_limits") == {
                "two_eighty_or_switch_time_restored": False,
                "legacy_upgrade_equivalence_proven": False,
                "current_source_restore_receipt_version": "3.48/build348",
                "current_build_receipt_version": "3.48/build348",
                "new_target_version_needs_matching_receipts": True,
                "device_carrier_audio_external_and_long_run_acceptance": "UNVERIFIED"},
            "historical or device/product limitation drift")
    for source in (old, before):
        require([row.get("id") for row in source.get("leaves", [])[:2]] == list(OLD_IDS)
                and all(row.get("acceptance_status") == "NOT_REVIEWED"
                        for row in source.get("leaves", [])[:2]),
                "historical P0 leaves changed or promoted")
    require([row.get("id") for row in candidate.get("leaves", [])[:2]] == list(NEW_IDS)
            and candidate.get("candidate_scope_change", {}).get("status") == "INACTIVE_REVIEW_CANDIDATE",
            "inactive candidate drift")
    leaves = active.get("leaves", [])
    require(len(leaves) == 115 and [row.get("id") for row in leaves[:2]] == list(NEW_IDS)
            and not ({row.get("id") for row in leaves} & set(OLD_IDS)),
            "active P0 leaf identity drift")
    require(len(active.get("parents", [])) == 49
            and len(active.get("applicability_matrix", [])) == 21
            and Counter(row.get("acceptance_status") for row in leaves)
            == Counter({"PARTIAL": 5, "NOT_REVIEWED": 110}),
            "active catalog count or PASS drift")
    for index, new_id in enumerate(NEW_IDS):
        expected = copy.deepcopy(candidate["leaves"][index])
        expected["requirement_sources"].append({
            "path": "src/" + str(FILES["activation"][0]),
            "sha256": FILES["activation"][1],
            "clause": "replacement_rules and historical_limits"})
        expected["status_reason"] = FORMAL_REASON
        expected["normal_review_rule_ref"] = (
            "acceptance-scope-protocol.json#/P0_normal_rules/" + new_id)
        require(index < len(leaves) and leaves[index] == expected,
                "active P0 leaf content differs from adopted candidate: " + new_id)
    require(active.get("version") == "reviewed-current-baseline-p0-1-v3"
            and active.get("scope_activation") == {
                "status": "ACTIVE_SCOPE_RULE_NO_FUNCTIONAL_ACCEPTANCE",
                "decision": expected_pins["activation"],
                "historical_p0_leaf_results_carried_forward": False,
                "other_113_leaf_objects_unchanged": True,
                "overall_denominator_frozen": False,
                "overall_percentage": None}
            and active.get("freeze", {}).get("overall_denominator_frozen") is False
            and active.get("freeze", {}).get("overall_denominator") is None
            and active.get("freeze", {}).get("overall_percentage") is None
            and active.get("accounting", {}).get("new_acceptance_passes_this_document") == 0,
            "active scope activation or denominator drift")
    required = set(entry.get("required_leaf_ids", []))
    before_required = set(before.get("leaves", [])[i]["id"] for i in range(115))
    require(required == before_required - set(OLD_IDS) | set(NEW_IDS)
            and len(entry.get("required_leaf_ids", [])) == len(required) == 115
            and entry["required_leaf_ids"] == sorted(required)
            and entry.get("overall_product_complete") is False,
            "active entry-map requirement set drift")
    rules = protocol.get("P0_normal_rules", {})
    require(set(rules) == {row["id"] for row in leaves if row["stage"] == "P0"},
            "active scope protocol P0 rule set drift")
    historical_rules = protocol.get("historical_P0_normal_rules", {})
    require(historical_rules.get("historical_i677_ledger_sha256") == FILES["historical_i677"][1]
            and set(historical_rules.get("archived_rules", {})) == set(OLD_IDS)
            and historical_rules.get("status") == "HISTORICAL_UNVERIFIABLE_NOT_ACTIVE_NO_PASS",
            "historical P0 rules were hidden or promoted")
    for index, new_id in enumerate(NEW_IDS):
        rule = rules.get(new_id, {})
        require(rule.get("existing_leaf_id") == new_id
                and rule.get("normal_required_condition_unchanged")
                == expected_rules[index]["normal_required_condition"]
                and rule.get("activation_decision_ref") == expected_pins["activation"]
                and rule.get("acceptance_status_unchanged") == "NOT_REVIEWED",
                "adopted P0 normal rule drift: " + new_id)
    require(protocol.get("P0_scope_activation") == {
                "status": "ACTIVE_CURRENT_VERIFIABLE_BASELINE_NO_FUNCTIONAL_ACCEPTANCE",
                "decision_ref": expected_pins["activation"],
                "old_leaf_ids": list(OLD_IDS), "new_leaf_ids": list(NEW_IDS),
                "catalog_leaves": 115, "overall_denominator_frozen": False},
            "active scope protocol activation drift")
    return errors


def check(root: Path = ROOT) -> dict:
    root = root.resolve(strict=True)
    errors = []
    for name, (relative, digest) in FILES.items():
        if sha256(source_path(root, relative)) != digest:
            errors.append(name + " immutable source bytes changed")
    docs = load_documents(root)
    errors.extend(validate(docs))
    before_path = source_path(root, FILES["pre_activation_i680"][0])
    active_path = source_path(root, ACTIVE)
    if (other_leaf_digest(before_path) != OTHER_113_SHA
            or other_leaf_digest(active_path) != OTHER_113_SHA
            or raw_leaf_objects(before_path)[2:] != raw_leaf_objects(active_path)[2:]):
        errors.append("other 113 raw leaf JSON objects changed")
    return {"activation_id": docs["activation"].get("activation_id"),
            "active": True, "catalog_leaves": len(docs["active"].get("leaves", [])),
            "functional_passes_awarded": 0, "overall_denominator_frozen": False,
            "valid": not errors, "errors": errors}


def main() -> int:
    try:
        result = check()
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
        print("P0-1 activation invalid: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result["valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
