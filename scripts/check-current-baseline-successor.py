#!/usr/bin/env python3
"""Fail closed on the evidence-only current-baseline successor; read-only."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
DOCUMENT = Path("docs/optimization/all-rust/current-baseline-scope-successor-2026-10-23.json")
SOURCE = "b50cd9f5641e1d1d9006b64dc3f9f107272b1ae750bab89a20c511f017073c33"
MANIFEST = "c01e243f8185f2909b98dde825cb183173c64f1d0b55866d4bcb70dd43d60f0e"
PREDECESSOR_SHA = "b8b1446f1a46f1cdb63258e48a72fc5bc1037bdd816041b0ea546a3fb8a9b13e"
EXPECTED_PINS = {
    "predecessor": ("docs/optimization/all-rust/current-baseline-scope-decision.json", PREDECESSOR_SHA),
    "cold_audit": ("build/clean-build/iteration-677-no-device-" + SOURCE + "/audit.json",
                   "35d22c737a890f7bfe05325fad31950b4e029db9edc4fddfc0205c430969c93b"),
    "artifact_audit": ("build/clean-build/iteration-677-no-device-" + SOURCE + "/artifact-audit.json",
                       "d6b4ceb2d851a988b7ab908a2bf549a31edb0399afb1b7d4ebdb1fb81e245703"),
    "restore_receipt": ("build/reports/iteration-678-current-source-restore.json",
                        "4a667d899cc6c6d580349e22fdb7068fd2b0f940a1973bb671741d47fc9351cf"),
    "software_receipt": ("build/reports/iteration-677-no-device-software-acceptance.json",
                         "9f43181215ee3b60970597c118e722407a0a6540595aef31d2ac9c96fe0e5e1a"),
    "release_manifest": ("dist/release-manifest.json",
                         "f6736d224ee07ddb021d13e498f7bed44fed237da7c22935b3f48d4b799871b6"),
    "dmg": ("dist/Switch-3.48-macOS27.dmg",
            "821843a9c793c930078c7a624d1577b87bacd81666d744d122f186dab969d750"),
    "historical_i677_ledger": ("docs/optimization/all-rust/acceptance-ledger-2026-10-19-iteration-677.json",
                                "e72a76fb9f8deca8916fccd25ed7d4aab4bcb9572175dd9be1bcbaf4115e673b"),
}
SOURCE_PINS = ("predecessor", "historical_i677_ledger")
EXTERNAL_PINS = tuple(name for name in EXPECTED_PINS if name not in SOURCE_PINS)
ARCHIVED_RELEASE_MANIFEST = "dist/archive/Switch-3.48-macOS27-manifest.json"
LEDGER_AT_CUT = "docs/optimization/all-rust/acceptance-ledger-2026-10-19.json"
LEDGER = EXPECTED_PINS["historical_i677_ledger"][0]
ACTIVE_LEDGER = LEDGER_AT_CUT
HISTORICAL = ("P0-1.working-baseline", "P0-1.baseline-280")
TRACKS = ("P0-1.current-source-baseline", "P0-1.current-build-baseline")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def canonical_sha(rows: list) -> str:
    return hashlib.sha256(json.dumps(rows, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode()).hexdigest()


def pin(value: object, name: str, errors: list[str]) -> None:
    path, digest = EXPECTED_PINS[name]
    if value != {"path": path, "sha256": digest}:
        errors.append(name + " pin drift")


def validate_source_scope(doc: dict, predecessor: dict, ledger: dict) -> list[str]:
    """Validate immutable source-side scope and pins without external receipts.

    This is only a scope check. The versioned i677 build evidence still needs
    ``validate``/``check`` and cannot confer acceptance on a later source cut.
    """
    errors: list[str] = []

    def require(condition: bool, message: str) -> None:
        if not condition:
            errors.append(message)

    require(set(doc) == {"schema", "successor_id", "predecessor", "source_cut",
                         "scope_mapping", "current_build", "ledger_integration", "limitations"},
            "successor fields drift")
    require(doc.get("schema") == "switch.current-baseline-scope-successor.v1"
            and doc.get("successor_id") == "current-baseline-software-evidence-2026-10-23",
            "successor identity drift")
    pin(doc.get("predecessor"), "predecessor", errors)
    require(predecessor.get("status") == "ADOPTED_SCOPE_DECISION_NO_FUNCTIONAL_ACCEPTANCE"
            and predecessor.get("decision_id") == "current-verifiable-baseline-2026-10-22"
            and predecessor.get("historical_facts", {}).get("two_eighty_source_or_switch_time_restore_proven") is False,
            "predecessor authority or history drift")

    cut = doc.get("source_cut", {})
    require(set(cut) == {"identity_sha256", "full_manifest_entries",
                         "full_manifest_canonical_array_sha256", "cold_audit", "artifact_audit"}
            and cut.get("identity_sha256") == SOURCE
            and cut.get("full_manifest_entries") == 3928
            and cut.get("full_manifest_canonical_array_sha256") == MANIFEST,
            "current source cut drift")
    pin(cut.get("cold_audit"), "cold_audit", errors)
    pin(cut.get("artifact_audit"), "artifact_audit", errors)
    rows = doc.get("scope_mapping", [])
    require(isinstance(rows, list) and len(rows) == 2, "scope mapping cardinality drift")
    if isinstance(rows, list) and len(rows) == 2:
        for i, (old, track, status, receipt_name) in enumerate(zip(
                HISTORICAL, TRACKS,
                ("VERIFIED_CURRENT_SOURCE_RESTORE_ONLY", "VERIFIED_CURRENT_BUILD_SOFTWARE_ONLY"),
                ("restore_receipt", "software_receipt"))):
            row = rows[i]
            require(isinstance(row, dict) and set(row) == {
                "historical_leaf", "current_evidence_track", "software_evidence",
                "formal_leaf_status", "functional_pass_awarded", "receipt"}
                and row.get("historical_leaf") == old
                and row.get("current_evidence_track") == track
                and row.get("software_evidence") == status
                and row.get("formal_leaf_status") == "NOT_REVIEWED"
                and row.get("functional_pass_awarded") is False,
                "mapping status drift: " + old)
            if isinstance(row, dict):
                pin(row.get("receipt"), receipt_name, errors)

    build = doc.get("current_build", {})
    require(set(build) == {"version", "build_number", "backend_version", "release_manifest", "dmg", "scope"}
            and (build.get("version"), build.get("build_number"), build.get("backend_version"))
            == ("3.48", "348", "3.48.0")
            and build.get("scope") == "NO_DEVICE_SOFTWARE_RELEASE", "current build identity drift")
    pin(build.get("release_manifest"), "release_manifest", errors)
    pin(build.get("dmg"), "dmg", errors)
    require(doc.get("ledger_integration") == {
        "mode": "EVIDENCE_ONLY_NO_FORMAL_LEAF_REWRITE", "formal_ledger": LEDGER_AT_CUT,
        "formal_leaf_count": 115, "historical_p0_passes_awarded": 0,
        "current_p0_functional_passes_awarded": 0, "overall_denominator_frozen": False,
        "overall_denominator": None, "overall_percentage": None,
        "overall_product_complete": False}, "ledger accounting drift")
    require(doc.get("limitations") == {
        "historical_2_80_source_or_switch_time_restored": False,
        "legacy_equivalence_or_upgrade_proven": False,
        "device_card_carrier_qmi_mbim_acceptance": "UNVERIFIED_EXCLUDED_OR_PENDING_REVIEW",
        "rendered_gui_and_physical_audio_acceptance": "UNVERIFIED",
        "external_delivery_and_legacy_rollback": "UNVERIFIED",
        "other_113_formal_leaves_automatically_promoted": False}, "acceptance boundary drift")

    leaves = ledger.get("leaves", [])
    by_id = {row.get("id"): row for row in leaves if isinstance(row, dict)}
    require(len(leaves) == 115 and len(by_id) == 115
            and all(by_id.get(old, {}).get("acceptance_status") == "NOT_REVIEWED"
                    and by_id.get(old, {}).get("evidence", {}).get("normal", {}).get("status") == "NOT_REVIEWED"
                    for old in HISTORICAL)
            and all(track not in by_id for track in TRACKS),
            "formal 115-leaf ledger or historical P0 dispositions drift")
    require(ledger.get("accounting", {}).get("candidate_leaf_rows") == 115
            and ledger.get("accounting", {}).get("new_acceptance_passes_this_document") == 0,
            "formal ledger accounting drift")
    return errors


def validate(doc: dict, predecessor: dict, ledger: dict, restore: dict,
             software: dict, audit: dict, artifact: dict, release: dict) -> list[str]:
    errors = validate_source_scope(doc, predecessor, ledger)

    def require(condition: bool, message: str) -> None:
        if not condition:
            errors.append(message)

    require(restore.get("schema") == "switch.current-source-restore.v1"
            and restore.get("requirement") == TRACKS[0]
            and restore.get("status") == "VERIFIED_CURRENT_SOURCE_RESTORE_ONLY"
            and restore.get("expected_current_source_digest") == SOURCE
            and restore.get("source_digests") == {key: SOURCE for key in ("capsule", "live", "restored")}
            and restore.get("audit_sha256") == EXPECTED_PINS["cold_audit"][1]
            and restore.get("manifest_source_rows") == restore.get("source_files_restored_and_byte_compared") == restore.get("file_modes_verified") == 3928
            and restore.get("manifest_canonical_array_sha256") == MANIFEST
            and restore.get("manifest_source_bytes") == restore.get("source_bytes_restored_and_byte_compared")
            and restore.get("disposable_restore_removed") is True
            and restore.get("functional_acceptance_awarded") is False
            and restore.get("overall_product_pass_awarded") is False,
            "current-source restore receipt drift")
    require(len(audit.get("source", [])) == 3928
            and canonical_sha(audit.get("source", [])) == MANIFEST,
            "full cold source manifest drift")
    require(artifact.get("status") == "passed"
            and artifact.get("software_tests") == "passed"
            and artifact.get("source_digest") == SOURCE
            and artifact.get("audit_sha256") == EXPECTED_PINS["cold_audit"][1],
            "cold artifact audit drift")
    require(software.get("software_acceptance_complete") is True
            and software.get("product_acceptance_complete") is False
            and software.get("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER"
            and software.get("source_digest") == SOURCE
            and (software.get("app_version"), software.get("build_number"), software.get("backend_version"))
            == ("3.48", "348", "3.48.0")
            and software.get("evidence", {}).get("clean_build_audit") == build_evidence("cold_audit")
            and software.get("evidence", {}).get("clean_build_artifact") == build_evidence("artifact_audit"),
            "current-build software receipt drift")
    require(release.get("source_digest") == SOURCE
            and (release.get("version"), release.get("build"), release.get("backend_version"))
            == ("3.48", "348", "3.48.0")
            and release.get("acceptance", {}).get("product_acceptance_complete") is False
            and release.get("acceptance", {}).get("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER"
            and release.get("acceptance", {}).get("evidence_sha256") == EXPECTED_PINS["software_receipt"][1]
            and release.get("acceptance", {}).get("verified_dmg_sha256") == EXPECTED_PINS["dmg"][1]
            and release.get("artifacts") and release["artifacts"][0].get("sha256") == EXPECTED_PINS["dmg"][1],
            "release manifest or DMG binding drift")
    return errors


def validate_active_scope(ledger: dict) -> list[str]:
    """Confirm the version-scoped historical receipt was not promoted in the active ledger."""
    by_id = {row.get("id"): row for row in ledger.get("leaves", [])}
    if (len(ledger.get("leaves", [])) == len(by_id) == 115
            and all(old not in by_id for old in HISTORICAL)
            and all(by_id.get(track, {}).get("acceptance_status") == "NOT_REVIEWED"
                    for track in TRACKS)
            and ledger.get("scope_activation", {}).get("status")
            == "ACTIVE_SCOPE_RULE_NO_FUNCTIONAL_ACCEPTANCE"
            and ledger.get("freeze", {}).get("overall_denominator_frozen") is False
            and ledger.get("freeze", {}).get("overall_percentage") is None):
        return []
    return ["active successor P0 scope or no-pass boundary changed"]


def build_evidence(name: str) -> dict:
    path, digest = EXPECTED_PINS[name]
    return {"path": path, "sha256": digest}


def evidence_location(name: str) -> str:
    """Keep the i677 receipt bytes pinned while locating its archived manifest.

    The immutable 3.48 receipt named the then-current release-manifest path.
    Later releases replace that path, so full historical verification reads
    the exact same bytes from the versioned archive instead.
    """
    return ARCHIVED_RELEASE_MANIFEST if name == "release_manifest" else EXPECTED_PINS[name][0]


def checked_path(workspace: Path, relative: str) -> Path:
    path = workspace / relative
    resolved = path.resolve(strict=True)
    if path.is_symlink() or not path.is_file() or workspace not in resolved.parents:
        raise ValueError("unsafe or absent evidence: " + relative)
    return path


def check_source_only(root: Path) -> dict:
    """Check source-side scope in a cold capsule that has no i677 receipts.

    This never verifies a build, source restoration, or release artifact. It
    rejects any external evidence in the workspace so the full checker cannot
    accidentally be downgraded by selecting this mode on an existing release.
    """
    root = root.resolve(strict=True)
    workspace = root.parent
    errors: list[str] = []
    present = [name for name in EXTERNAL_PINS
               if (workspace / evidence_location(name)).exists()
               or (workspace / evidence_location(name)).is_symlink()]
    if present:
        errors.append("source-only mode has external evidence; use full verification: "
                      + ", ".join(present))
    document = json.loads(checked_path(workspace, "src/" + DOCUMENT.as_posix()).read_text())
    source_documents = {}
    for name in SOURCE_PINS:
        relative, digest = EXPECTED_PINS[name]
        path = checked_path(workspace, "src/" + relative)
        if sha256(path) != digest:
            errors.append(name + " evidence bytes changed")
        source_documents[name] = json.loads(path.read_text())
    active_ledger = json.loads(checked_path(workspace, "src/" + ACTIVE_LEDGER).read_text())
    historical = source_documents["historical_i677_ledger"]
    errors.extend(validate_source_scope(document, source_documents["predecessor"], historical))
    errors.extend(validate_active_scope(active_ledger))
    return {"successor_id": document.get("successor_id"), "source_identity": SOURCE,
            "formal_leaves": len(historical.get("leaves", [])), "functional_passes_awarded": 0,
            "overall_denominator_frozen": False,
            "evidence_scope": "SOURCE_ONLY_NO_BUILD_ATTESTATION",
            "errors": errors, "valid": not errors}


def check(root: Path) -> dict:
    root = root.resolve(strict=True)
    workspace = root.parent
    document = json.loads(checked_path(workspace, "src/" + DOCUMENT.as_posix()).read_text())
    errors = []
    for name, (relative, digest) in EXPECTED_PINS.items():
        location = evidence_location(name)
        path = checked_path(workspace, ("src/" if relative.startswith("docs/") else "") + location)
        if sha256(path) != digest:
            errors.append(name + " evidence bytes changed")
    ledger = json.loads(checked_path(workspace, "src/" + LEDGER).read_text())
    active_ledger = json.loads(checked_path(workspace, "src/" + ACTIVE_LEDGER).read_text())
    def load(name: str) -> dict:
        relative = evidence_location(name)
        return json.loads(checked_path(workspace, ("src/" if relative.startswith("docs/") else "") + relative).read_text())
    errors.extend(validate(document, load("predecessor"), ledger, load("restore_receipt"),
                           load("software_receipt"), load("cold_audit"),
                           load("artifact_audit"), load("release_manifest")))
    errors.extend(validate_active_scope(active_ledger))
    return {"successor_id": document.get("successor_id"), "source_identity": SOURCE,
            "formal_leaves": len(ledger.get("leaves", [])), "functional_passes_awarded": 0,
            "overall_denominator_frozen": False,
            "evidence_scope": "PINNED_I677_FULL_HISTORICAL_ATTESTATION",
            "errors": errors, "valid": not errors}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--source-only", action="store_true",
                        help="check only immutable source scope in a receipt-free cold capsule")
    args = parser.parse_args()
    try:
        result = check_source_only(args.root) if args.source_only else check(args.root)
    except (OSError, ValueError, TypeError, KeyError, json.JSONDecodeError) as error:
        print("current baseline successor invalid: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result["valid"] else 1


if __name__ == "__main__":
    sys.exit(main())
