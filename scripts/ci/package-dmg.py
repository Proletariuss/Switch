#!/usr/bin/env python3
"""Derive a DMG from an accepted Release App without recompiling or changing it."""
import argparse
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import plistlib
import re
import stat
import subprocess
import tarfile
from urllib.parse import quote

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("release_transport", HERE / "publish-release.py")
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
GitHub, need, digest = transport.GitHub, transport.need, transport.digest
REPO = transport.REPO
LIMIT = transport.LIMIT

def names(version):
    return {"Switch_" + version + ".dmg", "Switch_" + version + ".dmg.sha256", "dmg-provenance.json"}

def check_inventory(rows):
    """Reject archive paths/links that could escape or redirect extraction."""
    expected = {row["path"]: row for row in rows}
    need(len(expected) == len(rows) and "Switch.app" in expected, "duplicate/missing App root")
    for name, row in expected.items():
        path = PurePosixPath(name)
        need(not path.is_absolute() and ".." not in path.parts and str(path) == name
             and path.parts[0] == "Switch.app", "unsafe App inventory path")
        need(row["kind"] in {"file", "directory", "symlink"} and 0 <= row["mode"] <= 0o7777, "unsupported inventory entry")
        for parent in path.parents:
            if str(parent) != ".":
                need(str(parent) in expected and expected[str(parent)]["kind"] == "directory", "non-directory App ancestor")
        if row["kind"] == "symlink":
            target = row["target"]
            resolved = posixpath.normpath(posixpath.join(str(path.parent), target))
            need(not target.startswith("/") and (resolved == "Switch.app" or resolved.startswith("Switch.app/")),
                 "App symlink escapes bundle")
    need(sum(row.get("size", 0) for row in rows) <= LIMIT, "expanded App exceeds limit")
    need(expected["Switch.app"]["kind"] == "directory", "App root is not a directory")
    return expected

def inventory(app):
    rows = []
    for path in [app] + sorted(app.rglob("*")):
        info = path.lstat()
        row = {"path": path.relative_to(app.parent).as_posix(), "mode": stat.S_IMODE(info.st_mode)}
        if stat.S_ISLNK(info.st_mode):
            row.update(kind="symlink", target=os.readlink(path))
        elif stat.S_ISDIR(info.st_mode):
            row.update(kind="directory")
        elif stat.S_ISREG(info.st_mode):
            row.update(kind="file", size=info.st_size, sha256=digest(path.read_bytes()))
        else:
            raise RuntimeError("unsupported extracted App entry")
        rows.append(row)
    return rows

def check_resolved_links(app):
    root = app.resolve(strict=True)
    for path in app.rglob("*"):
        if path.is_symlink():
            try:
                target = path.resolve(strict=True)
                target.relative_to(root)
            except (ValueError, RuntimeError, OSError) as error:
                raise RuntimeError("App symlink is dangling, cyclic or escapes the bundle: " + str(path)) from error

def compare_app(app, manifest):
    check_resolved_links(app)
    expected = check_inventory(manifest["app_inventory"])
    actual = {row["path"]: row for row in inventory(app)}
    need(actual == expected, "mounted/extracted App bytes, permissions or symlinks changed")

def signature(app):
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True, timeout=120)
    details = subprocess.check_output(["codesign", "-dv", "--verbose=4", str(app)], stderr=subprocess.STDOUT, text=True, timeout=120)
    need("Signature=adhoc" in details.splitlines(), "expected unchanged ad-hoc signature")
    print(details, flush=True)

def source_identity(manifest, release, manifest_raw):
    return {"commit": manifest["commit"], "tree": manifest["tree"], "version": manifest["version"],
            "build": manifest["build"], "source_digest": manifest["source_digest"],
            "manifest_sha256": digest(manifest_raw), "app_archive_sha256": manifest["artifact"]["sha256"],
            "app_tree_sha256": manifest["app_tree_sha256"], "release_id": release["id"],
            "tag": release["tag_name"]}

def require_same_assets(expected, actual):
    if expected == actual:
        return
    differences = []
    for name in sorted(set(expected) | set(actual)):
        left, right = expected.get(name), actual.get(name)
        if left != right:
            def identity(raw):
                return "missing" if raw is None else "size=" + str(len(raw)) + ",sha256=" + digest(raw)
            differences.append(name + ": expected(" + identity(left) + ") actual(" + identity(right) + ")")
    raise RuntimeError("accepted Release asset bytes differ: " + "; ".join(differences))

def bind_original_release(api, manifest, original):
    run_id, attempt = int(manifest["run"]["id"]), int(manifest["run"]["attempt"])
    commit = manifest["commit"]
    artifacts = api.pages("/actions/runs/" + str(run_id) + "/artifacts", "artifacts")
    candidate = api.artifact(artifacts, "Switch-candidate-" + commit + "-attempt-" + str(attempt), run_id, commit)
    reports = api.artifact(artifacts, "software-report-" + str(attempt) + "-candidate-and-upload", run_id, commit)
    accepted, _ = transport.validate_payload(candidate, reports, commit, manifest["tree"], manifest["version"],
                                              manifest["build"], run_id, attempt)
    require_same_assets(accepted, original)

def source(api, version, commit):
    need(re.fullmatch(r"[1-9][0-9]*\.[0-9]{2}", version) and version != "3.51", "invalid/protected version")
    need(re.fullmatch(r"[0-9a-f]{40}", commit), "invalid App source")
    tag = api.api("GET", "/git/ref/tags/v" + version)
    need(tag and tag["object"]["sha"] == commit and tag["object"]["type"] == "commit", "App tag/source mismatch")
    release = api.api("GET", "/releases/tags/v" + version)
    need(release and not release["draft"] and not release["prerelease"], "accepted public Release required")
    remote = api.pages("/releases/" + str(release["id"]) + "/assets")
    assets = {item["name"]: item for item in remote}
    need(len(assets) == len(remote), "duplicate Release asset")
    def download(name):
        row = assets[name]
        need(0 < row["size"] <= LIMIT and row["state"] == "uploaded", "invalid Release asset")
        raw = api.api("GET", "/releases/assets/" + str(row["id"]), binary=True)
        need(raw is not None and len(raw) == row["size"], "asset readback size mismatch")
        return raw
    manifest_raw = download("candidate-manifest.json")
    manifest = json.loads(manifest_raw)
    marker = "<!-- switch-release:v1 source=" + commit + " manifest=" + digest(manifest_raw) + " -->"
    need(marker in release.get("body", ""), "original release marker mismatch")
    actual = api.api("GET", "/git/commits/" + commit)
    need(manifest["commit"] == commit and manifest["tree"] == actual["tree"]["sha"]
         and manifest["version"] == version, "original manifest source mismatch")
    app_name = manifest["artifact"]["name"]
    original = {name: download(name) for name in (app_name, "test-reports.tar.gz", "SHA256SUMS")}
    original["candidate-manifest.json"] = manifest_raw
    expected_sums = "".join(digest(original[name]) + "  " + name + "\n"
                            for name in sorted(set(original) - {"SHA256SUMS"})).encode()
    need(original["SHA256SUMS"] == expected_sums, "original release SHA256SUMS mismatch")
    test_files = {}
    with tarfile.open(fileobj=io.BytesIO(original["test-reports.tar.gz"]), mode="r:gz") as archive:
        total = 0
        for entry in archive:
            need(entry.isfile() and entry.name.startswith("test-reports/")
                 and PurePosixPath(entry.name).parent == PurePosixPath("test-reports")
                 and entry.name not in test_files, "unsafe evidence archive")
            total += entry.size
            need(total <= 32 * 1024 * 1024, "evidence limit exceeded")
            test_files[entry.name] = archive.extractfile(entry).read()
    files = {app_name: original[app_name], "candidate-manifest.json": manifest_raw,
             "SHA256SUMS": test_files["test-reports/candidate-SHA256SUMS"]}
    files.update({"test-reports/" + name: test_files["test-reports/" + name] for name in manifest["reports_sha256"]})
    reports = {name: test_files["test-reports/" + name] for name in ("upload-roundtrip.json", "upload-roundtrip.log")}
    check_inventory(manifest["app_inventory"])
    rebuilt, _ = transport.validate_payload(files, reports, commit, manifest["tree"], version, manifest["build"],
                                            int(manifest["run"]["id"]), int(manifest["run"]["attempt"]))
    require_same_assets(rebuilt, original)
    run = api.api("GET", "/actions/runs/" + str(manifest["run"]["id"]))
    need(run["head_sha"] == commit and run["path"] == ".github/workflows/release.yml"
         and run["repository"]["full_name"] == REPO, "original App run mismatch")
    jobs = api.pages("/actions/runs/" + str(run["id"]) + "/jobs", "jobs")
    need(any(job["name"] == "publish" and job["conclusion"] == "success" for job in jobs),
         "original App publication did not succeed")
    bind_original_release(api, manifest, original)
    check_inventory(manifest["app_inventory"])
    need(set(assets) <= set(original) | names(version), "unexpected Release assets")
    return release, manifest, original, assets, source_identity(manifest, release, manifest_raw)

def validate_derivative(files, identity):
    version = identity["version"]
    need(set(files) == names(version), "DMG derivative whitelist mismatch")
    proof = json.loads(files["dmg-provenance.json"])
    name = "Switch_" + version + ".dmg"
    need(proof["schema_version"] == 1 and proof["kind"] == "release-dmg-derivative"
         and proof["source"] == identity, "DMG source provenance mismatch")
    need(proof["dmg"] == {"name": name, "size": len(files[name]), "sha256": digest(files[name])}, "DMG bytes differ")
    need(files[name + ".sha256"] == (digest(files[name]) + "  " + name + "\n").encode(), "DMG checksum mismatch")
    need(proof["validation"] == {"input_archive": True, "mounted_inventory": True, "signature": True,
                                "readonly_mount": True, "app_recompiled": False}, "DMG validation scope mismatch")
    need(proof["acceptance"] == {"signature": "ad-hoc", "notarized": False,
                                "macos27_runtime": "UNVERIFIED", "device_acceptance": "UNVERIFIED"},
         "DMG acceptance mismatch")
    pack = proof["packaging"]
    need(re.fullmatch(r"[0-9a-f]{40}", pack["commit"]) and pack["reusable_workflow"] == ".github/workflows/package-dmg.yml"
         and pack["workflow"] in {".github/workflows/package-dmg.yml", ".github/workflows/release.yml"},
         "DMG controller mismatch")
    need(pack["run_url"] == "https://github.com/" + REPO + "/actions/runs/" + str(pack["run_id"]), "DMG run URL mismatch")
    return proof

def verify_controller(api, proof, require_publish, files=None):
    pack = proof["packaging"]
    run = api.api("GET", "/actions/runs/" + str(pack["run_id"]))
    need(run["head_sha"] == pack["commit"] and run["path"] == pack["workflow"]
         and run["repository"]["full_name"] == REPO and run["event"] == "workflow_dispatch", "DMG run provenance mismatch")
    if pack["commit"] != os.environ["GITHUB_SHA"]:
        for path in ("scripts/ci/package-dmg.py", "scripts/ci/publish-release.py", ".github/workflows/package-dmg.yml"):
            old = api.api("GET", "/contents/" + path + "?ref=" + pack["commit"])
            current = api.api("GET", "/contents/" + path + "?ref=" + os.environ["GITHUB_SHA"])
            need(old is not None and current is not None and old["sha"] == current["sha"],
                 "packaging verifier changed; original controller proof cannot be substituted")
    attempt = int(pack["run_attempt"])
    need(1 <= attempt <= int(run["run_attempt"]), "DMG attempt mismatch")
    jobs = api.pages("/actions/runs/" + str(pack["run_id"]) + "/attempts/" + str(attempt) + "/jobs", "jobs")
    if files is not None:
        artifact_name = ("Switch-dmg-" + proof["source"]["version"] + "-" + proof["source"]["commit"]
                         + "-attempt-" + str(attempt))
        artifacts = api.pages("/actions/runs/" + str(pack["run_id"]) + "/artifacts", "artifacts")
        original = api.artifact(artifacts, artifact_name, int(pack["run_id"]), pack["commit"])
        need(original == files, "DMG files differ from original successful packaging artifact")
    for name, step in [("DMG verify", "Verify mounted DMG against accepted App")] + (
            [("DMG publish", "Publish verified DMG derivative")] if require_publish else []):
        matches = [j for j in jobs if j["name"] == name or j["name"].endswith(" / " + name)]
        need(len(matches) == 1 and matches[0]["conclusion"] == "success"
             and any(s["name"] == step and s["conclusion"] == "success" for s in matches[0]["steps"]),
             "missing successful DMG job: " + name)

def mounted_verify(dmg, manifest, workspace):
    mount = workspace / "mounted"
    mount.mkdir(mode=0o700)
    before = digest(dmg.read_bytes())
    attached = False
    try:
        subprocess.run(["hdiutil", "verify", str(dmg)], check=True, timeout=300)
        attached = True  # Detach is attempted even if attach emits malformed output.
        result = subprocess.check_output(["hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen",
                                          "-plist", "-mountpoint", str(mount), str(dmg)], timeout=300)
        entities = plistlib.loads(result)["system-entities"]
        mounts = [row for row in entities if "mount-point" in row]
        need(len(mounts) == 1 and mounts[0]["mount-point"] == str(mount)
             and re.fullmatch(r"/dev/disk[0-9]+s[0-9]+", mounts[0]["dev-entry"]), "unexpected mounted DMG device")
        need({p.name for p in mount.iterdir()} == {"Switch.app", "Applications"}, "unexpected DMG root entries")
        compare_app(mount / "Switch.app", manifest)
        need((mount / "Applications").is_symlink() and os.readlink(mount / "Applications") == "/Applications",
             "DMG installation link mismatch")
        signature(mount / "Switch.app")
    finally:
        if attached:
            subprocess.run(["hdiutil", "detach", str(mount)], check=True, timeout=120)
    need(digest(dmg.read_bytes()) == before, "DMG bytes changed during readonly verification")

def build(api, version, commit, destination, workspace):
    release, manifest, original, remote, identity = source(api, version, commit)
    destination.mkdir(parents=True, exist_ok=False)
    workspace.mkdir(mode=0o700, parents=True, exist_ok=False)
    present = set(remote) & names(version)
    if present:
        need(present == names(version), "partial DMG publication: rerun the original failed publish job; do not rebuild")
        files = {name: api.api("GET", "/releases/assets/" + str(remote[name]["id"]), binary=True) for name in present}
        proof = validate_derivative(files, identity)
        verify_controller(api, proof, True, files)
        for name, raw in files.items():
            (destination / name).write_bytes(raw)
        mounted_verify(destination / ("Switch_" + version + ".dmg"), manifest, workspace)
        print("PASS: existing immutable DMG independently rechecked; no replacement", flush=True)
        return
    archive = workspace / "accepted-app.tar.gz"
    archive.write_bytes(original[manifest["artifact"]["name"]])
    stage = workspace / "stage"
    stage.mkdir(mode=0o700)
    # All tar entry bytes/types/modes and safe internal symlink ancestry were checked above.
    subprocess.run(["tar", "-xzpf", str(archive), "--no-same-owner", "-C", str(stage)], check=True, timeout=300)
    compare_app(stage / "Switch.app", manifest)
    signature(stage / "Switch.app")
    os.symlink("/Applications", stage / "Applications")
    dmg = destination / ("Switch_" + version + ".dmg")
    subprocess.run(["hdiutil", "create", "-fs", "HFS+", "-format", "UDZO", "-volname", "Switch " + version,
                    "-srcfolder", str(stage), str(dmg)], check=True, timeout=600)
    mounted_verify(dmg, manifest, workspace)
    raw = dmg.read_bytes()
    run = api.api("GET", "/actions/runs/" + os.environ["GITHUB_RUN_ID"])
    need(run["head_sha"] == os.environ["GITHUB_SHA"], "packaging checkout/run SHA mismatch")
    proof = {"schema_version": 1, "kind": "release-dmg-derivative", "source": identity,
             "packaging": {"commit": os.environ["GITHUB_SHA"], "run_id": os.environ["GITHUB_RUN_ID"],
                           "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"], "run_url": run["html_url"],
                           "workflow": run["path"], "reusable_workflow": ".github/workflows/package-dmg.yml"},
             "dmg": {"name": dmg.name, "sha256": digest(raw), "size": len(raw)},
             "validation": {"input_archive": True, "mounted_inventory": True, "signature": True,
                            "readonly_mount": True, "app_recompiled": False},
             "acceptance": {"signature": "ad-hoc", "notarized": False, "macos27_runtime": "UNVERIFIED",
                            "device_acceptance": "UNVERIFIED"}}
    (destination / (dmg.name + ".sha256")).write_text(digest(raw) + "  " + dmg.name + "\n")
    (destination / "dmg-provenance.json").write_text(json.dumps(proof, indent=2) + "\n")
    validate_derivative({p.name: p.read_bytes() for p in destination.iterdir()}, identity)
    print("PASS: accepted App reproduced in readonly DMG; byte/mode/link/signature checks passed; no App compilation", flush=True)

def publish(api, version, commit, directory):
    release, manifest, original, remote, identity = source(api, version, commit)
    files = {p.name: p.read_bytes() for p in directory.iterdir() if p.is_file()}
    proof = validate_derivative(files, identity)
    # The build job's successful mount verification is required, not only JSON booleans.
    verify_controller(api, proof, False, files)
    present = set(remote) & names(version)
    if present != names(version):
        need(proof["packaging"]["commit"] == os.environ["GITHUB_SHA"]
             and str(proof["packaging"]["run_id"]) == os.environ["GITHUB_RUN_ID"],
             "partial upload must resume from its original packaging run")
    original_ids = {name: remote[name]["id"] for name in original}
    checked = {}
    for name, raw in files.items():
        row = remote.get(name)
        if row is None:
            row = api.api("POST", "/releases/" + str(release["id"]) + "/assets?name=" + quote(name, safe=""),
                          raw, binary=True, upload=True)
        need(row["size"] == len(raw) and row["state"] == "uploaded", "DMG asset size/state mismatch")
        transport.same_asset(api.api("GET", "/releases/assets/" + str(row["id"]), binary=True), raw)
        checked[name] = row["id"]
    final = api.pages("/releases/" + str(release["id"]) + "/assets")
    need({row["name"]: row["id"] for row in final} == original_ids | checked and len(final) == 7,
         "Release asset whitelist/IDs changed")
    for name, raw in original.items():
        transport.same_asset(api.api("GET", "/releases/assets/" + str(original_ids[name]), binary=True), raw)
    tag = api.api("GET", "/git/ref/tags/v" + version)
    need(tag["object"]["type"] == "commit" and tag["object"]["sha"] == commit, "App tag changed")
    print("Published verified DMG derivative: " + release["html_url"], flush=True)
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
        summary.write("DMG: " + release["html_url"] + "\n\nApp source: " + commit
                      + "\nPackaging source: " + proof["packaging"]["commit"] + "\n")

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["build", "publish"])
    parser.add_argument("--version", required=True)
    parser.add_argument("--app-sha", required=True)
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--workspace", type=Path)
    args = parser.parse_args()
    need(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("GITHUB_REPOSITORY") == REPO
         and os.environ.get("GITHUB_EVENT_NAME") == "workflow_dispatch", "GitHub explicit dispatch required")
    api = GitHub()
    if args.mode == "build":
        need(args.workspace is not None, "build workspace required")
        build(api, args.version, args.app_sha, args.directory.resolve(), args.workspace.resolve())
    else:
        publish(api, args.version, args.app_sha, args.directory.resolve())

if __name__ == "__main__":
    main()
