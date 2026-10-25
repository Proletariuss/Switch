#!/usr/bin/env python3
"""Check the adopted current-baseline scope decision without awarding product PASS.

The default check needs only source files. ``--verify-artifacts`` additionally
rechecks the immutable i663 final-b build cut in the sibling build directory.
It never opens a device, user data, or a network connection.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
DECISION = Path("docs/optimization/all-rust/current-baseline-scope-decision.json")
LEDGER = Path("docs/optimization/all-rust/acceptance-ledger-2026-10-19.json")
# The adopted decision records LEDGER's path as it existed at the i663 cut.
# Keep that decision and its digest unchanged as the current ledger advances:
# this create-only versioned copy supplies the exact historical bytes instead.
HISTORICAL_LEDGER = Path("docs/optimization/all-rust/acceptance-ledger-2026-10-19-iteration-663.json")
BASELINES = Path("docs/optimization/all-rust/baseline-provenance.json")
SNAPSHOT = Path("docs/optimization/all-rust/iteration-663-source-snapshot.json")
BUILD_CUT = "iteration-663-current-code-final-b"
EXPECTED = {
    "ledger_sha256": "c4d76f5b11e64316de4c9e69ac2d754c0a8e2ab96bbeba06f446c9d570cbba7d",
    "baseline_sha256": "7c1a97894ae7f16cecd941e1fbc42d9b9281d53cbab45b1b56a4ce507a9ca72c",
    "snapshot_sha256": "5402927239df01df534e7cf2996c68c61f6c63951c6bef5980a083be76553d9f",
    "source_identity_sha256": "035c20e02a7f881547c2188ae148c620f6e1b167f823af719c3b1105d5bde425",
    "source_manifest_sha256": "293ca2162619ded2d1b6ffda506a7473315dbf76974e2a4800e659fe9b10317f",
    "audit_sha256": "39bc7e173b341d649c7e6007e5139782a64e9c03341f8d9f431238dcf3f58c09",
    "artifact_sha256": "a82a24e2a248ba1b6f90fcd5fb9aa7f777d3c2c9d2d844855dcd3c2d858061f3",
    "app_tree_manifest_sha256": "c07d69f26ab3d4fbe1ece66681f0fc0fe083a9c632b9f807169a2ecbe15532cf",
    "app_tree_sha256": "e2c3299132e9f1595b12be235670672f53b9a4189b4997214f498b9887555b53",
    "app_executable_sha256": "f52ac9c27767bcb2a59ec61b716254f6201ce5a90de1ddea85215b3d991030ad",
    "backend_sha256": "289434b696e95e3ee387733e8f4ef846b3ba24abe812bd4554d824e94efb9f6d",
    "gate_sha256": "324d1010ae9a0267432360c3ef10abf69228f1d589fc036fe20228b96de82839",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_file(root: Path, relative: Path) -> Path:
    path = root / relative
    resolved = path.resolve(strict=True)
    if path.is_symlink() or not resolved.is_file() or root.resolve() not in resolved.parents:
        raise ValueError("source evidence path is unsafe: " + str(relative))
    return path


def canonical_rows_sha256(rows: list[dict]) -> str:
    payload = json.dumps(rows, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(payload).hexdigest()


def app_tree_rows(root: Path) -> list[dict]:
    rows = []
    for entry in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        item = {"path": entry.relative_to(root).as_posix(), "mode": stat.S_IMODE(entry.lstat().st_mode)}
        if entry.is_symlink():
            item.update(kind="symlink", target=os.readlink(entry))
        elif entry.is_file():
            item.update(kind="file", size=entry.stat().st_size, sha256=sha256_file(entry))
        elif entry.is_dir():
            item.update(kind="directory")
        else:
            raise ValueError("unsupported app member: " + item["path"])
        rows.append(item)
    return rows


def validate(decision: dict, ledger: dict, baselines: dict) -> list[str]:
    errors = []

    def require(condition: bool, message: str) -> None:
        if not condition:
            errors.append(message)

    require(set(decision) == {"schema", "decision_id", "status", "authority",
                              "historical_facts", "current_scope", "current_release_candidate",
                              "verified_software_cut", "accounting"},
            "current baseline decision has unreviewed fields")
    require(decision.get("schema") == "switch.current-baseline-scope-decision.v1"
            and decision.get("decision_id") == "current-verifiable-baseline-2026-10-22"
            and decision.get("status") == "ADOPTED_SCOPE_DECISION_NO_FUNCTIONAL_ACCEPTANCE",
            "current baseline decision identity drift")
    authority = decision.get("authority", {})
    require(authority == {
        "kind": "direct_user_instruction", "local_date": "2026-10-22",
        "decision": "改用当前可验证基线", "this_round": "软件验收；DJI 实机验收待设备接入",
        "earlier_availability_reply": "不保留了",
    }, "current baseline user authority drift")
    history = decision.get("historical_facts", {})
    require(set(history) == {"ledger", "baseline_provenance", "legacy_equivalence_proven",
                             "two_eighty_source_or_switch_time_restore_proven", "old_leaf_dispositions"},
            "historical facts have unreviewed fields")
    require(history.get("ledger") == {"path": str(LEDGER), "sha256": EXPECTED["ledger_sha256"]}
            and history.get("baseline_provenance") == {
                "path": str(BASELINES), "sha256": EXPECTED["baseline_sha256"]},
            "historical ledger or provenance pin drift")
    require(history.get("legacy_equivalence_proven") is False
            and history.get("two_eighty_source_or_switch_time_restore_proven") is False,
            "historical 2.80 equivalence falsely claimed")
    expected_old = ["P0-1.working-baseline", "P0-1.baseline-280"]
    rows = history.get("old_leaf_dispositions", [])
    require([row.get("id") for row in rows] == expected_old
            and all(set(row) == {"id", "historical_ledger_status", "current_scope_disposition",
                                     "functional_pass_awarded"}
                    and row.get("historical_ledger_status") == "NOT_REVIEWED"
                    and row.get("current_scope_disposition") == "UNVERIFIABLE_HISTORICAL_NOT_PASS"
                    and row.get("functional_pass_awarded") is False for row in rows),
            "historical P0 leaves were hidden or promoted")
    actual_leaves = {row.get("id"): row for row in ledger.get("leaves", [])}
    require(all(actual_leaves.get(leaf_id, {}).get("mandatory") is True
                and actual_leaves.get(leaf_id, {}).get("acceptance_status") == "NOT_REVIEWED"
                and actual_leaves.get(leaf_id, {}).get("evidence", {}).get("normal", {}).get("status")
                == "NOT_REVIEWED" for leaf_id in expected_old),
            "historical ledger P0 status changed")
    provenance = baselines.get("roles", {})
    require(all(provenance.get(role, {}).get("recovery_state") == "UNAVAILABLE_USER_CONFIRMED_NOT_RETAINED"
                and provenance.get(role, {}).get("user_availability_confirmation", {}).get("reply") == "不保留了"
                and provenance.get(role, {}).get("user_availability_confirmation", {}).get("requirement_waiver_inferred") is False
                for role in ("final_280", "pre_all_rust")),
            "historical baseline unavailability changed")
    scope = decision.get("current_scope", {})
    new = scope.get("old_to_new_leaf_map", [])
    require(set(scope) == {"version", "integration_status", "old_to_new_leaf_map",
                           "all_other_requirement_leaves_retained_without_automatic_pass",
                           "device_card_carrier_qmi_mbim_upgrade_legacy_scope",
                           "historical_2_80_upgrade_compatibility", "source_cut_relation"}
            and scope.get("version") == "current-baseline-v1"
            and scope.get("integration_status") == "VERSIONED_SCOPE_DECISION_ONLY_NO_SCORING"
            and [(row.get("old_id"), row.get("new_id")) for row in new] == [
                ("P0-1.working-baseline", "P0-1.current-source-baseline"),
                ("P0-1.baseline-280", "P0-1.current-build-baseline")]
            and [row.get("software_evidence_status") for row in new] == [
                "VERIFIED_COLD_SOURCE_EXPORT_SUBSET", "VERIFIED_ISOLATED_COLD_BUILD_SUBSET"]
            and all(set(row) == {"old_id", "new_id", "software_evidence_status",
                                     "functional_acceptance_status", "remaining"}
                    and row.get("functional_acceptance_status") == "NOT_PASS" for row in new)
            and [row.get("remaining") for row in new] == [
                "Restore a complete current source tree from the pinned manifest in an isolated location and compare raw bytes and modes.",
                "Review clean-machine dependency closure and any final release identity; this cut proves neither 2.80 compatibility nor hardware behavior."]
            and scope.get("all_other_requirement_leaves_retained_without_automatic_pass") is True
            and scope.get("device_card_carrier_qmi_mbim_upgrade_legacy_scope") == "PENDING_REVIEW"
            and scope.get("historical_2_80_upgrade_compatibility") == "UNVERIFIED_NOT_CLAIMED"
            and scope.get("source_cut_relation") == (
                "The verified final-b build predates this metadata-only decision file. Its digest identifies that immutable build input; the post-decision source tree has a different identity and requires a new build before release."),
            "current scope mapping or unreviewed instances drift")
    require(decision.get("current_release_candidate") == {
        "version": "3.48", "build_number": "348", "backend_version": "3.48.0",
        "artifact_status": "NOT_BUILT", "dmg_path": None, "dmg_sha256": None,
        "acceptance_scope": "SOFTWARE_ONLY",
        "external_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
    }, "current release candidate identity or validation boundary drift")
    cut = decision.get("verified_software_cut", {})
    require(set(cut) == {"name", "build_source_identity_sha256", "selective_source_snapshot",
                         "full_source_manifest", "cold_build_audit", "artifact_audit", "app", "software_gate"},
            "verified software cut has unreviewed fields")
    expected_build = "build/clean-build/" + BUILD_CUT
    require(cut.get("name") == BUILD_CUT
            and cut.get("build_source_identity_sha256") == EXPECTED["source_identity_sha256"]
            and cut.get("selective_source_snapshot") == {"path": str(SNAPSHOT), "sha256": EXPECTED["snapshot_sha256"]}
            and cut.get("full_source_manifest") == {
                "path": expected_build + "/audit.json#/source", "entries": 3832,
                "canonical_array_sha256": EXPECTED["source_manifest_sha256"]}
            and cut.get("cold_build_audit") == {
                "path": expected_build + "/audit.json", "sha256": EXPECTED["audit_sha256"]}
            and cut.get("artifact_audit") == {
                "path": expected_build + "/artifact-audit.json", "sha256": EXPECTED["artifact_sha256"],
                "status": "passed"},
            "verified source or cold-build identity drift")
    app = cut.get("app", {})
    require(set(app) == {"relative_path", "version", "build_number", "backend_version",
                         "tree_manifest", "tree_manifest_sha256", "tree_sha256", "executable_sha256",
                         "backend_executable_sha256", "codesign_deep_strict_verified", "signing_mode"}
            and app.get("relative_path") == expected_build + "/dist/Switch.app"
            and (app.get("version"), app.get("build_number"), app.get("backend_version"))
            == ("3.46", "346", "3.46.0")
            and app.get("tree_manifest") == "build/reports/iteration-663-final-b-app-tree-manifest.json"
            and app.get("tree_manifest_sha256") == EXPECTED["app_tree_manifest_sha256"]
            and app.get("tree_sha256") == EXPECTED["app_tree_sha256"]
            and app.get("executable_sha256") == EXPECTED["app_executable_sha256"]
            and app.get("backend_executable_sha256") == EXPECTED["backend_sha256"]
            and app.get("codesign_deep_strict_verified") is True
            and app.get("signing_mode") == "ad_hoc",
            "verified app identity drift")
    require(cut.get("software_gate") == {
        "path": "build/reports/iteration-663-migration-gates.log",
        "sha256": EXPECTED["gate_sha256"], "status": "passed"},
        "software gate evidence drift")
    accounting = decision.get("accounting", {})
    require(accounting == {
        "historical_p0_passes_awarded": 0, "new_current_p0_passes_awarded": 0,
        "other_functional_passes_awarded": 0, "overall_denominator_frozen": False,
        "overall_denominator": None, "overall_percentage": None,
        "overall_product_complete": False},
        "current scope falsely claims product acceptance")
    return errors


def validate_active_scope(ledger: dict) -> list[str]:
    """Keep the adopted history distinct from the now-active current scope."""
    errors = []
    by_id = {row.get("id"): row for row in ledger.get("leaves", [])}
    old_ids = {"P0-1.working-baseline", "P0-1.baseline-280"}
    new_ids = {"P0-1.current-source-baseline", "P0-1.current-build-baseline"}
    if (len(ledger.get("leaves", [])) != 115 or len(by_id) != 115
            or old_ids & by_id.keys()
            or any(by_id.get(leaf_id, {}).get("acceptance_status") != "NOT_REVIEWED"
                   or by_id.get(leaf_id, {}).get("evidence", {}).get("normal", {}).get("status")
                   != "NOT_REVIEWED" for leaf_id in new_ids)
            or ledger.get("scope_activation", {}).get("status")
            != "ACTIVE_SCOPE_RULE_NO_FUNCTIONAL_ACCEPTANCE"
            or ledger.get("freeze", {}).get("overall_denominator_frozen") is not False
            or ledger.get("freeze", {}).get("overall_percentage") is not None):
        errors.append("active current-baseline P0 scope or no-pass boundary changed")
    return errors


def verify_artifacts(decision: dict, source_root: Path) -> list[str]:
    """Recheck the earlier immutable cut; never compare it to post-decision source identity."""
    errors = []
    build = source_root.parent / "build"
    cut = decision["verified_software_cut"]
    capsule = build / "clean-build" / BUILD_CUT
    paths = {
        "cold audit": (capsule / "audit.json", EXPECTED["audit_sha256"]),
        "artifact audit": (capsule / "artifact-audit.json", EXPECTED["artifact_sha256"]),
        "App tree manifest": (build / "reports/iteration-663-final-b-app-tree-manifest.json",
                              EXPECTED["app_tree_manifest_sha256"]),
        "software gate": (build / "reports/iteration-663-migration-gates.log", EXPECTED["gate_sha256"]),
    }
    app_root = capsule / "dist/Switch.app"
    paths["App executable"] = (app_root / "Contents/MacOS/Switch", EXPECTED["app_executable_sha256"])
    paths["Rust backend"] = (app_root / "Contents/Resources/backend/switchd-rs", EXPECTED["backend_sha256"])
    for label, (path, expected) in paths.items():
        if not path.is_file() or path.is_symlink() or sha256_file(path) != expected:
            errors.append(label + " missing or hash mismatch")
    if errors:
        return errors
    audit = json.loads((capsule / "audit.json").read_text())
    artifact = json.loads((capsule / "artifact-audit.json").read_text())
    manifest = json.loads((build / "reports/iteration-663-final-b-app-tree-manifest.json").read_text())
    if (len(audit.get("source", [])) != 3832
            or canonical_rows_sha256(audit.get("source", [])) != EXPECTED["source_manifest_sha256"]):
        errors.append("complete source manifest mismatch")
    if (artifact.get("status") != "passed"
            or artifact.get("source_digest") != EXPECTED["source_identity_sha256"]
            or artifact.get("audit_sha256") != EXPECTED["audit_sha256"]
            or artifact.get("app") != str(app_root)):
        errors.append("artifact audit or source identity mismatch")
    if (manifest.get("tree_sha256") != EXPECTED["app_tree_sha256"]
            or manifest.get("root") != str(app_root)
            or manifest.get("entries") != app_tree_rows(app_root)
            or canonical_rows_sha256(manifest.get("entries", [])) != EXPECTED["app_tree_sha256"]):
        errors.append("App tree manifest mismatch")
    for binary in artifact.get("binaries", []):
        if binary.get("path") == "Contents/MacOS/Switch" and binary.get("sha256") != EXPECTED["app_executable_sha256"]:
            errors.append("artifact App executable hash mismatch")
        if binary.get("path") == "Contents/Resources/backend/switchd-rs" and binary.get("sha256") != EXPECTED["backend_sha256"]:
            errors.append("artifact Rust backend hash mismatch")
    if not any(row.get("path") == "Contents/MacOS/Switch" for row in artifact.get("binaries", [])):
        errors.append("artifact audit lacks App executable")
    if not any(row.get("path") == "Contents/Resources/backend/switchd-rs" for row in artifact.get("binaries", [])):
        errors.append("artifact audit lacks Rust backend")
    try:
        signature = subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app_root)],
                                   capture_output=True, check=False, timeout=20)
        if signature.returncode != 0:
            errors.append("App code signature no longer verifies")
    except (OSError, subprocess.TimeoutExpired):
        errors.append("App code signature cannot be verified")
    return errors


def check(root: Path, artifacts: bool = False) -> dict:
    root = root.resolve()
    decision_path = source_file(root, DECISION)
    ledger_path = source_file(root, LEDGER)
    historical_ledger_path = source_file(root, HISTORICAL_LEDGER)
    baselines_path = source_file(root, BASELINES)
    snapshot_path = source_file(root, SNAPSHOT)
    decision = json.loads(decision_path.read_text())
    active_ledger = json.loads(ledger_path.read_text())
    historical_ledger = json.loads(historical_ledger_path.read_text())
    baselines = json.loads(baselines_path.read_text())
    errors = validate(decision, historical_ledger, baselines)
    errors.extend(validate_active_scope(active_ledger))
    # The adopted decision remains bound to the immutable i663 ledger.  The
    # current scope is checked separately; it cannot rewrite historical facts.
    if sha256_file(historical_ledger_path) != EXPECTED["ledger_sha256"]:
        errors.append("historical ledger bytes changed")
    if sha256_file(baselines_path) != EXPECTED["baseline_sha256"]:
        errors.append("historical baseline bytes changed")
    if sha256_file(snapshot_path) != EXPECTED["snapshot_sha256"]:
        errors.append("selective source snapshot bytes changed")
    if artifacts:
        errors.extend(verify_artifacts(decision, root))
    return {
        "decision_id": decision.get("decision_id"),
        "source_only_check": not artifacts,
        "historical_p0_passes_awarded": 0,
        "current_p0_functional_passes_awarded": 0,
        "overall_denominator_frozen": False,
        "overall_percentage": None,
        "errors": errors,
        "valid": not errors,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--verify-artifacts", action="store_true")
    args = parser.parse_args()
    try:
        result = check(args.root, args.verify_artifacts)
    except (OSError, ValueError, TypeError, KeyError, json.JSONDecodeError) as error:
        print("current baseline decision invalid: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result["valid"] else 1


if __name__ == "__main__":
    sys.exit(main())
