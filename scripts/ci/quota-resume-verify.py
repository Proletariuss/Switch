#!/usr/bin/env python3
"""Reproduce pinned preparation provenance; this is NOT semantic equivalence proof."""
import json
import os
from pathlib import Path
import subprocess
import sys

from quota_resume_common import (AUTH_BLOB, AUTH_PATH, AUTH_SHA, BRANCH, GitHub,
    Refusal, emit, iso, member, require, sha)


def main():
    api = GitHub()
    require(os.environ.get("AUTH_SHA", AUTH_SHA) == AUTH_SHA, "authorization SHA override refused")
    prepared = os.environ["PREPARED_SHA"]
    run_id = int(os.environ["PREPARE_RUN_ID"])
    attempt = int(os.environ["PREPARE_ATTEMPT"])
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
    source = Path(os.environ["SOURCE_DIR"]).resolve()
    work = workspace / "work"
    require(source.is_relative_to(workspace) and source != workspace and not work.is_relative_to(source),
            "source/output roots are not isolated")
    work.mkdir(exist_ok=True)
    proof_path = work / "provenance-proof.json"
    require(not proof_path.exists(), "proof output already exists")
    pr = api.pinned_pr()
    require(pr["head"]["sha"] == prepared, "candidate moved before independent reproduction")
    candidate = api.prepared_commit(prepared)
    run = api.quality_run(run_id, AUTH_SHA)
    require(run["status"] == "completed" and run["conclusion"] == "success"
            and run["run_attempt"] == attempt, "preparation run is not a successful exact attempt")
    jobs = api.jobs(run_id, attempt)
    require(any(j["name"].endswith("prepare-reviewed / prepare") and j["conclusion"] == "success" for j in jobs),
            "prepared job did not succeed")
    artifact = api.artifact(run_id, "reviewed-preparation-" + AUTH_SHA + "-" + str(attempt))
    archive, artifact_sha = api.zip(artifact)
    original_report_bytes = member(archive, "reviewed-preparation.json")
    original_patch = member(archive, "formatting.patch")
    original_report = json.loads(original_report_bytes)
    require(original_report["expected_commit"] == AUTH_SHA and original_report["prepared_commit"] == prepared
            and original_report["authorization_blob"] == AUTH_BLOB
            and original_report["functional_acceptance_awarded"] is False, "original preparation report binding mismatch")
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=source, text=True).strip()
    require(git("rev-parse", "HEAD") == AUTH_SHA and not git("status", "--porcelain"), "source checkout is not clean pinned authorization")
    require(git("rev-parse", "HEAD:" + AUTH_PATH) == AUTH_BLOB, "checkout authorization differs")
    evidence = workspace / "prepared-evidence"
    require(not evidence.exists(), "reproduction output is not a fresh directory")
    environment = os.environ.copy()
    environment.update(EXPECTED_COMMIT=AUTH_SHA, GITHUB_REF="refs/heads/" + BRANCH,
                       PYTHONDONTWRITEBYTECODE="1", GITHUB_OUTPUT=str(work / "reproduction-outputs.txt"))
    # Only the immutable root-reviewed authorization tree is executed, with a read-only token.
    subprocess.run([sys.executable, "scripts/ci/prepare-reviewed.py"], cwd=source, env=environment, check=True, timeout=600)
    reproduced = json.loads((evidence / "reviewed-preparation.json").read_text())
    recreated = git("rev-parse", "HEAD")
    require(git("rev-list", "--parents", "-n", "1", "HEAD").split() == [recreated, AUTH_SHA], "reproduced commit parent mismatch")
    require(reproduced["prepared_commit"] == recreated, "reproduced report commit mismatch")
    require(git("rev-parse", "HEAD^{tree}") == candidate["tree"]["sha"], "complete prepared Git tree did not reproduce")
    require(git("rev-parse", "HEAD:" + AUTH_PATH) == AUTH_BLOB, "reproduction changed authorization")
    stable_original = {key: value for key, value in original_report.items() if key != "prepared_commit"}
    stable_reproduced = {key: value for key, value in reproduced.items() if key != "prepared_commit"}
    require(stable_original == stable_reproduced, "preparation report fields did not reproduce")
    repeated_patch = (evidence / "formatting.patch").read_bytes()
    require(repeated_patch == original_patch, "exact formatting patch did not reproduce")
    require(api.pinned_pr()["head"]["sha"] == prepared, "candidate moved during independent reproduction")
    proof = {
        "schema_version": 1, "kind": "pinned-preparation-provenance-reproduction",
        "provenance_reproduced": True, "semantic_equivalence_proven": False,
        "product_acceptance_complete": False,
        "authorization_sha": AUTH_SHA, "authorization_blob": AUTH_BLOB,
        "prepared_sha": prepared, "prepared_tree": candidate["tree"]["sha"],
        "prepare_run_id": run_id, "prepare_attempt": attempt,
        "prepare_artifact_id": artifact["id"], "prepare_artifact_zip_sha256": artifact_sha,
        "original_report_sha256": sha(original_report_bytes), "formatting_patch_sha256": sha(original_patch),
        "format_token_changes": original_report.get("format_token_changes", []),
        "reproduced_commit": recreated,
        "coordinator_run_id": int(os.environ["GITHUB_RUN_ID"]),
        "coordinator_attempt": int(os.environ["GITHUB_RUN_ATTEMPT"]),
        "verified_at": iso(),
        "limits": "Pinned formatter and reviewed metadata provenance only; no semantic, test, device, carrier or merge acceptance."
    }
    proof_path.write_text(json.dumps(proof, indent=2) + "\n")
    emit(provenance_reproduced=True, prepared_sha=prepared, proof_path="work/provenance-proof.json")

if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print("PROVENANCE REPRODUCTION FAILED: " + str(error)[:500], file=sys.stderr)
        raise SystemExit(1)
