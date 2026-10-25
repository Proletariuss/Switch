#!/usr/bin/env python3
"""Opt-in real Swift/file/Rust DTO/daemon handoff durability acceptance.

Runs only supplied precompiled binaries. Every runtime child uses the existing
mandatory hardware and external-network deny profile, with no unsandboxed
fallback. Checkpoints are produced only by the compiled production Swift store;
this driver writes neither a successful checkpoint nor a preferences plist.
All synthetic roots, failures, logs and receipts are retained under build.
"""
import argparse
import copy
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import socket
import stat
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
SELF = Path(__file__).resolve()
SOURCE = SELF.parents[1]
BUILD = SOURCE.parent / "build"
CHECKER = SOURCE / "scripts/check-daemon-lifetime.py"
SPEC = importlib.util.spec_from_file_location("handoff_lifetime_helpers", CHECKER)
LIFE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIFE)
REQUIRE = LIFE.require
HANDOFF = "/api/v2/sms/forwarding/handoff"
LEDGER = "sms-forwarding-handoff.json"
SWIFT_CHECKPOINT = "sms-forwarding-swift-checkpoint-v1.json"
EXACT = "tests::swift_forwarding_handoff_fixture_round_trips"
LIMIT = 8 * 1024 * 1024


def source_hashes():
    paths = {
        SELF, CHECKER, SOURCE / "scripts/check-p6-data-roundtrip.py",
        SOURCE / "rust/Cargo.toml", SOURCE / "rust/Cargo.lock",
        SOURCE / "rust/crates/proletarius-handoff-core/Cargo.toml",
        SOURCE / "rust/crates/proletarius-audio-core/Cargo.toml",
        SOURCE / "rust/crates/proletarius-audio-core/src/lib.rs",
        SOURCE / "scripts/build-handoff-core.sh",
        SOURCE / "scripts/test-forwarding-authority.sh",
        SOURCE / "scripts/test-sms-forwarding-client.sh",
        SOURCE / "scripts/test-forwarding-handoff-compat.sh",
        SOURCE / "rust/crates/proletarius-api-contract/src/lib.rs",
        SOURCE / "rust/crates/proletarius-store/src/lib.rs",
        SOURCE / "rust/crates/proletariusd/src/main.rs",
        SOURCE / "rust/crates/proletariusd/src/forwarding_handoff_store.rs",
        SOURCE / "rust/crates/proletariusd/src/daemon_lifetime.rs",
        SOURCE / "rust/crates/proletariusd/src/module_recovery.rs",
        SOURCE / "rust/crates/proletariusd/src/shutdown.rs",
    }
    paths.update((SOURCE / "rust/crates/proletarius-handoff-core/src").rglob("*.rs"))
    paths.update((SOURCE / "app/Sources").glob("SMSForwarding*.swift"))
    paths.update(path for path in (SOURCE / "app/Tests/SMSForwardingClient").rglob("*")
                 if path.is_file())
    return {str(path.relative_to(SOURCE)): LIFE.sha(path) for path in sorted(paths)}


def executable(path, label):
    path = path.resolve(strict=True)
    REQUIRE(path.is_relative_to(BUILD.resolve()), label + " must be under Switch build")
    REQUIRE(path.is_file() and os.access(path, os.X_OK), label + " must be a compiled executable")
    with path.open("rb") as stream:
        magic = stream.read(4)
    REQUIRE(magic in {bytes.fromhex(value) for value in
                     ("feedface", "cefaedfe", "feedfacf", "cffaedfe", "cafebabe", "bebafeca", "cafebabf", "bfbafeca")},
            label + " must be a compiled Mach-O executable, not a source wrapper")
    return path


def backend(path):
    if path.is_dir() and path.suffix == ".app":
        path = path / "Contents/Resources/backend/switchd-rs"
    return executable(path, "candidate backend")


def json_file(path):
    info = path.lstat()
    REQUIRE(stat.S_ISREG(info.st_mode) and not path.is_symlink(), "not a regular generated JSON file: " + str(path))
    REQUIRE(0 < info.st_size <= LIMIT, "generated JSON size outside fixture budget")
    return json.loads(path.read_bytes())


def same_json(left, right):
    # Preserve JSON scalar types: Python's True == 1 must not weaken DTO parity.
    encode = lambda value: json.dumps(value, sort_keys=True, ensure_ascii=False,
                                      separators=(",", ":"), allow_nan=False)
    return encode(left) == encode(right)


def file_tree(root):
    result = {}
    for path in sorted(root.rglob("*")):
        info = path.lstat()
        REQUIRE(not stat.S_ISLNK(info.st_mode), "unexpected synthetic checkpoint symlink")
        if stat.S_ISREG(info.st_mode):
            result[str(path.relative_to(root))] = {
                "size": info.st_size, "mode": stat.S_IMODE(info.st_mode), "sha256": LIFE.sha(path)}
    return result


def request(daemon, method, path, body=None, token=None):
    stream = socket.socket(socket.AF_UNIX)
    stream.settimeout(3)
    connection = http.client.HTTPConnection("localhost", timeout=3)
    try:
        stream.connect(str(daemon.public))
        connection.sock = stream
        connection.request(method, path, body=None if body is None else json.dumps(body), headers={
            "x-proletarius-app-token": daemon.token if token is None else token,
            "content-type": "application/json"})
        response = connection.getresponse()
        payload = response.read(LIMIT + 1)
        REQUIRE(len(payload) <= LIMIT, "daemon JSON response exceeded fixture budget")
        return response.status, json.loads(payload)
    finally:
        connection.close()
        stream.close()


def observed_request(row, daemon, method, path, body=None, token=None):
    status, value = request(daemon, method, path, body, token)
    record = {"method": method, "path": path, "status": status, "response": value}
    if body is not None:
        record["request_sha256"] = hashlib.sha256(json.dumps(body, sort_keys=True).encode()).hexdigest()
    if token is not None:
        record["noncurrent_token"] = True
    row.setdefault("http", []).append(record)
    return status, value


def clean_ready(daemon, row):
    # Suite.Daemon.ready intentionally expects startup quarantine. This test
    # starts fresh roots and must exercise the ordinary authenticated listener.
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        REQUIRE(daemon.process.poll() is None, "daemon exited before clean readiness")
        try:
            status, value = request(daemon, "GET", "/api/v2/runtime")
            REQUIRE(status == 200 and value.get("ok") is True and value.get("backend") == "rust"
                    and value.get("go_backend_connected") is False
                    and value.get("go_backend_phase") == "retired", "wrong candidate runtime/authentication")
            REQUIRE("module_recovery" not in value, "fresh handoff root unexpectedly quarantined")
            REQUIRE(not Path(str(daemon.public) + ".go").exists(), "unexpected Go socket")
            daemon.ready_observed = True
            daemon.record["ready_runtime"] = value
            row.setdefault("clean_runtime", []).append(value)
            row.setdefault("owned_sockets", []).append(LIFE.sockets(daemon.public))
            return
        except (OSError, http.client.HTTPException):
            time.sleep(0.03)
    daemon.record["timeout"] = True
    raise AssertionError("candidate clean readiness timeout")


class FixtureProcess(LIFE.Daemon):
    """Reuse exact owned-group wait/reap/cleanup receipts for compiled clients."""
    def __init__(self, suite, label, program, arguments, environment):
        self.suite = suite
        self.root = Path(environment["SWITCH_DATA_DIR"])
        self.started = time.monotonic()
        self.group_gone = False
        self.ready_observed = False
        self.log = suite.logs / (label + ".log")
        self.record = {"label": label, "kind": "compiled_fixture", "program": str(program),
                       "program_sha256": LIFE.sha(program), "arguments": arguments,
                       "root": str(self.root), "log": str(self.log),
                       "sandbox_profile_sha256": LIFE.sha(suite.profile),
                       "sandbox_policy_check_required_before_exec": True,
                       "timeout": False, "forced_cleanup": False,
                       "environment": {key: value for key, value in environment.items()
                                       if key.startswith("SWITCH_FORWARDING_") and not key.endswith("TOKEN")}}
        suite.report["processes"].append(self.record)
        with self.log.open("xb") as output:
            self.process = subprocess.Popen(
                ["/usr/bin/sandbox-exec", "-f", str(suite.profile), sys.executable,
                 str(SELF), "--sandboxed-exec", str(program), *arguments],
                env=environment, cwd=self.root, stdin=subprocess.DEVNULL,
                stdout=output, stderr=output, start_new_session=True)
        self.record["pid"] = self.process.pid
        suite.children.append(self)


def fixture_environment(root, fixture, roundtrip, mode=None, daemon=None):
    environment = LIFE.BASE.environment(root)
    environment.update({
        "PYTHONDONTWRITEBYTECODE": "1", "CFFIXED_USER_HOME": str(root),
        "SWITCH_BUILD_DIR": str(BUILD), "SWITCH_FORWARDING_CHECKPOINT_DIR": str(root),
        "SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH": str(fixture),
        "SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH": str(roundtrip),
    })
    if mode is not None:
        environment["SWITCH_FORWARDING_HANDOFF_MODE"] = mode
    if daemon is not None:
        environment["SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET"] = str(daemon.public)
        environment["SWITCH_FORWARDING_HANDOFF_LIVE_TOKEN"] = daemon.token
    return environment


def swift_mode(suite, binary, mode, root, fixture, roundtrip, daemon=None):
    REQUIRE(not fixture.exists(), "Swift export must not overwrite a previous artifact")
    child = FixtureProcess(suite, "swift-" + mode, binary, [],
                           fixture_environment(root, fixture, roundtrip, mode, daemon))
    REQUIRE(child.wait(30) == 0, "compiled Swift " + mode + " failed; preserve its log")
    child.record["successful_exit_without_forced_cleanup"] = True
    child.record["sandbox_policy_checked_before_exec"] = True
    if mode in ("live", "live-replay"):
        prefix = "SWIFT_LIVE_RECEIPT "
        receipts = [json.loads(line[len(prefix):]) for line in child.log_text().splitlines()
                    if line.startswith(prefix)]
        expected = {"mode": mode, "actual_get": 1 if mode == "live" else 0,
                    "actual_post": 1, "accepted": True,
                    "already_accepted": mode == "live-replay"}
        REQUIRE(len(receipts) == 1 and same_json(receipts[0], expected),
                "compiled Swift lacks its exact actual Unix HTTP request/acknowledgement receipt")
        child.record["actual_swift_unix_http_receipt"] = receipts[0]
    return child


def require_snapshot(row, daemon, expected):
    status, value = observed_request(row, daemon, "GET", HANDOFF)
    REQUIRE(status == 200 and value.get("accepted") is True and same_json(value.get("snapshot"), expected),
            "real daemon did not return the exact actual Swift snapshot")
    return value


def checkpoint_proof(root, expected):
    path = root / SWIFT_CHECKPOINT
    REQUIRE(same_json(json_file(path), expected), "production Swift checkpoint differs from its actual DTO export")
    information = path.lstat()
    REQUIRE(stat.S_IMODE(information.st_mode) == 0o600 and information.st_nlink == 1,
            "production Swift checkpoint lacks its private regular-file boundary")
    return {"path": str(path), "sha256": LIFE.sha(path), "size": information.st_size,
            "mode": stat.S_IMODE(information.st_mode), "nlink": information.st_nlink}


def ledger_proof(root, expected):
    path = root / LEDGER
    value = json_file(path)
    REQUIRE(isinstance(value, list) and len(value) == 1 and value[0].get("schema_version") == 1
            and same_json(value[0].get("handoff"), expected), "actual Rust durable ledger differs from Swift")
    return {"path": str(path), "sha256": LIFE.sha(path), "size": path.stat().st_size,
            "mode": stat.S_IMODE(path.stat().st_mode)}


def run_layers(suite, args):
    artifacts = suite.path / "x"
    artifacts.mkdir(mode=0o700)
    checkpoint = suite.path / "c"
    checkpoint.mkdir(mode=0o700)
    produced, restarted, roundtrip = (artifacts / name for name in ("p.json", "r.json", "rust.json"))

    def independent_swift(row):
        row["checkpoint_directory"] = str(checkpoint)
        swift_mode(suite, args.swift_binary, "produce", checkpoint, produced, roundtrip)
        first = json_file(produced)
        REQUIRE(isinstance(first, dict) and first.get("schema_version") == 1, "Swift did not export its real DTO")
        row["produce_json_sha256"] = LIFE.sha(produced)
        row["production_checkpoint_after_produce"] = checkpoint_proof(checkpoint, first)
        row["production_files_after_produce"] = file_tree(checkpoint)
        REQUIRE(row["production_files_after_produce"], "Swift produce left no durable checkpoint file")
        swift_mode(suite, args.swift_binary, "restart", checkpoint, restarted, roundtrip)
        REQUIRE(same_json(json_file(restarted), first), "separate Swift restart did not reconstruct the exact production file snapshot")
        row["restart_json_sha256"] = LIFE.sha(restarted)
        row["production_checkpoint_after_restart"] = checkpoint_proof(checkpoint, first)
        row["production_files_after_restart"] = file_tree(checkpoint)
        row["independent_process_reconstruction_equal"] = True
    suite.case("actual_swift_production_file_survives_process_exit_and_restart", independent_swift)

    if args.rust_test_binary is not None:
        def dto_roundtrip(row):
            REQUIRE(not roundtrip.exists(), "Rust output must not pre-exist")
            child = FixtureProcess(suite, "rust-exact-dto", args.rust_test_binary,
                                   ["--exact", EXACT, "--ignored", "--nocapture"],
                                   fixture_environment(checkpoint, restarted, roundtrip))
            REQUIRE(child.wait(30) == 0, "exact compiled Rust DTO test failed")
            child.record["sandbox_policy_checked_before_exec"] = True
            text = child.log_text()
            REQUIRE(re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", text) is not None,
                    "Rust invocation did not execute exactly one required ignored test")
            REQUIRE("test " + EXACT + " ... ok" in text, "different Rust test executed")
            REQUIRE(same_json(json_file(roundtrip), json_file(restarted)), "actual Rust DTO serialization changed Swift semantics")
            row["rust_output_sha256"] = LIFE.sha(roundtrip)
            verify = artifacts / "v.json"
            swift_mode(suite, args.swift_binary, "verify", checkpoint, verify, roundtrip)
            # verify must decode Rust's file and reopen its production store.
            REQUIRE(same_json(json_file(verify), json_file(restarted)), "Swift verify did not re-export the actual reopened checkpoint")
            row["swift_verify_json_sha256"] = LIFE.sha(verify)
            row["production_checkpoint_after_verify"] = checkpoint_proof(checkpoint, json_file(restarted))
            row["production_files_after_verify"] = file_tree(checkpoint)
            row["exact_required_rust_test"] = EXACT
            row["true_swift_rust_swift_roundtrip"] = True
        suite.case("exact_compiled_rust_dto_then_independent_swift_production_file_verify", dto_roundtrip)
    else:
        suite.report["dto_roundtrip_not_requested"] = True
        suite.report["dto_roundtrip_limitation"] = "No Rust output is fabricated; Rust DTO and Swift verify layers were not run."

    def live_daemon(row):
        root = suite.path / "d"
        root.mkdir(mode=0o700)
        REQUIRE(not any(root.iterdir()), "live daemon must start from a fresh root")
        row["fresh_root"] = str(root)
        row["initial_root_empty"] = True
        first = suite.spawn("handoff-daemon-first", root)
        clean_ready(first, row)
        status, empty = observed_request(row, first, "GET", HANDOFF)
        REQUIRE(status == 200 and empty.get("accepted") is False and empty.get("snapshot") is None,
                "fresh daemon already has a handoff snapshot")
        live = artifacts / "l.json"
        client = swift_mode(suite, args.swift_binary, "live", root, live, roundtrip, first)
        row["first_actual_swift_unix_http_receipt"] = client.record["actual_swift_unix_http_receipt"]
        expected = json_file(live)
        REQUIRE(isinstance(expected, dict), "live Swift DTO export missing")
        require_snapshot(row, first, expected)
        row["first_durable_ledger"] = ledger_proof(root, expected)
        row["first_production_checkpoint"] = checkpoint_proof(root, expected)
        row["live_swift_export_sha256"] = LIFE.sha(live)
        first.close()
        row["first_real_sigterm_exit_zero"] = first.process.returncode == 0

        second = suite.spawn("handoff-daemon-restarted", root)
        REQUIRE(first.token != second.token, "daemon restart must use a new app token")
        clean_ready(second, row)
        require_snapshot(row, second, expected)
        status, _ = observed_request(row, second, "GET", HANDOFF, token=first.token)
        REQUIRE(status == 401, "old daemon token survived restart")
        replay = artifacts / "lr.json"
        client = swift_mode(suite, args.swift_binary, "live-replay", root, replay, roundtrip, second)
        row["restarted_actual_swift_unix_http_receipt"] = client.record["actual_swift_unix_http_receipt"]
        REQUIRE(same_json(json_file(replay), expected), "new Swift process did not replay its original production checkpoint")
        require_snapshot(row, second, expected)
        row["restarted_production_checkpoint"] = checkpoint_proof(root, expected)
        # Independently verify the server's exact idempotent acknowledgement.
        status, ack = observed_request(row, second, "POST", HANDOFF, expected)
        REQUIRE(status == 200 and ack.get("accepted") is True and ack.get("already_accepted") is True
                and ack.get("handoff_id") == expected.get("handoff_id"), "restarted daemon replay lacks exact durable acknowledgement")
        row["idempotent_acknowledgement"] = ack
        before = (root / LEDGER).read_bytes()
        row["ledger_before_conflicts"] = ledger_proof(root, expected)
        changed = copy.deepcopy(expected)
        REQUIRE(isinstance(changed.get("nwait_sms"), dict), "actual Swift fixture lacks N-wait cursor scope")
        changed["nwait_sms"]["cursor"] = str(changed["nwait_sms"].get("cursor", "")) + "-conflict"
        replacement = copy.deepcopy(expected)
        replacement["handoff_id"] = str(uuid.uuid4())
        for name, altered in [("same_id_changed_cursor", changed), ("new_id_same_snapshot", replacement)]:
            status, conflict = observed_request(row, second, "POST", HANDOFF, altered)
            REQUIRE(status == 409 and conflict.get("code") == "handoff_conflict"
                    and conflict.get("retryable") is False, "wrong exact handoff conflict: " + name)
            REQUIRE((root / LEDGER).read_bytes() == before, "conflict mutated actual durable ledger: " + name)
            require_snapshot(row, second, expected)
            row.setdefault("conflicts", []).append({"name": name, "status": status,
                                                       "unchanged_ledger_sha256": LIFE.sha(root / LEDGER)})
        row["ledger_after_conflicts"] = ledger_proof(root, expected)
        REQUIRE(row["ledger_before_conflicts"]["sha256"] == row["ledger_after_conflicts"]["sha256"],
                "negative requests changed Rust handoff ledger hash")
        second.close()
        REQUIRE((root / LEDGER).read_bytes() == before, "real daemon shutdown changed handoff snapshot")
        row["restarted_real_sigterm_exit_zero"] = second.process.returncode == 0
        row["new_daemon_and_swift_process_read_original_file"] = True
        row["provider_configured_or_message_submitted"] = False
        row["live_checkpoint_directory_is_daemon_root"] = True
    suite.case("fresh_real_daemon_swift_file_handoff_restart_replay_conflict_and_real_shutdown", live_daemon)


def sandboxed_exec(arguments):
    REQUIRE(arguments, "compiled child executable is missing")
    LIFE.policy_canary()
    os.execve(arguments[0], arguments, os.environ.copy())


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--sandboxed-exec":
        sandboxed_exec(sys.argv[2:])
        return 1
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="candidate .app or switchd-rs executable under build")
    parser.add_argument("--swift-binary", required=True, type=Path)
    parser.add_argument("--rust-test-binary", type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    destination = args.report.resolve()
    REQUIRE(destination.is_relative_to((BUILD / "reports").resolve()), "report must stay under Switch build/reports")
    REQUIRE(not destination.exists() and not destination.is_symlink(), "refusing to overwrite an existing report")
    report = {"schema_version": 1, "passed": False, "validation_completed": False,
              "purpose": "actual production-file Swift restart, exact Rust DTO and real daemon durable handoff",
              "scope": "synthetic checkpoints and authenticated Unix HTTP only; no provider, SMS, hardware or carrier acceptance",
              "rust_dto_requested": args.rust_test_binary is not None,
              "scenarios": [], "processes": []}
    suite = None
    try:
        REQUIRE(sys.platform == "darwin", "mandatory macOS sandbox required")
        args.binary = backend(args.binary)
        args.swift_binary = executable(args.swift_binary, "compiled Swift fixture")
        if args.rust_test_binary is not None:
            args.rust_test_binary = executable(args.rust_test_binary, "compiled Rust exact-test fixture")
        inputs = [args.binary, args.swift_binary] + ([] if args.rust_test_binary is None else [args.rust_test_binary])
        inputs.extend(path.resolve(strict=True) for path in args.binary.parent.glob("*.dylib") if path.is_file())
        report["binary_inputs"] = {str(path): LIFE.sha(path) for path in inputs}
        report["os"] = {"platform": sys.platform, "macos_version": platform.mac_ver()[0],
                        "architecture": platform.machine()}
        report["source_inputs"] = source_hashes()
        report["zero_drift_scope"] = "listed compiled executable bytes plus listed production/fixture/helper source inputs; whole-project freeze audited separately"
        suite = LIFE.Suite(args.binary, report)
        suite.canary()
        run_layers(suite, args)
        report["validation_completed"] = True
    except Exception as error:
        report["error"] = type(error).__name__ + ": " + str(error)
    finally:
        cleanup_errors = []
        if suite is not None:
            for child in reversed(suite.children):
                try:
                    child.cleanup()
                except Exception as error:
                    cleanup_errors.append(child.record["label"] + ": " + str(error))
                finally:
                    child.record["reaped"] = child.process.poll() is not None
                    child.record["owned_process_group_gone"] = not child.group_exists()
                    child.record["full_log"] = child.log_text()
            try:
                suite.cleanup_canary()
            except Exception as error:
                cleanup_errors.append("sandbox canary: " + str(error))
            report["synthetic_evidence_retained"] = str(suite.path)
            row = report.get("sandbox_canary", {})
            row["reaped"] = getattr(suite, "canary_process", None) is not None and suite.canary_process.returncode is not None
            if row.get("pid"):
                try:
                    os.killpg(row["pid"], 0)
                    row["owned_process_group_gone"] = False
                except ProcessLookupError:
                    row["owned_process_group_gone"] = True
            if row.get("log"):
                row["full_log"] = Path(row["log"]).read_text(errors="replace")
        report["cleanup_errors"] = cleanup_errors
        try:
            if "binary_inputs" in report:
                REQUIRE({path: LIFE.sha(Path(path)) for path in report["binary_inputs"]} == report["binary_inputs"],
                        "compiled inputs changed during validation")
                REQUIRE(source_hashes() == report["source_inputs"], "scoped production/fixture/helper inputs changed during validation")
                REQUIRE(suite is not None and LIFE.sha(suite.profile) == report["sandbox_profile_sha256"],
                        "mandatory OS profile changed during validation")
                report["listed_inputs_zero_drift"] = True
        except Exception as error:
            report["fingerprint_error"] = str(error)
        rows = report["processes"] + ([report["sandbox_canary"]] if "sandbox_canary" in report else [])
        report["all_processes_reaped"] = bool(rows) and all(row.get("reaped") for row in rows)
        report["no_forced_cleanup"] = bool(rows) and all(not row.get("forced_cleanup") for row in rows)
        bad = any(row.get("forced_cleanup") or row.get("timeout") or row.get("surviving_owned_descendants")
                  or row.get("owned_process_group_gone") is False for row in rows)
        report["full_cross_language_dto_roundtrip_passed"] = bool(args.rust_test_binary) and any(
            row.get("true_swift_rust_swift_roundtrip") and row.get("passed") for row in report["scenarios"])
        report["passed"] = (report["validation_completed"] and not report.get("error")
                            and not report.get("fingerprint_error") and not cleanup_errors and not bad
                            and report["all_processes_reaped"] and report["no_forced_cleanup"]
                            and all(row["passed"] for row in report["scenarios"]))
        destination.parent.mkdir(parents=True, exist_ok=True)
        with destination.open("x") as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
    print(json.dumps({"passed": report["passed"], "report": str(destination),
                      "full_cross_language_dto_roundtrip_passed": report["full_cross_language_dto_roundtrip_passed"],
                      "scenario_count": len(report["scenarios"]), "error": report.get("error")}, ensure_ascii=False))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
