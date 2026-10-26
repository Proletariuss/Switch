#!/usr/bin/env python3
"""Bind an App candidate to its source and actual gate reports; no product attestation."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile

SOURCE = Path(__file__).resolve().parents[2]
WORKSPACE = SOURCE.parent

def output(*command):
    return subprocess.check_output(command, cwd=SOURCE, text=True).strip()

def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()

def inventory(app):
    records = []
    for path in [app] + sorted(app.rglob("*")):
        info = path.lstat()
        row = {"path": path.relative_to(app.parent).as_posix(), "mode": stat.S_IMODE(info.st_mode)}
        if path.is_symlink():
            row.update(kind="symlink", target=os.readlink(path))
        elif path.is_dir():
            row.update(kind="directory")
        elif path.is_file():
            row.update(kind="file", size=info.st_size, sha256=sha(path))
        else:
            raise SystemExit("unsupported candidate file type")
        records.append(row)
    return records

if len(sys.argv) == 3 and sys.argv[1] == "verify":
    root = Path(sys.argv[2])
    manifest = json.loads((root / "candidate-manifest.json").read_text())
    archive = root / manifest["artifact"]["name"]
    if sha(archive) != manifest["artifact"]["sha256"]:
        raise SystemExit("uploaded candidate hash mismatch")
    expected = {row["path"]: row for row in manifest["app_inventory"]}
    seen = set()
    with tarfile.open(archive, "r:gz") as bundle:
        for member in bundle:
            name = member.name.rstrip("/")
            row = expected.get(name)
            if row is None or name in seen or member.mode != row["mode"]:
                raise SystemExit("archive inventory/mode mismatch: " + name)
            seen.add(name)
            if row["kind"] == "file":
                if not member.isfile() or member.size != row["size"]:
                    raise SystemExit("archive file type/size mismatch")
                digest = hashlib.sha256()
                with bundle.extractfile(member) as stream:
                    for block in iter(lambda: stream.read(1024 * 1024), b""):
                        digest.update(block)
                if digest.hexdigest() != row["sha256"]:
                    raise SystemExit("archive file content mismatch")
            elif row["kind"] == "directory" and not member.isdir():
                raise SystemExit("archive directory mismatch")
            elif row["kind"] == "symlink" and (not member.issym() or member.linkname != row["target"]):
                raise SystemExit("archive symlink mismatch")
    if seen != set(expected):
        raise SystemExit("archive missing entries")
    for name, digest in manifest["reports_sha256"].items():
        if sha(root / "test-reports" / name) != digest:
            raise SystemExit("uploaded report hash mismatch")
    print("PASS: uploaded bytes, App inventory, permissions and gate reports match candidate manifest")
    raise SystemExit(0)
if len(sys.argv) != 1:
    raise SystemExit("usage: package-candidate.py [verify DIRECTORY]")
commit = output("git", "rev-parse", "HEAD")
if commit != os.environ["EXPECTED_COMMIT"]:
    raise SystemExit("candidate checkout does not match requested commit")
source_digest = output("python3", "scripts/source-identity.py")
if source_digest != os.environ["EXPECTED_SOURCE_DIGEST"]:
    raise SystemExit("source changed during candidate build")
reports = WORKSPACE / "reports"
required = {"portable", "rust", "device-sms-send", "module-network-recovery", "api-request-policy", "sms-forwarding-client", "release-initialization", "app-build", "bundle-audit", "signature"}
for name in required:
    report = json.loads((reports / (name + ".json")).read_text())
    if report["status"] != "passed" or report["exit_code"] != 0 or report["commit"] != commit or report["run_id"] != os.environ["GITHUB_RUN_ID"] or report["run_attempt"] != os.environ["GITHUB_RUN_ATTEMPT"]:
        raise SystemExit("missing, failed or mismatched candidate gate: " + name)
app = WORKSPACE / "dist/Switch.app"
records = inventory(app)
version = (SOURCE / "VERSION").read_text().strip()
delivery = WORKSPACE / "candidate"
delivery.mkdir(exist_ok=False)
archive = delivery / f"Switch-{version}-candidate-{commit}-macOS27-arm64.app.tar.gz"
# BSD tar retains the audited modes/symlinks; direct upload of .app loses modes.
subprocess.run(["tar", "-czf", str(archive), "-C", str(app.parent), "Switch.app"], check=True)
import shutil
shutil.copytree(reports, delivery / "test-reports")
manifest = {
    "schema_version": 1, "kind": "ci-app-candidate", "version": version,
    "build": (SOURCE / "BUILD_NUMBER").read_text().strip(),
    "commit": commit, "tree": output("git", "rev-parse", "HEAD^{tree}"), "source_digest": source_digest,
    "run": {"id": os.environ["GITHUB_RUN_ID"], "attempt": os.environ["GITHUB_RUN_ATTEMPT"], "url": os.environ["GITHUB_SERVER_URL"] + "/" + os.environ["GITHUB_REPOSITORY"] + "/actions/runs/" + os.environ["GITHUB_RUN_ID"]},
    "toolchain": {"rust": output("rustc", "+1.98.0", "--version", "--verbose"), "xcode": output("xcodebuild", "-version"), "swift": output("xcrun", "swiftc", "--version"), "sdk": output("xcrun", "--show-sdk-version"), "host": output("sw_vers"), "architecture": output("uname", "-m")},
    "acceptance": {"scope": "github-hosted-offline-software-gates-and-bundle-audit", "product_acceptance_complete": False, "device_acceptance": "UNVERIFIED", "macos27_runtime": "UNVERIFIED", "signature": "ad-hoc", "notarized": False, "original_v3_51_release": False},
    "artifact": {"name": archive.name, "sha256": sha(archive), "size": archive.stat().st_size},
    "app_inventory": records,
    "app_tree_sha256": hashlib.sha256(json.dumps(records, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
    "reports_sha256": {p.name: sha(p) for p in sorted(reports.iterdir()) if p.is_file()},
}
(delivery / "candidate-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
(delivery / "SHA256SUMS").write_text("".join(sha(p) + "  " + p.name + "\n" for p in [archive, delivery / "candidate-manifest.json"]))
print(archive)
