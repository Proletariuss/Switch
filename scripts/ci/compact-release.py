#!/usr/bin/env python3
"""Archive audit attachments, then retain only the accepted DMG and its checksum."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re

spec = importlib.util.spec_from_file_location("package_dmg", Path(__file__).with_name("package-dmg.py"))
dmg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dmg)
GitHub, need, digest, REPO = dmg.GitHub, dmg.need, dmg.digest, dmg.REPO

def retained_names(version):
    return {"Switch_" + version + ".dmg", "Switch_" + version + ".dmg.sha256"}

def audit_names(version, commit):
    return {"Switch-" + version + "-candidate-" + commit + "-macOS27-arm64.app.tar.gz",
            "candidate-manifest.json", "test-reports.tar.gz", "SHA256SUMS", "dmg-provenance.json"}

def asset_map(rows):
    result = {row["name"]: row for row in rows}
    need(len(result) == len(rows), "duplicate Release names")
    return result

def read_asset(api, row):
    need(row["state"] == "uploaded" and 0 < row["size"] <= dmg.LIMIT, "invalid Release asset")
    raw = api.api("GET", "/releases/assets/" + str(row["id"]), binary=True)
    need(raw is not None and len(raw) == row["size"] and row["digest"] == "sha256:" + digest(raw),
         "Release asset raw readback mismatch")
    return raw

def job_succeeded(jobs, name, step):
    matches = [job for job in jobs if job["name"] == name or job["name"].endswith(" / " + name)]
    need(len(matches) == 1 and matches[0]["conclusion"] == "success"
         and any(s["name"] == step and s["conclusion"] == "success" for s in matches[0]["steps"]),
         "missing successful job: " + name)

def original_dmg(api, version, commit, run_id, requested_attempt):
    need(re.fullmatch(r"[1-9][0-9]*\.[0-9]{2}", version) and version != "3.51", "invalid/protected version")
    need(re.fullmatch(r"[0-9a-f]{40}", commit) and run_id > 0 and 0 <= requested_attempt <= 50, "invalid packaging input")
    artifacts = api.pages("/actions/runs/" + str(run_id) + "/artifacts", "artifacts")
    prefix = "Switch-dmg-" + version + "-" + commit + "-attempt-"
    candidates = [row for row in artifacts if row["name"].startswith(prefix)
                  and re.fullmatch(r"[1-9][0-9]*", row["name"][len(prefix):])]
    if requested_attempt:
        candidates = [row for row in candidates if row["name"] == prefix + str(requested_attempt)]
    need(1 <= len(candidates) <= 5, "packaging artifact missing or ambiguous")
    run = api.api("GET", "/actions/runs/" + str(run_id))
    need(run["repository"]["full_name"] == REPO, "packaging repository mismatch")
    originals = [api.artifact(artifacts, row["name"], run_id, run["head_sha"]) for row in candidates]
    need(all(files == originals[0] for files in originals), "different packaging attempts require an explicit original attempt")
    files = originals[0]
    proof = json.loads(files["dmg-provenance.json"])
    identity = proof["source"]
    need(identity["commit"] == commit and identity["version"] == version
         and identity["tag"] == "v" + version and str(proof["packaging"]["run_id"]) == str(run_id),
         "packaging/App identity mismatch")
    if requested_attempt:
        need(int(proof["packaging"]["run_attempt"]) == requested_attempt, "not the requested original proof attempt")
    dmg.validate_derivative(files, identity)
    # Binds raw bytes to the successful mount job, original artifact and unchanged verifier code.
    dmg.verify_controller(api, proof, False, files)
    # A failed publish-only attempt can later succeed without rebuilding the nondeterministic DMG.
    original_attempt = int(proof["packaging"]["run_attempt"])
    need(original_attempt <= int(run["run_attempt"]) <= 50, "packaging attempt bound exceeded")
    found = False
    for attempt in range(int(run["run_attempt"]), original_attempt - 1, -1):
        jobs = api.pages("/actions/runs/" + str(run_id) + "/attempts/" + str(attempt) + "/jobs", "jobs")
        relevant = [job for job in jobs if (job["name"] == "DMG publish" or job["name"].endswith(" / DMG publish")) and job.get("steps")]
        if relevant:
            job_succeeded(relevant, "DMG publish", "Publish verified DMG derivative")
            found = True
            break
    need(found, "no successful DMG publication")
    return files, proof

def context(api, version, commit, proof):
    tag = api.api("GET", "/git/ref/tags/v" + version)
    need(tag and tag["object"]["type"] == "commit" and tag["object"]["sha"] == commit, "App tag changed")
    source = api.api("GET", "/git/commits/" + commit)
    need(source["tree"]["sha"] == proof["source"]["tree"], "App source tree mismatch")
    release = api.api("GET", "/releases/tags/v" + version)
    need(release and not release["draft"] and release["id"] == proof["source"]["release_id"], "Release identity mismatch")
    marker = "<!-- switch-release:v1 source=" + commit + " manifest=" + proof["source"]["manifest_sha256"] + " -->"
    need(marker in release.get("body", ""), "original Release marker missing")
    rows = asset_map(api.pages("/releases/" + str(release["id"]) + "/assets"))
    need(retained_names(version) <= set(rows) <= retained_names(version) | audit_names(version, commit),
         "unknown or missing Release attachment")
    return release, rows

def record(row, raw):
    return {"name": row["name"], "id": row["id"], "size": len(raw), "sha256": digest(raw)}

def matching(row, expected):
    return (row["id"] == expected["id"] and row["size"] == expected["size"]
            and row["state"] == "uploaded" and row["digest"] == "sha256:" + expected["sha256"])

def verify_retained(api, rows, files, version, expected=None):
    result = []
    for name in sorted(retained_names(version)):
        raw = read_asset(api, rows[name])
        need(raw == files[name], "retained DMG/checksum differs from successful packaging run")
        current = record(rows[name], raw)
        if expected is not None:
            need(current == expected[name], "retained asset ID or bytes changed")
        result.append(current)
    return result

def prepare(api, args):
    files, proof = original_dmg(api, args.version, args.app_sha, args.packaging_run_id, args.packaging_attempt)
    release, rows = context(api, args.version, args.app_sha, proof)
    keep = verify_retained(api, rows, files, args.version)
    args.directory.mkdir(parents=True, exist_ok=False)
    audit = args.directory / "audit"
    audit.mkdir()
    removed = []
    for name in sorted(audit_names(args.version, args.app_sha) & set(rows)):
        raw = read_asset(api, rows[name])
        if name == "dmg-provenance.json":
            need(raw == files[name], "Release provenance differs from successful packaging")
        if name == "candidate-manifest.json":
            need(digest(raw) == proof["source"]["manifest_sha256"], "original App manifest changed")
        if name.endswith(".app.tar.gz"):
            need(digest(raw) == proof["source"]["app_archive_sha256"], "original App archive changed")
        (audit / name).write_bytes(raw)
        removed.append(record(rows[name], raw))
    (args.directory / "packaging-proof.json").write_bytes(files["dmg-provenance.json"])
    plan = {"schema_version": 1, "kind": "compact-release-plan", "version": args.version, "app_sha": args.app_sha,
            "release_id": release["id"], "packaging_run_id": int(proof["packaging"]["run_id"]),
            "packaging_attempt": int(proof["packaging"]["run_attempt"]),
            "controller": os.environ["GITHUB_SHA"], "run_id": os.environ["GITHUB_RUN_ID"],
            "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"], "retain": keep, "remove": removed}
    (args.directory / "cleanup-plan.json").write_text(json.dumps(plan, indent=2) + "\n")
    print("PASS: original DMG bytes verified; " + str(len(removed)) + " exact audit attachments staged for preservation", flush=True)

def remove_planned(api, release_id, version, commit, retained, removed):
    rows = asset_map(api.pages("/releases/" + str(release_id) + "/assets"))
    expected_keep = {row["name"]: row for row in retained}
    expected_remove = {row["name"]: row for row in removed}
    need(set(expected_keep) == retained_names(version) and len(expected_keep) == len(retained), "invalid retained plan")
    need(set(expected_remove) <= audit_names(version, commit) and len(expected_remove) == len(removed), "invalid deletion plan")
    need(set(rows) <= set(expected_keep) | set(expected_remove) and set(expected_keep) <= set(rows),
         "unplanned or missing Release asset")
    for name, expected in expected_keep.items():
        need(matching(rows[name], expected), "retained asset identity changed")
    # Precheck every remaining target before the first deletion. Missing targets are already complete.
    for name, expected in expected_remove.items():
        if name in rows:
            need(matching(rows[name], expected), "same-name audit asset was replaced")
    for name, expected in expected_remove.items():
        if name in rows:
            api.api("DELETE", "/releases/assets/" + str(expected["id"]))
            print("Deleted authorized audit asset: " + name + " id=" + str(expected["id"]), flush=True)

def prune(api, args):
    plan = json.loads((args.directory / "cleanup-plan.json").read_bytes())
    need(plan["schema_version"] == 1 and plan["kind"] == "compact-release-plan"
         and plan["controller"] == os.environ["GITHUB_SHA"] and str(plan["run_id"]) == os.environ["GITHUB_RUN_ID"]
         and plan["version"] == args.version and plan["app_sha"] == args.app_sha, "cleanup plan/source mismatch")
    artifact_name = "Switch-release-audit-" + args.version + "-" + plan["run_id"] + "-attempt-" + plan["run_attempt"]
    artifacts = api.pages("/actions/runs/" + plan["run_id"] + "/artifacts", "artifacts")
    snapshot = api.artifact(artifacts, artifact_name, int(plan["run_id"]), plan["controller"])
    local = {p.relative_to(args.directory).as_posix(): p.read_bytes() for p in args.directory.rglob("*") if p.is_file()}
    need(snapshot == local, "archived audit snapshot differs from cleanup inputs")
    jobs = api.pages("/actions/runs/" + plan["run_id"] + "/attempts/" + plan["run_attempt"] + "/jobs", "jobs")
    job_succeeded(jobs, "Release compact verify", "Verify retained DMG and preserve audit files")
    files, proof = original_dmg(api, args.version, args.app_sha, plan["packaging_run_id"], plan["packaging_attempt"])
    need(local["packaging-proof.json"] == files["dmg-provenance.json"], "cleanup proof changed")
    release, rows = context(api, args.version, args.app_sha, proof)
    need(release["id"] == plan["release_id"], "cleanup Release changed")
    keep = {row["name"]: row for row in plan["retain"]}
    verify_retained(api, rows, files, args.version, keep)
    for row in plan["remove"]:
        raw = local["audit/" + row["name"]]
        need(len(raw) == row["size"] and digest(raw) == row["sha256"], "preserved audit bytes changed")
    remove_planned(api, release["id"], args.version, args.app_sha, plan["retain"], plan["remove"])
    _, final = context(api, args.version, args.app_sha, proof)
    need(set(final) == retained_names(args.version), "Release compaction incomplete")
    verify_retained(api, final, files, args.version, keep)
    # Preserve the immutable source marker and explain the new download layout.
    snapshot_row = next(row for row in artifacts if row["name"] == artifact_name)
    start, end = "<!-- switch-compact:v1 -->", "<!-- /switch-compact:v1 -->"
    block = (start + "\n### Downloads und Prüfnachweise\n\n"
             + "Als hochgeladene Downloads bleiben nur `Switch_" + args.version + ".dmg` und die zugehörige "
             + "`.dmg.sha256`-Datei. Die in diesem Bereinigungsschritt vorhandenen Audit-Anlagen wurden vor dem Löschen "
             + "unverändert im Actions-Artefakt `" + artifact_name + "` gesichert: "
             + "https://github.com/" + REPO + "/actions/runs/" + plan["run_id"] + ". "
             + "Das von GitHub gemeldete Ablaufdatum dieses Zwischenartefakts ist "
             + snapshot_row["expires_at"] + "; es ist kein dauerhaftes Archiv. "
             + "Früher im Text beschriebene Manifest-, Bericht- und App-Tar-Anlagen gehören damit zu den Audit-Daten, "
             + "nicht mehr zur Download-Liste. Automatische Quellcodearchive bleiben unverändert.\n" + end)
    latest = api.api("GET", "/releases/" + str(release["id"]))
    body = latest.get("body", "")
    marker = "<!-- switch-release:v1 source=" + args.app_sha + " manifest=" + proof["source"]["manifest_sha256"] + " -->"
    need(marker in body, "Release source marker changed")
    if plan["remove"] or start not in body:
        need(body.count(start) == body.count(end) and body.count(start) <= 1, "ambiguous compaction note")
        if start in body:
            first, last = body.index(start), body.index(end) + len(end)
            body = body[:first] + block + body[last:]
        else:
            body += "\n\n" + block
        updated = api.api("PATCH", "/releases/" + str(release["id"]), {"body": body})
        need(updated["body"] == body and marker in updated["body"], "Release compaction note mismatch")
    print("PASS: Release retains only the original DMG and checksum; IDs, bytes and tag unchanged", flush=True)
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
        summary.write("Compacted: " + release["html_url"] + "\nAudit snapshot: " + artifact_name
                      + "\nOnly DMG and its checksum remain as uploaded assets. Automatic source archives are unchanged.\n")

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["prepare", "prune"])
    parser.add_argument("--version", required=True)
    parser.add_argument("--app-sha", required=True)
    parser.add_argument("--packaging-run-id", type=int, required=True)
    parser.add_argument("--packaging-attempt", type=int, default=0)
    parser.add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    need(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("GITHUB_REPOSITORY") == REPO
         and os.environ.get("GITHUB_EVENT_NAME") == "workflow_dispatch", "explicit GitHub dispatch required")
    if args.mode == "prepare":
        prepare(GitHub(), args)
    else:
        prune(GitHub(), args)

if __name__ == "__main__":
    main()
