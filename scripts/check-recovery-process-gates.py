#!/usr/bin/env python3
"""Run all eight ignored recovery child entrypoints through controlled parents.

Cargo identifies the exact current test executable. Every child is synthetic,
hardware/network denied and reaped; no release daemon or physical I/O is used.
The historical matrices are source-owned so a cold export needs no old reports.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
SOURCE = Path(__file__).resolve().parents[1]
BUILD = SOURCE.parent / "build"
SCRIPTS = SOURCE / "scripts"
SUITES = ("restore", "publication", "backup", "finite")
MARKER = "SWITCH_RECOVERY_PROCESS_GATE "
# This separate fixture is controlled by call_mode_adb::child_tests::HeldProbe.
# It is inventoried here, never dispatched as a recovery worker/observer.
KNOWN_NON_RECOVERY_IGNORED = frozenset({"call_mode_adb::child_tests::real_child_fixture"})


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def source_digest():
    value = subprocess.check_output(
        [sys.executable, "-B", str(SCRIPTS / "source-identity.py")], text=True).strip()
    require(len(value) == 64 and all(c in "0123456789abcdef" for c in value),
            "source identity is malformed")
    return value


def select_test_binary(output, build=BUILD, source=SOURCE, *, verify_file=True):
    """Accept exactly one Cargo-reported daemon test artifact, never a glob."""
    records = [json.loads(line) for line in output.splitlines() if line.strip()]
    finished = [row for row in records if row.get("reason") == "build-finished"]
    require(len(finished) == 1 and finished[0].get("success") is True,
            "Cargo did not finish the test artifact build successfully")
    candidates = []
    for row in records:
        target = row.get("target", {})
        if (row.get("reason") != "compiler-artifact"
                or target.get("name") != "switchd-rs"
                or row.get("profile", {}).get("test") is not True):
            continue
        require(target.get("kind") == ["bin"]
                and Path(target.get("src_path", "")) == source / "rust/crates/proletariusd/src/main.rs"
                and Path(row.get("manifest_path", "")) == source / "rust/crates/proletariusd/Cargo.toml",
                "Cargo reported the wrong daemon target identity")
        value = row.get("executable")
        require(isinstance(value, str) and Path(value).is_absolute(),
                "Cargo daemon test artifact has no absolute executable")
        path = Path(value)
        require(path.is_relative_to(build.resolve()) and ".." not in path.parts,
                "Cargo test executable is outside this build")
        if verify_file:
            metadata = path.lstat()
            require(path.resolve(strict=True) == path
                    and stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1
                    and metadata.st_size > 0 and os.access(path, os.X_OK),
                    "Cargo test executable is unsafe or outside this build")
        candidates.append(path)
    require(len(candidates) == 1, "Cargo must report exactly one daemon test executable")
    return candidates[0]



def checked_file(path, root, *, executable=False):
    path, root = Path(path), Path(root).resolve(strict=True)
    metadata = path.lstat()
    require(path.is_absolute() and path.resolve(strict=True) == path
            and path.is_relative_to(root) and stat.S_ISREG(metadata.st_mode)
            and metadata.st_nlink == 1 and metadata.st_uid == os.geteuid()
            and metadata.st_size > 0 and (not executable or os.access(path, os.X_OK)),
            "retained recovery file is unsafe or outside its owned root")
    return path


def stable_binary_hash(path, root, *, retained=True):
    path = checked_file(path, root, executable=True)
    before = path.lstat()
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        opened = os.fstat(descriptor)
        require((before.st_dev, before.st_ino) == (opened.st_dev, opened.st_ino),
                "retained binary changed before opening")
        digest = hashlib.sha256()
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            while block := stream.read(1024 * 1024):
                digest.update(block)
        after, current = os.fstat(descriptor), path.lstat()
        fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size", "st_mtime_ns", "st_ctime_ns")
        require(all(getattr(before, key) == getattr(after, key) == getattr(current, key)
                    for key in fields), "retained binary changed during reading")
        require(not retained or stat.S_IMODE(current.st_mode) == 0o500,
                "unsafe retained binary mode")
        return digest.hexdigest()
    finally:
        os.close(descriptor)


def retain_test_binary(binary, digest, build=BUILD):
    """Exclusive byte copy; an identical existing copy is never overwritten."""
    artifacts = Path(build) / "artifacts"
    artifacts.mkdir(mode=0o700, parents=True, exist_ok=True)
    require(artifacts.resolve(strict=True) == artifacts, "artifact root is a symlink")
    directory = artifacts / ("recovery-process-" + digest)
    directory.mkdir(mode=0o700, exist_ok=True)
    metadata = directory.lstat()
    require(directory.resolve(strict=True) == directory and stat.S_ISDIR(metadata.st_mode)
            and metadata.st_uid == os.geteuid() and stat.S_IMODE(metadata.st_mode) == 0o700,
            "unsafe retained artifact directory")
    destination = directory / "switchd-rs-libtest"
    original = Path(binary)
    initial = original.lstat()
    before = stable_binary_hash(original, build, retained=False)
    if not destination.exists() and not destination.is_symlink():
        descriptor = os.open(original, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, "rb") as source, destination.open("xb") as output:
            opened = os.fstat(source.fileno())
            require((initial.st_dev, initial.st_ino) == (opened.st_dev, opened.st_ino),
                    "Cargo artifact changed before copying")
            shutil.copyfileobj(source, output, length=1024 * 1024)
        destination.chmod(0o500)
    require(stable_binary_hash(destination, artifacts) == before
            == stable_binary_hash(original, build, retained=False),
            "Cargo artifact changed or retained copy differs")
    final = original.lstat()
    require(all(getattr(initial, field) == getattr(final, field) for field in
                ("st_dev", "st_ino", "st_mode", "st_nlink", "st_size", "st_mtime_ns", "st_ctime_ns")),
            "Cargo artifact metadata changed during retention")
    return destination


def verify_retained_binary(summary, digest, reports_root):
    build = Path(reports_root).resolve().parent
    expected = build / "artifacts" / ("recovery-process-" + digest) / "switchd-rs-libtest"
    require(summary.get("binary") == str(expected), "recovery binary is not the exact retained artifact")
    checked_file(expected, build / "artifacts", executable=True)
    require(stable_binary_hash(expected, build / "artifacts") == summary.get("binary_sha256"),
            "retained recovery binary digest changed")
    discovery = checked_file(summary["cargo_artifact_log"], reports_root)
    require(sha(discovery) == summary.get("cargo_artifact_log_sha256"),
            "Cargo artifact discovery evidence changed")
    # Discovery remains auditable after the disposable original target is removed.
    original = select_test_binary(discovery.read_text(), build=build,
                                  source=build.parent / "src", verify_file=False)
    require(summary.get("cargo_discovered_binary") == str(original)
            and summary.get("cargo_discovered_binary_sha256") == summary.get("binary_sha256"),
            "retained binary differs from the Cargo discovery identity")


def expected_cases(name, driver):
    if name == "restore":
        return [{"phase": phase} for phase in driver.PHASES]
    if name == "publication":
        return ([{"phase": phase, "negative": ""} for phase in driver.PHASES]
                + [{"phase": value.split(":")[0] + ":before-prepare", "negative": value}
                   for value in driver.NEGATIVES])
    if name == "backup":
        return list(driver.CASES)
    return list(driver.EXPECTED_MATRIX["cases"])


def required_helpers():
    result = set()
    for name in SUITES:
        driver = load("ignored_inventory_" + name, "check-restore-journal-crash.py"
                      if name == "restore" else f"recovery_process_{name}.py")
        result.update((driver.WORKER, driver.OBSERVER))
    require(len(result) == 8, "controlled ignored daemon helper set changed")
    return result


def validate_ignored_inventory_names(names):
    required = required_helpers()
    require(type(names) is list and all(type(name) is str for name in names)
            and len(names) == len(set(names))
            and required.isdisjoint(KNOWN_NON_RECOVERY_IGNORED)
            and set(names) == required | KNOWN_NON_RECOVERY_IGNORED,
            "ignored daemon test set differs from the eight controlled parent helpers "
            "and the exact known non-recovery fixture")


def ignored_inventory(suite, report, crash):
    command = ["/usr/bin/sandbox-exec", "-f", str(suite.profile), str(suite.binary),
               "--list", "--ignored", "--format", "terse"]
    process = subprocess.Popen(command, env=crash.LIFETIME.BASE.environment(suite.path),
                               cwd=suite.path, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               text=True, start_new_session=True)
    row = dict(pid=process.pid, command=command, reaped=False, forced_cleanup=False, timeout=False)
    report["ignored_inventory_process"] = row
    try:
        raw, _ = process.communicate(timeout=20)
    except subprocess.TimeoutExpired:
        row.update(timeout=True, forced_cleanup=True)
        os.killpg(process.pid, signal.SIGKILL)
        raw, _ = process.communicate(timeout=3)
    row.update(exit_code=process.returncode, reaped=True)
    try:
        os.killpg(process.pid, 0)
    except ProcessLookupError:
        row["owned_process_group_gone"] = True
    else:
        row["forced_cleanup"] = True
        os.killpg(process.pid, signal.SIGKILL)
    path = suite.logs / "ignored-test-inventory.log"
    path.write_text(raw)
    row.update(log=str(path), log_sha256=sha(path))
    names = [line.removesuffix(": test") for line in raw.splitlines() if line.endswith(": test")]
    report["ignored_daemon_helpers"] = names
    require(row.get("owned_process_group_gone") is True and process.returncode == 0
            and not row["forced_cleanup"] and not row["timeout"], "ignored inventory process failed")
    validate_ignored_inventory_names(names)


def checked_log(row):
    # Retained copies survive cleanup of disposable runtime directories.
    path = Path(row.get("retained_log", row.get("log", "")))
    metadata = path.lstat()
    require(stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1
            and metadata.st_size > 0 and not path.is_symlink()
            and sha(path) == row.get("log_sha256"), "missing or changed owned process log")
    if "retained_log" in row:
        require(row.get("retained_log_sha256") == row.get("log_sha256"),
                "retained owned log has a different identity")


def validate_suite(name, report, driver, *, mode="full"):
    require(report.get("validation_completed") is True and not report.get("error")
            and not report.get("cleanup_errors") and not report.get("case_cleanup_errors")
            and not report.get("outer_budget_exceeded"), "recovery parent did not complete cleanly")
    require(mode in ("full", "smoke") and report.get("mode") == mode
            and report.get("synthetic_only") is True and report.get("hardware_acceptance") is False
            and report.get("full_p6_acceptance") is False and report.get("overall_product_complete") is False,
            "recovery parent mode or non-product scope is invalid")
    canary = report.get("sandbox_canary", {})
    require(canary.get("exit_code") == 0 and canary.get("inherited_child") is True
            and canary.get("reaped") is True and canary.get("owned_process_group_gone") is True
            and not any(canary.get(k) for k in
                        ("timeout", "forced_cleanup", "surviving_owned_descendants", "interrupted_cleanup")),
            "mandatory inherited hardware/network sandbox canary failed")
    checked_log(canary)
    if name == "restore":
        inventory = report.get("ignored_inventory_process", {})
        require(inventory.get("exit_code") == 0 and inventory.get("reaped") is True
                and inventory.get("owned_process_group_gone") is True
                and not inventory.get("forced_cleanup") and not inventory.get("timeout"),
                "ignored daemon helper inventory is incomplete or failed")
        validate_ignored_inventory_names(report.get("ignored_daemon_helpers", []))
        checked_log(inventory)
    expected = expected_cases(name, driver)
    fixed_counts = {"restore": 8, "publication": 42, "backup": 105, "finite": 13}
    require(len(expected) == fixed_counts[name], "reviewed recovery matrix size changed")
    if mode == "smoke" and name != "restore":
        expected = expected[:1]
    rows = report.get("scenarios", [])
    require(len(rows) == len(expected) and all(
        row.get("passed") is True and not row.get("cleanup_errors")
        and all(row.get(key) == value for key, value in case.items())
        for row, case in zip(rows, expected)), "recovery matrix has missing, reordered or failed cases")
    processes = report.get("processes", [])
    require(len(processes) == 2 * len(expected), "recovery process ledger is incomplete")
    keys = ("case_index", "phase", "negative") if name == "publication" else (("phase",) if name == "restore" else ("case_id",))
    if name == "publication":
        require(all(type(row.get("case_index")) is int and row["case_index"] == index
                    for index, row in enumerate(rows)),
                "publication scenario index is missing, reordered or not an integer")
        require(all(type(row.get("case_index")) is int
                    and 0 <= row["case_index"] < len(expected) for row in processes),
                "publication child index is missing, invalid or not an integer")
    # Publication negatives alter only the restart observer. Its worker
    # deliberately creates the normal baseline with an empty negative selector.
    # Canonical case_index keeps those same-phase worker baselines distinct.
    # Normalize expected workers only; actual rows and negative observers stay exact.
    wanted = collections.Counter((tuple(
        case_index if key == "case_index" else
        "" if name == "publication" and test == driver.WORKER and key == "negative"
        else case.get(key) for key in keys), test)
        for case_index, case in enumerate(expected) for test in (driver.WORKER, driver.OBSERVER))
    actual = collections.Counter((tuple(row.get(key) for key in keys), row.get("test"))
                                 for row in processes)
    require(actual == wanted, "recovery workers/observers are missing or duplicated")
    pids = [row.get("pid") for row in processes]
    require(all(type(pid) is int and pid > 0 for pid in pids), "owned process PID is missing")
    # PIDs may be reused after a fully reaped earlier case; case/test identity
    # and each Popen-derived child receipt establish ownership instead.
    for row in processes:
        worker = row["test"] == driver.WORKER
        code = (-signal.SIGKILL if name == "restore" else 86) if worker else 0
        if name == "finite":
            code = 0
        require(type(row.get("exit_code")) is int and row["exit_code"] == code
                and row.get("reaped") is True and row.get("owned_process_group_gone") is True
                and not any(row.get(key) for key in ("forced_cleanup", "timeout", "surviving_owned_descendants"))
                and bool(row.get("intentional_sigkill")) == (name == "restore" and worker),
                "recovery child exit or owned process cleanup failed")
        expected_events = (["worker_complete"] if worker else ["observer_complete"]) if name == "finite" else (
            ["boundary"] if worker and name == "restore" else
            ["ready", "boundary"] if worker else ["restart"])
        require(row.get("event_kinds") == expected_events,
                "recovery child event count or sequence differs from its exact helper role")
        checked_log(row)
        if name == "finite":
            require(row.get("libtest") == {"passed": 1, "failed": 0, "ignored": 0},
                    "finite I/O child did not execute the exact test")
    if name in SUITES:
        inventory = report.get("inventory_process", {})
        require(inventory.get("exit_code") == 0 and inventory.get("reaped") is True
                and inventory.get("owned_process_group_gone") is True
                and not inventory.get("forced_cleanup") and not inventory.get("timeout"),
                "recovery test inventory process failed")
        checked_log(inventory)


def json_record(path, digest, reports_root):
    path = Path(path)
    before = path.lstat()
    require(path.is_absolute() and path.resolve(strict=True) == path
            and path.is_relative_to(reports_root.resolve())
            and stat.S_ISREG(before.st_mode) and before.st_nlink == 1,
            "recovery evidence must be a private regular file under capsule reports")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino),
                "recovery evidence changed before opening")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            raw = stream.read()
        after = os.fstat(descriptor)
        require((opened.st_size, opened.st_mtime_ns, opened.st_nlink)
                == (after.st_size, after.st_mtime_ns, after.st_nlink)
                and after.st_nlink == 1, "recovery evidence changed during reading")
    finally:
        os.close(descriptor)
    require(hashlib.sha256(raw).hexdigest() == digest, "recovery evidence digest changed")
    value = json.loads(raw)
    require(isinstance(value, dict), "recovery evidence must be a JSON object")
    return value


def verify_full_marker(log, expected_digest, *, reports_root=None):
    """One shared final verifier for cold-build audit and release attestation."""
    reports_root = BUILD / "reports" if reports_root is None else Path(reports_root)
    markers = [json.loads(line[len(MARKER):]) for line in log.splitlines() if line.startswith(MARKER)]
    require(len(markers) == 1, "cold log needs exactly one full recovery-process marker")
    marker = markers[0]
    require(set(marker) == {"mode", "passed", "source_digest", "binary_sha256",
                            "scenario_count", "child_count", "report", "sha256"}
            and marker.get("mode") == "full" and marker.get("passed") is True
            and marker.get("source_digest") == expected_digest
            and type(marker.get("scenario_count")) is int and marker["scenario_count"] == 168
            and type(marker.get("child_count")) is int and marker["child_count"] == 336
            and isinstance(marker.get("binary_sha256"), str)
            and len(marker["binary_sha256"]) == 64
            and all(c in "0123456789abcdef" for c in marker["binary_sha256"]),
            "cold recovery-process marker is incomplete, smoke, or for another source")
    summary = json_record(marker["report"], marker["sha256"], reports_root)
    require(all(summary.get(key) == marker[key] for key in
                ("mode", "passed", "source_digest", "binary_sha256", "scenario_count", "child_count"))
            and summary.get("synthetic_only") is True and summary.get("hardware_acceptance") is False
            and summary.get("full_p6_acceptance") is False and summary.get("overall_product_complete") is False,
            "full recovery summary differs from its cold-log marker")
    verify_retained_binary(summary, expected_digest, reports_root)
    suites = summary.get("suites")
    require(isinstance(suites, list) and [row.get("suite") for row in suites] == list(SUITES),
            "full recovery summary has incomplete or reordered parent suites")
    for record in suites:
        name = record["suite"]
        report = json_record(record["path"], record["sha256"], reports_root)
        require(report.get("passed") is True and report.get("source_digest") == expected_digest
                and report.get("binary_sha256") == marker["binary_sha256"]
                and report.get("binary") == summary["binary"]
                and record.get("scenario_count") == len(report.get("scenarios", []))
                and record.get("child_count") == len(report.get("processes", [])),
                "recovery parent receipt differs from its summary")
        filename = "check-restore-journal-crash.py" if name == "restore" else f"recovery_process_{name}.py"
        require(report.get("driver_sha256") == sha(SCRIPTS / filename), "recovery parent source changed")
        driver = load("verify_recovery_" + name, filename)
        evidence_rows = [report.get("sandbox_canary", {}), report.get("inventory_process", {})]
        if name == "restore":
            evidence_rows.append(report.get("ignored_inventory_process", {}))
        evidence_rows.extend(report.get("processes", []))
        for row in evidence_rows:
            require("retained_log" in row, "recovery parent lacks retained child evidence")
            checked_file(row["retained_log"], reports_root)
        validate_suite(name, report, driver)
    require(sum(row["scenario_count"] for row in suites) == 168
            and sum(row["child_count"] for row in suites) == 336,
            "full recovery summary does not bind all scenarios and children")
    return {"path": marker["report"], "sha256": marker["sha256"]}


def archive_logs(report, destination):
    """Keep immutable copies of child receipts beside the capsule gate report."""
    destination.mkdir(mode=0o700)
    rows = [report.get("sandbox_canary", {}), report.get("inventory_process", {}),
            report.get("ignored_inventory_process", {})]
    rows.extend(report.get("processes", []))
    for index, row in enumerate(rows):
        if row.get("log") and Path(row["log"]).is_file():
            target = destination / f"{index}.log"
            with target.open("xb") as output:
                output.write(Path(row["log"]).read_bytes())
            row["retained_log"] = str(target)
            row["retained_log_sha256"] = sha(target)
    if report.get("test_inventory_log"):
        target = destination / "test-inventory.log"
        shutil.copyfile(report["test_inventory_log"], target)
        report["retained_test_inventory_log"] = str(target)
        report["retained_test_inventory_log_sha256"] = sha(target)


def run_suite(name, binary, digest, binary_hash, directory, runtime_parent, jobs, *, smoke=False):
    # A fresh helper instance keeps the event discriminator local to this suite.
    crash = load("recovery_crash_" + name, "check-restore-journal-crash.py")
    driver = crash if name == "restore" else load("recovery_matrix_" + name, f"recovery_process_{name}.py")
    mode = "smoke" if smoke else "full"
    report = dict(schema_version=1, suite=name, mode=mode, passed=False, validation_completed=False,
                  synthetic_only=True, hardware_acceptance=False, power_loss_tested=False,
                  full_p6_acceptance=False, overall_product_complete=False,
                  source_digest=digest, binary=str(binary), binary_sha256=binary_hash,
                  driver_sha256=sha(SCRIPTS / ("check-restore-journal-crash.py" if name == "restore"
                                              else f"recovery_process_{name}.py")),
                  scenarios=[], processes=[])
    suite = None
    started = time.monotonic()
    try:
        suite = crash.LIFETIME.Suite(binary, report, runtime_parent=runtime_parent)
        suite.canary()
        if name == "restore":
            ignored_inventory(suite, report, crash)
            crash.run(suite, report)
        elif name == "finite":
            driver.inventory(suite, report, crash)
            driver.run_matrix(suite, report, crash, min(jobs, 2), started + driver.OUTER_SECONDS, smoke=smoke)
        else:
            driver.run_matrix(suite, report, crash, jobs, smoke=smoke)
        require(sha(binary) == binary_hash and source_digest() == digest,
                "source or test executable changed during recovery parent")
        report["validation_completed"] = True
    except Exception as error:
        report["error"] = type(error).__name__ + ": " + str(error)
    finally:
        if suite is not None:
            try:
                suite.cleanup_canary()
            except Exception as error:
                report.setdefault("cleanup_errors", []).append(str(error))
            report["synthetic_evidence_retained"] = str(suite.path)
        try:
            validate_suite(name, report, driver, mode=mode)
            report["passed"] = True
        except Exception as error:
            report.setdefault("verification_error", str(error))
        report["elapsed_seconds"] = time.monotonic() - started
        try:
            archive_logs(report, directory / (name + "-logs"))
        except Exception as error:
            report["passed"] = False
            report.setdefault("cleanup_errors", []).append("retain logs: " + str(error))
        with (directory / (name + ".json")).open("x") as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write("\n")
    require(report["passed"], f"{name} recovery parent failed; inspect {directory / (name + '.json')}")
    print(f"recovery process {mode} gate {name}: {len(report['scenarios'])} scenarios passed", flush=True)
    return {"suite": name, "path": str(directory / (name + ".json")),
            "sha256": sha(directory / (name + ".json")), "scenario_count": len(report["scenarios"]),
            "child_count": len(report["processes"])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, choices=range(1, 5), default=4)
    parser.add_argument("--smoke", action="store_true", help="development representatives; never full software acceptance")
    args = parser.parse_args()
    reports = BUILD / "reports"
    reports.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="recovery-process-gates-", dir=reports)).resolve()
    summary = dict(schema_version=1, mode="smoke" if args.smoke else "full", passed=False, synthetic_only=True,
                   hardware_acceptance=False, full_p6_acceptance=False,
                   overall_product_complete=False, suites=[])
    try:
        require(sys.platform == "darwin", "mandatory Darwin hardware/network sandbox required")
        digest = source_digest()
        summary["source_digest"] = digest
        command = [os.environ.get("SWITCH_CARGO_BIN") or "cargo", "+1.98.0", "test", "--offline", "--locked",
                   "--manifest-path", str(SOURCE / "rust/Cargo.toml"), "-p", "proletariusd",
                   "--bin", "switchd-rs", "--no-run", "--message-format=json"]
        summary["cargo_command"] = command
        result = subprocess.run(command, cwd=SOURCE, capture_output=True, text=True)
        (directory / "cargo-artifacts.jsonl").write_text(result.stdout)
        (directory / "cargo-artifacts.stderr.log").write_text(result.stderr)
        require(result.returncode == 0, "Cargo test artifact discovery failed")
        original = select_test_binary(result.stdout)
        binary_hash = sha(original)
        binary = retain_test_binary(original, digest)
        summary.update(binary=str(binary), binary_sha256=binary_hash,
                       cargo_discovered_binary=str(original), cargo_discovered_binary_sha256=binary_hash,
                       cargo_artifact_log=str(directory / "cargo-artifacts.jsonl"),
                       cargo_artifact_log_sha256=sha(directory / "cargo-artifacts.jsonl"))
        require(source_digest() == digest, "source changed during Cargo artifact discovery")
        temp_root = Path(os.environ.get("SWITCH_TEST_TMPDIR", "/private/tmp")).resolve(strict=True)
        runtime_parent = Path(tempfile.mkdtemp(prefix="swrc-", dir=temp_root)).resolve()
        summary["private_canary_parent"] = str(runtime_parent)
        for name in SUITES:
            summary["suites"].append(run_suite(name, binary, digest, binary_hash,
                                              directory, runtime_parent, args.jobs, smoke=args.smoke))
        require(source_digest() == digest and sha(binary) == binary_hash, "final source or binary drift")
        summary["passed"] = True
    except Exception as error:
        summary["error"] = type(error).__name__ + ": " + str(error)
    finally:
        summary["scenario_count"] = sum(row["scenario_count"] for row in summary["suites"])
        summary["child_count"] = sum(row["child_count"] for row in summary["suites"])
        with (directory / "summary.json").open("x") as output:
            json.dump(summary, output, ensure_ascii=False, indent=2)
            output.write("\n")
    marker = {key: summary.get(key) for key in ("mode", "passed", "source_digest", "binary_sha256",
                                               "scenario_count", "child_count")}
    marker.update(report=str(directory / "summary.json"), sha256=sha(directory / "summary.json"))
    print(MARKER + json.dumps(marker, sort_keys=True), flush=True)
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
