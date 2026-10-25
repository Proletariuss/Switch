#!/usr/bin/env python3
"""Opt-in copied-bundle upgrade/rollback on one synthetic data directory.

Preserves legacy runtime footprints instead of copying only business stores.
The mandatory Iteration 546 sandbox forbids hardware and external connections.
A data-compatible run with nonzero shutdowns is explicitly failed acceptance.
"""
import argparse
import http.client
import importlib.util
import json
import hashlib
import os
from pathlib import Path
import plistlib
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time

HELPER = Path(__file__).with_name("check-p6-data-roundtrip.py").resolve()
SPEC = importlib.util.spec_from_file_location("p546_roundtrip", HELPER)
BASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BASE)
BACKEND = Path("Contents/Resources/backend")
LIFECYCLE_NAMES = (
    ".legacy-execution-pending-v1.json", ".switchd-lifetime.lock",
    ".call-mode-pristine-v1", "call-mode-operation.lock", ".module-adb-interface.lock",
    "module-backups", "voice-runtime", "call-mode-restore.active",
    "call-mode-restore-archive", "voice-control", "module-recovery-v1", "backup-lifecycle-v1",
)
MAX_EVIDENCE_BYTES = 64 * 1024 * 1024


def bundle_inventory(app):
    inventory = {}
    for path in sorted(app.rglob("*")):
        name = str(path.relative_to(app))
        if path.is_symlink():
            # Never let a copied bundle silently load an external user payload.
            BASE.require(path.resolve().is_relative_to(app.resolve()), "bundle symlink escapes app: " + name)
            inventory[name] = {"symlink": os.readlink(path)}
        elif path.is_file():
            inventory[name] = {"sha256": BASE.digest(path), "mode": path.stat().st_mode & 0o777}
    return inventory


def copy_bundle(source, target, expected_version):
    source = source.resolve(strict=True)
    BASE.require(source.is_dir() and source.suffix == ".app", "expected .app directory")
    info = plistlib.loads((source / "Contents/Info.plist").read_bytes())
    BASE.require(info["CFBundleShortVersionString"] == expected_version, "unexpected bundle version")
    original = bundle_inventory(source)
    shutil.copytree(source, target, symlinks=True)
    copied = bundle_inventory(target)
    BASE.require(original == copied, "bundle copy changed inputs")
    result = subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(target)],
                            capture_output=True, text=True, timeout=30)
    BASE.require(result.returncode == 0, "copied bundle signature rejected: " + result.stderr)
    return {"source": str(source), "copy": str(target), "version": info["CFBundleShortVersionString"],
            "build": info["CFBundleVersion"], "files": copied, "signature_verified": True}


def lifecycle_footprints(root):
    """Snapshot protocol namespaces without following links or reading FIFOs."""
    result = {}

    def capture(directory, name, relative):
        try:
            metadata = os.stat(name, dir_fd=directory, follow_symlinks=False)
        except FileNotFoundError:
            return
        kind = ("regular" if stat.S_ISREG(metadata.st_mode) else
                "directory" if stat.S_ISDIR(metadata.st_mode) else
                "symlink" if stat.S_ISLNK(metadata.st_mode) else
                "fifo" if stat.S_ISFIFO(metadata.st_mode) else
                "socket" if stat.S_ISSOCK(metadata.st_mode) else "special")
        stamp = lambda value: (value.st_dev, value.st_ino, value.st_mode, value.st_nlink,
                               value.st_uid, value.st_gid, value.st_size,
                               value.st_mtime_ns, value.st_ctime_ns)
        item = {"type": kind, "size": metadata.st_size, "mode": stat.S_IMODE(metadata.st_mode),
                "device": metadata.st_dev, "inode": metadata.st_ino, "links": metadata.st_nlink,
                "uid": metadata.st_uid, "gid": metadata.st_gid,
                "mtime_ns": metadata.st_mtime_ns, "ctime_ns": metadata.st_ctime_ns}
        result[relative] = item
        if kind in ("regular", "directory"):
            flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
            if kind == "directory":
                flags |= os.O_DIRECTORY
            descriptor = os.open(name, flags, dir_fd=directory)
            try:
                BASE.require(stamp(os.fstat(descriptor)) == stamp(metadata), "evidence changed before read")
                if kind == "directory":
                    for child in sorted(os.listdir(descriptor)):
                        capture(descriptor, child, relative + "/" + child)
                else:
                    BASE.require(metadata.st_size <= MAX_EVIDENCE_BYTES, "evidence exceeds snapshot bound")
                    digest = hashlib.sha256()
                    count = 0
                    while block := os.read(descriptor, 1024 * 1024):
                        count += len(block)
                        BASE.require(count <= MAX_EVIDENCE_BYTES, "evidence grew beyond snapshot bound")
                        digest.update(block)
                    item["sha256"] = digest.hexdigest()
                BASE.require(stamp(os.fstat(descriptor)) == stamp(metadata), "evidence changed during read")
            finally:
                os.close(descriptor)
        elif kind == "symlink":
            item["target"] = os.readlink(name, dir_fd=directory)
        BASE.require(stamp(os.stat(name, dir_fd=directory, follow_symlinks=False)) == stamp(metadata),
                     "evidence namespace changed during read")

    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for name in LIFECYCLE_NAMES:
            capture(descriptor, name, name)
    finally:
        os.close(descriptor)
    return result


def recovery_evidence_preserved(before, after):
    # The first contemporary daemon may create its own lifetime lock. This is
    # recorded, never treated as historical cleanup or a pristine receipt.
    added = set(after) - set(before)
    if added:
        if added != {".switchd-lifetime.lock"}:
            return False
        lock = after[".switchd-lifetime.lock"]
        if not (lock["type"] == "regular" and lock["size"] == 0 and
                lock["links"] == 1 and lock["mode"] == 0o600):
            return False
    return all(after.get(name) == entry for name, entry in before.items())


def shutdown_accepted(stage, native=False):
    return (stage.get("exit_code") == 0 and
            stage.get("owned_group_exit_confirmed") is True and
            stage.get("shutdown_timeout") is False and
            stage.get("forced_owned_group_cleanup") is False and
            stage.get("sockets_remaining") == [] and
            (not native or stage.get("native_shutdown_receipt") is True))


def quarantine_observation(daemon):
    runtime = daemon.request("GET", "/api/v2/runtime")
    status = runtime.get("module_recovery", {})
    BASE.require(status.get("state") == "legacy_recovery_required", "module quarantine missing")
    BASE.require(status.get("historical_recovery_confirmed") is False, "history falsely restored")
    health = daemon.request("GET", "/api/health")
    BASE.require(health.get("ok") is True and health.get("esim_available") is False and
                 health.get("discovery_error") == "legacy_recovery_required", "health quarantine projection")
    call_mode = daemon.request("GET", "/api/call-mode/status")
    BASE.require(call_mode.get("state") == "legacy_recovery_required", "ADB getter escaped quarantine")
    devices = daemon.request("GET", "/api/sms/devices")
    BASE.require(any(d.get("id") == "reader" for d in devices["devices"]), "reader projection disappeared")
    connection = http.client.HTTPConnection("localhost", timeout=5)
    stream = socket.socket(socket.AF_UNIX)
    stream.settimeout(5)
    stream.connect(str(daemon.root / "s"))
    connection.sock = stream
    try:
        connection.request("POST", "/api/at", json.dumps({"command": "AT"}),
                           {"x-proletarius-app-token": BASE.TOKEN, "content-type": "application/json"})
        response = connection.getresponse()
        rejected = json.loads(response.read())
        BASE.require(response.status == 409 and rejected.get("code") == "legacy_recovery_required" and
                     rejected.get("submitted") is False, "module physical action not rejected before submit")
    finally:
        connection.close()
    return {"runtime": status, "health": health, "call_mode": call_mode,
            "sms_devices": devices, "physical_action_rejection": rejected}


def exercise(root, report_root, label, binary, child, deleted, sender=None, expect_quarantine=False):
    profile = root / "hardware-denied.sb"
    profile.write_text(BASE.sandbox_profile(root))
    with socket.socket(socket.AF_UNIX) as blocked:
        blocked.bind(str(report_root / (label + "-blocked")))
        blocked.listen(4)
        BASE.validate_sandbox(root, profile, report_root / (label + "-blocked"))
    observation = {"name": label, "sandbox_canary_and_child_inheritance": "passed",
                   "input_sha256": BASE.hashes(root),
                   "lifecycle_before": lifecycle_footprints(root)}
    daemon = BASE.Daemon(binary, root, child)
    try:
        observation["runtime"] = daemon.ready()
        if expect_quarantine:
            observation["module_quarantine"] = quarantine_observation(daemon)
        observation["before"] = BASE.inspect(daemon, deleted)
        if sender:
            request = {"sender": sender}
            if child is None:
                request["client_request_id"] = "p6-inplace-" + label + "-" + sender
            result = daemon.request("POST", "/api/v2/sms-archive/delete-sender", request)
            BASE.require(result.get("deleted") is True and result.get("deleted_count") == 1,
                         "archive deletion not confirmed")
            if child is None:
                BASE.require(result.get("mutation_receipt", {}).get("outcome") == "completed",
                             "durable archive receipt missing")
            deleted.append(sender)
        observation["after"] = BASE.inspect(daemon, deleted)
    finally:
        try:
            shutdown_started = time.monotonic()
            observation["exit_code"] = daemon.close()
        except AssertionError as error:
            # The helper kills only its own isolated process group on timeout.
            # Keep this as a failed lifecycle observation, never a successful
            # clean exit, so later rollback generations can still be inspected.
            if str(error) not in ("bounded daemon shutdown timed out", "daemon child process survived shutdown"):
                raise
            observation["exit_code"] = daemon.process.returncode
            BASE.require(daemon.owned_group_exit_confirmed, "owned group survived bounded cleanup")
        observation["shutdown_timeout"] = daemon.shutdown_timeout
        observation["forced_owned_group_cleanup"] = daemon.forced_owned_group_cleanup
        observation["owned_group_exit_confirmed"] = daemon.owned_group_exit_confirmed
        # Daemon helper uses one log name; retain each generation before restart.
        log = daemon.log.read_text()
        observation["shutdown_seconds"] = round(time.monotonic() - shutdown_started, 3)
        log_path = report_root / (label + ".log")
        log_path.write_text(log)
        observation["log"] = str(log_path)
    observation["output_sha256"] = BASE.hashes(root)
    observation["lifecycle_after"] = lifecycle_footprints(root)
    observation["native_shutdown_receipt"] = "switchd-rs: native shutdown complete" in log
    observation["shutdown_error"] = (
        "call_mode_shutdown_unconfirmed" if
        "call mode shutdown unconfirmed" in log or "call_mode_shutdown_unconfirmed" in log or
        "switchd-rs shutdown: call mode:" in log else None)
    observation["sockets_remaining"] = [name for name in ["s", "s.go", "s.rs", "s.in"]
                                        if os.path.lexists(root / name)]
    if expect_quarantine:
        BASE.require(recovery_evidence_preserved(observation["lifecycle_before"], observation["lifecycle_after"]),
                     "candidate changed recovery evidence")
        BASE.require(shutdown_accepted(observation, native=True) and observation["shutdown_seconds"] < 15,
                     "quarantined candidate did not promptly drain its own owners/sockets")
    return observation


def run(args, report):
    BASE.require(sys.platform == "darwin", "mandatory sandbox requires macOS")
    runtime = BASE.BUILD / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="p555-", dir=runtime)).resolve()
    BASE.require(len(os.fsencode(root / "data/s.in")) < 100, "Unix socket path too long")
    report["runtime_root"] = str(root)
    report["harness_sha256"] = BASE.digest(Path(__file__).resolve())
    report["sandbox_helper_sha256"] = BASE.digest(HELPER)
    bundles = root / "bundles"
    bundles.mkdir()
    old_copy, candidate_copy = bundles / "Legacy.app", bundles / "Candidate.app"
    expected_candidate_version = args.expected_candidate_version
    BASE.require(bool(expected_candidate_version.strip()), "expected candidate version is required")
    report["expected_candidate_version"] = expected_candidate_version
    report["bundles"] = [copy_bundle(args.legacy_app, old_copy, "3.42"),
                          copy_bundle(args.candidate_app, candidate_copy, expected_candidate_version)]
    old, child = old_copy / BACKEND / "switchd-rs", old_copy / BACKEND / "switchd"
    candidate = candidate_copy / BACKEND / "switchd-rs"
    BASE.require(child.is_file(), "legacy pair is incomplete")
    BASE.require(not (candidate_copy / BACKEND / "switchd").exists(), "candidate bundle contains Go child")
    initial = root / "seed"
    initial.mkdir()
    BASE.seed(initial)
    original = BASE.hashes(initial)
    report["original_fixture_sha256"] = original
    # Control establishes the candidate's clean-root behavior with the same
    # payload and sandbox before testing old-root provenance.
    control = root / "control"
    control.mkdir()
    BASE.copy_stores(initial, control)
    report["clean_candidate_control"] = exercise(control, root, "control", candidate, None, [])
    BASE.require(shutdown_accepted(report["clean_candidate_control"], native=True), "clean candidate control failed")
    data = root / "data"
    data.mkdir()
    BASE.copy_stores(initial, data)
    report["stages"] = []
    deleted = []
    for label, binary, go_child, sender in [
        ("legacy_first_write", old, child, "fixture-delete"),
        ("candidate_inplace_upgrade", candidate, None, "fixture-upgrade"),
        ("legacy_inplace_rollback", old, child, "fixture-downgrade"),
        ("candidate_inplace_reupgrade", candidate, None, None),
    ]:
        # Never replace, delete, or reclassify a legacy lock/marker/runtime file.
        report["stages"].append(exercise(data, root, label, binary, go_child, deleted, sender,
                                       args.expect_module_quarantine and label.startswith("candidate")))
        BASE.require(BASE.hashes(initial) == original, "original fixture copy changed")
    for bundle in report["bundles"]:
        BASE.require(bundle_inventory(Path(bundle["source"])) == bundle["files"], "source bundle drifted")
        BASE.require(bundle_inventory(Path(bundle["copy"])) == bundle["files"], "copied bundle drifted")
    report["original_fixture_unchanged"] = True
    report["data_roundtrip_passed"] = True
    report["candidate_shutdown_accepted"] = all(shutdown_accepted(stage, native=True)
                                                for stage in report["stages"] if stage["name"].startswith("candidate"))
    report["legacy_shutdown_accepted"] = all(shutdown_accepted(stage)
                                             for stage in report["stages"] if stage["name"].startswith("legacy"))
    report["validation_completed"] = True
    report["cleanup"] = "all owned process groups exited; legacy runtime files preserved"
    report["passed"] = report["candidate_shutdown_accepted"] and report["legacy_shutdown_accepted"]
    report["candidate_module_isolation_accepted"] = args.expect_module_quarantine and report["candidate_shutdown_accepted"]


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--legacy-app", type=Path, required=True)
    parser.add_argument("--candidate-app", type=Path, required=True)
    parser.add_argument("--candidate-label", required=True, help="describe actual snapshot; do not imply current source")
    parser.add_argument("--expected-candidate-version", default="3.43",
                        help="exact CFBundleShortVersionString expected in the candidate; defaults to the historical 3.43 fixture")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--expect-module-quarantine", action="store_true",
                        help="require typed module isolation and bounded clean candidate exits; old exit remains independent")
    return parser.parse_args(argv)


def main():
    args = parse_args()
    BASE.require(args.report.resolve().is_relative_to(BASE.BUILD.resolve()), "report must be under workspace build")
    report = {"passed": False, "validation_completed": False, "synthetic_only": True,
              "candidate_label": args.candidate_label,
              "scope": "copied bundle daemons; one unchanged synthetic data directory; no SwiftUI launch/install",
              "full_p6_acceptance": False}
    try:
        run(args, report)
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({key: report.get(key) for key in
                      ["passed", "validation_completed", "data_roundtrip_passed", "candidate_shutdown_accepted", "error"]}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
