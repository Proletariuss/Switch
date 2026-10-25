#!/usr/bin/env python3
"""Offline fixture/compiler evidence only; never launches Switch.app or switchd-rs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

SOURCE = Path(__file__).resolve().parents[2]
WORKSPACE = SOURCE.parent


def input_snapshot():
    # Discover afresh at each boundary, so additions/deletions also fail closed.
    roots = [SOURCE / "app/Sources", SOURCE / "app/Tests/AcceptanceContext",
             SOURCE / "app/Tests/AcceptanceBootstrap", SOURCE / "app/Tests/AcceptanceServices",
             SOURCE / "app/Tests/SMSStore", SOURCE / "scripts/acceptance"]
    paths = sorted({path for root in roots for path in root.rglob("*")
                    if path.is_file() and "__pycache__" not in path.parts
                    and path.suffix in {".swift", ".py", ".rb"}})
    return {str(path.relative_to(SOURCE)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in paths}


def finish_report(output, before, snapshot, report, error=None):
    try:
        after = snapshot()
        changed = sorted(key for key in before.keys() | after.keys() if before.get(key) != after.get(key))
    except Exception as exc:
        after, changed = {}, ["snapshot-error: " + repr(exc)]
    stable = not changed
    (output / "input-hashes.json").write_text(json.dumps({"before": before, "after": after,
        "inputs_stable_for_entire_run": stable, "changed_inputs": changed}, indent=2) + "\n")
    report.update(passed=stable and error is None, inputs_stable_for_entire_run=stable,
                  input_count=len(before), changed_inputs=changed)
    if error is not None:
        report["failure"] = str(error)
    if not stable:
        report["failure"] = "source/fixture/driver input drift; no passing credit"
    (output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    return 0 if report["passed"] else 1


def drift_fixture(case, output):
    # The self-test uses only newly-created mock inputs in this report. It never
    # writes, patches, renames or removes production source/fixture/driver files.
    inputs = output / "mock-inputs"
    inputs.mkdir()
    for name in ["Source.swift", "Fixture.swift", "driver.py"]:
        (inputs / name).write_text("original\n")
    def snapshot():
        return {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(inputs.iterdir())}
    before = snapshot()
    changes = {"source": "Source.swift", "fixture": "Fixture.swift", "driver": "driver.py"}
    if case in changes:
        (inputs / changes[case]).write_text("changed\n")
    elif case == "added":
        (inputs / "Added.swift").write_text("added\n")
    elif case == "deleted":
        (inputs / "Fixture.swift").unlink()
    elif case != "stable":
        raise SystemExit("unknown mock drift case")
    return finish_report(output, before, snapshot, {"scope": "offline-mock-input-drift-self-test"})


def run_batch(output, environment, checks):
    def run(label, command):
        started = time.monotonic()
        with (output / (label + ".log")).open("w") as log:
            result = subprocess.run([str(value) for value in command], cwd=SOURCE,
                                    env=environment, stdout=log, stderr=subprocess.STDOUT)
        checks.append({"label": label, "command": [str(value) for value in command],
                       "exit_code": result.returncode, "seconds": round(time.monotonic() - started, 3),
                       "log": label + ".log"})
        print(f"{label}: exit {result.returncode}", flush=True)
        (output / "checks.json").write_text(json.dumps(checks, indent=2) + "\n")
        if result.returncode:
            raise SystemExit(f"FAILED {label}; see retained log")

    swift = ["xcrun", "swiftc", "-parse-as-library", "-module-cache-path", str(output / "cache")]
    run("input-drift-selftest", [sys.executable, "scripts/acceptance/test-capsule-app-drift.py", output / "drift-selftest"])
    context = "app/Sources/AcceptanceContext.swift"
    fixture = "scripts/acceptance/GuardianFixture.swift"
    run("compile-context", swift + [context, fixture, "app/Tests/AcceptanceContext/main.swift", "-o", output / "bin/context-tests"])
    run("context-fixtures", [output / "bin/context-tests", output / "fixtures"])
    run("compile-bootstrap-fixture", swift + [context, "app/Tests/AcceptanceBootstrap/main.swift", "-o", output / "bin/bootstrap-fixture"])
    bootstrap_rows = []
    for label, args, expected in [
        ("production", [], 0),
        ("empty", ["--switch-acceptance-capsule="], 78),
        ("separate", ["--switch-acceptance-capsule", "/missing"], 78),
        ("unknown", ["--switch-acceptance-unknown"], 78),
        ("missing-proof", ["--switch-acceptance-capsule=/missing"], 78),
        ("duplicate", ["--switch-acceptance-capsule=/missing"] * 2, 78),
        ("relative", ["--switch-acceptance-capsule=relative"], 78),
    ]:
        command = [str(output / "bin/bootstrap-fixture")] + args
        result = subprocess.run(command,
                                cwd=SOURCE, env=environment, text=True, capture_output=True)
        sentinel = "SERVICE_INITIALIZATION_SENTINEL" in result.stdout
        if result.returncode != expected or sentinel != (expected == 0):
            raise SystemExit("bootstrap failed to fence dependency sentinel")
        bootstrap_rows.append({"name": label, "command": command, "exit_code": result.returncode,
                               "stdout": result.stdout, "stderr": result.stderr, "dependency_sentinel_reached": sentinel})
    (output / "bootstrap-fixtures.json").write_text(json.dumps(bootstrap_rows, indent=2) + "\n")
    checks.append({"label": "bootstrap-process-refusal-fixtures", "count": len(bootstrap_rows), "result": "PASS"})
    run("compile-guardian", swift + [context, fixture, "scripts/acceptance/guardian.swift", "-o", output / "bin/guardian"])
    run("guardian-selftest", [output / "bin/guardian", "--self-test", output / "guardian-fixture"])
    # An actual launch verb must remain unavailable even with a valid tool.
    with (output / "guardian-launch-refusal.log").open("w") as log:
        refused = subprocess.run([str(output / "bin/guardian"), "--launch"], cwd=SOURCE,
                                 env=environment, stdout=log, stderr=subprocess.STDOUT)
    if refused.returncode != 78:
        raise SystemExit("guardian real launch did not refuse")
    checks.append({"label": "guardian-real-launch-refused", "command": [str(output / "bin/guardian"), "--launch"], "exit_code": refused.returncode, "expected_exit_code": 78})
    run("prepare-sms-adapters", ["ruby", "app/Tests/SMSStore/prepare.rb", "app/Sources", output / "sms-generated"])
    generated = [output / "sms-generated/SMSStore.swift", output / "sms-generated/SMSModels.swift"]
    run("prepare-service-adapters", [sys.executable, "app/Tests/AcceptanceServices/prepare.py", "app/Sources", output / "service-generated"])
    generated_inputs = generated + [output / "service-generated/UpdateChecker.swift", output / "service-generated/AutoLaunch.swift"]
    generated_before = {str(path.relative_to(output)): hashlib.sha256(path.read_bytes()).hexdigest()
                        for path in generated_inputs}
    run("compile-services", swift + ["app/Tests/AcceptanceServices/PolicyAdapter.swift",
        "app/Tests/AcceptanceServices/ServiceSpies.swift", output / "service-generated/UpdateChecker.swift", output / "service-generated/AutoLaunch.swift",
        "app/Tests/SMSStore/Adapters.swift"] + generated + ["app/Tests/AcceptanceServices/main.swift", "-o", output / "bin/service-tests"])
    run("service-boundary-spies", [output / "bin/service-tests"])
    run("compile-sms-regression", swift + [context, "app/Tests/SMSStore/Adapters.swift", "app/Tests/SMSStore/main.swift"]
        + generated + ["-o", output / "bin/sms-regression-tests"])
    run("sms-production-regression", [output / "bin/sms-regression-tests"])
    # A frontend-only whole source check. No link, App process, FFI, backend,
    # original user-data migration or application service initialization occurs.
    sources = sorted(SOURCE.glob("app/Sources/**/*.swift"))
    before = {str(path.relative_to(SOURCE)): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}
    run("app-source-typecheck", swift + ["-typecheck", "-target", "arm64-apple-macos27.0", "-swift-version", "5", "-module-name", "Switch"] + sources)
    after = {str(path.relative_to(SOURCE)): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}
    (output / "app-typecheck-source-hashes.json").write_text(json.dumps({"before": before, "after": after,
        "source_stable_during_typecheck": before == after}, indent=2) + "\n")
    if before != after:
        raise RuntimeError("App source changed during typecheck")
    generated_after = {str(path.relative_to(output)): hashlib.sha256(path.read_bytes()).hexdigest()
                       for path in generated_inputs}
    (output / "generated-input-hashes.json").write_text(json.dumps({"before": generated_before,
        "after": generated_after, "stable_from_generation_through_last_check": generated_before == generated_after}, indent=2) + "\n")
    if generated_before != generated_after:
        raise RuntimeError("generated compilation input changed during run")
    artifacts = {}
    for path in sorted(output.rglob("*")):
        if path.is_file() and not path.is_symlink() and "cache" not in path.relative_to(output).parts:
            artifacts[str(path.relative_to(output))] = hashlib.sha256(path.read_bytes()).hexdigest()
    report = {"scope": "pure-fixtures-native-synthetic-guardian-and-Swift-typecheck", "checks": checks,
        "actual_app_launched": False, "actual_daemon_launched": False, "real_keychain_accessed": False,
        "hardware_or_external_network_used": False, "formal_duration_credit_seconds": 0,
        "g8_g9_runtime_admission": "NO-GO", "product_acceptance_complete": False,
        "app_source_stable_during_typecheck": before == after, "artifacts_sha256": artifacts,
        "prior_attempt_boundary_incident": "verification-1 service-spies used ambiguous Swift type shadowing and reached real AutoLaunch/launchctl. Exact generated host LaunchAgents plist was deleted; prior contents/existence unknown. Current explicit IO adapters plus private HOME do not erase that incident."}
    return report


def main():
    fixture_mode = len(sys.argv) == 4 and sys.argv[1] == "--input-drift-fixture"
    if len(sys.argv) != 2 and not fixture_mode:
        raise SystemExit("usage: test-capsule-app.py NEW_ABSOLUTE_REPORT_DIRECTORY")
    output = Path(sys.argv[-1])
    if not output.is_absolute() or not output.resolve().is_relative_to(WORKSPACE / "build"):
        raise SystemExit("report must be absolute and below this workspace build")
    output.mkdir(mode=0o700)
    if fixture_mode:
        return drift_fixture(sys.argv[2], output)
    before = input_snapshot()
    (output / "input-hashes-start.json").write_text(json.dumps(before, indent=2) + "\n")
    for name in ["bin", "cache", "fixtures", "sms-generated", "service-generated", "home", "tmp"]:
        (output / name).mkdir(mode=0o700)
    # Compiler output is local. This does not represent OS confinement evidence.
    environment = dict(os.environ, CLANG_MODULE_CACHE_PATH=str(output / "cache"),
                       SWIFT_MODULECACHE_PATH=str(output / "cache"), HOME=str(output / "home"),
                       CFFIXED_USER_HOME=str(output / "home"), TMPDIR=str(output / "tmp"),
                       PYTHONDONTWRITEBYTECODE="1")
    checks = []
    error = None
    report = {"scope": "offline-fixture-compilation-incomplete", "checks": checks}
    try:
        report = run_batch(output, environment, checks)
    except (Exception, SystemExit) as exc:
        error = repr(exc)
        print("FAILED: " + error, file=sys.stderr, flush=True)
    return finish_report(output, before, input_snapshot, report, error)


if __name__ == "__main__":
    sys.exit(main())
