#!/usr/bin/env python3
"""Opt-in macOS old/current store roundtrip; mandatory hardware/network sandbox.

Only the four literal synthetic business stores from native_recovery_tests.rs
are copied between fresh stage directories. Runtime locks, Keychain and all
other persisted types are deliberately outside this test's acceptance scope.
"""
import argparse
import ctypes
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time

SOURCE = Path(__file__).resolve().parents[1]
BUILD = SOURCE.parent / "build"
TOKEN = "p546-synthetic-only"
STORES = {
    "sms-archive.json": "ARCHIVE",
    "device-send-ledger.json": "LEDGER",
    "multistandby/store-v1.json": "MULTI",
    "multistandby/operations-v1.json": "OPERATIONS",
}


def require(condition, detail):
    if not condition:
        raise AssertionError(detail)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def hashes(root):
    return {name: digest(root / name) for name in STORES}


def seed(root):
    text = (SOURCE / "rust/crates/proletariusd/tests/native_recovery_tests.rs").read_text()
    for name, constant in STORES.items():
        match = re.search(r'const ' + constant + r': &str = r#"(.*?)"#;', text, re.S)
        require(match is not None, f"missing literal recovery fixture {constant}")
        value = json.loads(match[1])
        if constant == "ARCHIVE":
            for suffix in ["upgrade", "downgrade"]:
                value.append({"id": f"fixture-{suffix}", "sender": f"fixture-{suffix}",
                              "content": f"delete during {suffix}", "direction": "in",
                              "timestamp": "2026-09-01T12:02:00Z", "device": "reader"})
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value, ensure_ascii=False))


def copy_stores(source, target):
    for name in STORES:
        destination = target / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        # Copy only the explicit payload allowlist, never runtime locks/sockets.
        destination.write_bytes((source / name).read_bytes())
    require(hashes(source) == hashes(target), "stage payload copy mismatch")


def sandbox_profile(root):
    # All child processes inherit these kernel restrictions. No fallback to an
    # unsandboxed launch is allowed, including when sandbox-exec is unavailable.
    path = json.dumps(str(root))
    return f'''(version 1)
(allow default)
(deny iokit-open)
(deny mach-lookup)
(deny file-read* file-write* (subpath "/dev"))
(allow file-read-metadata (subpath "/dev"))
(allow file-read-data (literal "/dev"))
(allow file-read* (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom"))
(deny file-write*)
(allow file-write* (subpath {path}) (literal "/dev/null"))
(deny network-outbound)
(allow network-outbound (subpath {path}))
'''


def environment(root, legacy=None):
    env = {"PATH": "/usr/bin:/bin", "HOME": str(root), "TMPDIR": str(root),
           "SWITCH_DATA_DIR": str(root), "PROLETARIUS_APP_TOKEN": TOKEN,
           "PROLETARIUS_BLE_SOCKET": str(root / "absent-ble")}
    if legacy:
        env["PROLETARIUS_GO_BACKEND_PATH"] = str(legacy)
    return env


def canary(root, blocked_socket, inherited=False):
    # These policy queries do not enumerate/open actual USB/PCSC hardware.
    library = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    for operation in [b"iokit-open", b"mach-lookup"]:
        require(library.sandbox_check(os.getpid(), operation, 0) == 1,
                f"sandbox did not deny {operation!r}")
    with open("/dev/null", "rb"):
        pass
    try:
        with open("/dev/zero", "rb"):
            pass
    except PermissionError:
        pass
    else:
        raise AssertionError("sandbox did not deny device-file access")
    for address, family in [(blocked_socket, socket.AF_UNIX),
                            (("127.0.0.1", 9), socket.AF_INET)]:
        with socket.socket(family) as client:
            try:
                client.connect(address)
            except PermissionError:
                pass
            else:
                raise AssertionError("sandbox did not deny non-stage connection")
    with socket.socket(socket.AF_UNIX) as client:
        client.connect(str(root / "allowed"))
    if not inherited:
        result = subprocess.run([sys.executable, str(Path(__file__).resolve()),
                                 "--canary", str(root), blocked_socket, "--inherited"],
                                env=environment(root), capture_output=True, text=True, timeout=10)
        require(result.returncode == 0, "child sandbox inheritance failed: " + result.stderr)


def validate_sandbox(root, profile, blocked_socket):
    with socket.socket(socket.AF_UNIX) as allowed:
        allowed.bind(str(root / "allowed"))
        allowed.listen(4)
        result = subprocess.run(["/usr/bin/sandbox-exec", "-f", str(profile),
                                 sys.executable, str(Path(__file__).resolve()),
                                 "--canary", str(root), str(blocked_socket)],
                                env=environment(root), capture_output=True, text=True, timeout=20)
        require(result.returncode == 0, "mandatory sandbox canary failed: " + result.stderr)
    (root / "allowed").unlink()


def wait_owned_group_gone(process, timeout=1.0):
    """A reaped leader alone does not prove its owned descendants have exited."""
    deadline = time.monotonic() + timeout
    while True:
        process.poll()
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return True
        except OSError:
            # EPERM and other observation failures never prove disappearance.
            return False
        if time.monotonic() >= deadline:
            return False
        time.sleep(min(.02, max(0, deadline - time.monotonic())))


class Daemon:
    def __init__(self, executable, root, legacy=None):
        self.root, self.legacy = root, legacy
        self.log = root / "daemon.log"
        profile = root / "hardware-denied.sb"
        self.process = None
        self.shutdown_timeout = False
        self.forced_owned_group_cleanup = False
        self.owned_group_exit_confirmed = False
        # The old supervisor waits for explicit-serial initialization before
        # launching Go. Let its discovery fail inside the mandatory sandbox;
        # passing a nonexistent serial path would prevent old-pair readiness.
        arguments = [] if legacy else ["-port", str(root / "absent-serial")]
        with self.log.open("wb") as log:
            self.process = subprocess.Popen(
                ["/usr/bin/sandbox-exec", "-f", str(profile), str(executable),
                 *arguments, "-listen", "unix:" + str(root / "s")],
                env=environment(root, legacy), cwd=root, stdin=subprocess.DEVNULL,
                stdout=log, stderr=log, start_new_session=True)

    def request(self, method, path, body=None):
        connection = http.client.HTTPConnection("localhost", timeout=5)
        stream = socket.socket(socket.AF_UNIX)
        stream.settimeout(5)
        stream.connect(str(self.root / "s"))
        connection.sock = stream
        try:
            connection.request(method, path, None if body is None else json.dumps(body),
                               {"x-proletarius-app-token": TOKEN, "content-type": "application/json"})
            response = connection.getresponse()
            result = json.loads(response.read())
            require(response.status == 200, f"{method} {path}: {response.status} {result}")
            return result
        finally:
            connection.close()

    def ready(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "daemon exited before readiness; inspect " + str(self.log))
            try:
                value = self.request("GET", "/api/v2/runtime")
                if self.legacy:
                    require(value.get("version") == "3.42.0", "legacy binary is not 3.42.0")
                    if value.get("go_backend_connected") is not True:
                        time.sleep(.05)
                        continue
                else:
                    require(value.get("backend") == "rust", "current backend identity")
                    require(value.get("go_backend_connected") is False, "current invoked Go")
                    require(value.get("go_backend_phase") == "retired", "current Go owner not retired")
                return {key: value.get(key) for key in
                        ["backend", "version", "go_backend_connected", "go_backend_phase", "default_runtime"]}
            except (OSError, http.client.HTTPException):
                time.sleep(.05)
        raise AssertionError("daemon readiness timeout; inspect " + str(self.log))

    def close(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
        try:
            code = self.process.wait(timeout=85)
        except subprocess.TimeoutExpired:
            self.shutdown_timeout = True
            self.forced_owned_group_cleanup = True
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait(timeout=1)
            self.owned_group_exit_confirmed = wait_owned_group_gone(self.process)
            require(self.owned_group_exit_confirmed, "owned process group termination unconfirmed")
            raise AssertionError("bounded daemon shutdown timed out")
        # A surviving owned child is a cleanup failure, even if gateway exited.
        try:
            os.killpg(self.process.pid, 0)
        except ProcessLookupError:
            self.owned_group_exit_confirmed = True
        else:
            self.forced_owned_group_cleanup = True
            os.killpg(self.process.pid, signal.SIGKILL)
            self.owned_group_exit_confirmed = wait_owned_group_gone(self.process)
            require(self.owned_group_exit_confirmed, "owned process group termination unconfirmed")
            raise AssertionError("daemon child process survived shutdown")
        return code


def inspect(daemon, deleted):
    rows = daemon.request("GET", "/api/v2/sms-archive")
    expected = {"fixture-accepted", "fixture-delete", "fixture-upgrade", "fixture-downgrade"} - set(deleted)
    require({row["sender"] for row in rows} == expected and len(rows) == len(expected),
            "archive deletion lost/resurrected or records duplicated")
    sentinel = next(row for row in rows if row["sender"] == "fixture-accepted")
    require(sentinel["content"] == "confirmed archive body", "archive content changed")
    require(sentinel["future_field"] == {"keep": "旧格式 😀"}, "unknown archive field lost")
    require(sentinel["send_request_id"] == "accepted-old", "archive request ID lost")
    for identity, state in [("accepted-old", "accepted"), ("interrupted-old", "unknown")]:
        intent = daemon.request("GET", "/api/sms/send-intents?client_request_id=" + identity)
        require(intent["state"] == state and intent["can_submit"] is False,
                "send confirmation or no-replay admission regressed")
        require("attempt" not in intent, "private attempt leaked")
    records = json.loads((daemon.root / "device-send-ledger.json").read_text())[0]["records"]
    require(set(records) == {"accepted-old", "interrupted-old"}, "new send intent appeared")
    require(records["interrupted-old"]["attempt"] == "old-interrupted-attempt", "unknown attempt changed")
    require(records["interrupted-old"]["state"] == "unknown", "durable send outcome changed")
    messages = daemon.request("GET", "/api/multistandby/messages?profile_id=old-profile")["messages"]
    require(len(messages) == 2, "N-standby messages lost/duplicated")
    by_id = {message["message_id"]: message for message in messages}
    require(by_id["received"]["state"] == "received" and
            by_id["received"]["body"] == "confirmed old multi inbox", "inbox changed")
    require(by_id["interrupted"]["state"] == "unknown" and
            by_id["interrupted"]["error"] == "backend_restarted_during_submission",
            "N-standby interrupted send became actionable")
    require("attempt_token" not in by_id["interrupted"], "private N-standby attempt leaked")
    for identity, state in [("old-running", "unknown"), ("old-completed", "completed")]:
        operation = daemon.request("GET", "/api/multistandby/operations/" + identity)
        require(operation["state"] == state, "operation recovery regressed")
    return {"archive_count": len(rows), "unknown_send_can_submit": False,
            "multistandby_count": len(messages), "unknown_fields_preserved": True}


def run(args, report):
    require(sys.platform == "darwin", "mandatory sandbox requires macOS")
    current, old = args.current.resolve(strict=True), args.legacy_backend.resolve(strict=True)
    legacy = old / "switchd-rs"
    child = old / "switchd"
    for executable in [current, legacy, child]:
        require(executable.is_file() and os.access(executable, os.X_OK), "missing executable: " + str(executable))
    report["binaries_sha256"] = {str(path): digest(path) for path in [current, legacy, child]}
    report["harness_sha256"] = digest(Path(__file__).resolve())
    report["fixture_source_sha256"] = digest(
        SOURCE / "rust/crates/proletariusd/tests/native_recovery_tests.rs")
    runtime = BUILD / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="p546-", dir=runtime)).resolve()
    report["runtime_root"] = str(root)
    require(len(os.fsencode(root / "4/s.in")) < 100, "Unix socket path too long")
    initial = root / "seed"
    initial.mkdir()
    seed(initial)
    initial_hashes = hashes(initial)
    report["original_fixture_sha256"] = initial_hashes
    deleted, previous = [], initial
    report["stages"] = []
    # This safe external canary endpoint is outside every stage allowlist.
    with socket.socket(socket.AF_UNIX) as blocked:
        blocked.bind(str(root / "blocked"))
        blocked.listen(8)
        for index, (label, executable, go_child, sender) in enumerate([
            ("legacy_write", legacy, child, "fixture-delete"),
            ("rust_upgrade_write", current, None, "fixture-upgrade"),
            ("legacy_downgrade_write", legacy, child, "fixture-downgrade"),
            ("rust_reupgrade_read", current, None, None),
        ], 1):
            stage = root / str(index)
            stage.mkdir()
            copy_stores(previous, stage)
            profile = stage / "hardware-denied.sb"
            profile.write_text(sandbox_profile(stage))
            validate_sandbox(stage, profile, root / "blocked")
            observation = {"name": label, "sandbox_canary_and_child_inheritance": "passed",
                           "sandbox_profile_sha256": digest(profile),
                           "input_sha256": hashes(stage)}
            report["stages"].append(observation)
            daemon = Daemon(executable, stage, go_child)
            try:
                observation["runtime"] = daemon.ready()
                observation["before"] = inspect(daemon, deleted)
                if sender:
                    request = {"sender": sender}
                    if go_child is None:
                        request["client_request_id"] = "p6-data-" + label + "-" + sender
                    result = daemon.request("POST", "/api/v2/sms-archive/delete-sender", request)
                    require(result.get("deleted") is True and result.get("deleted_count") == 1,
                            "archive write not acknowledged")
                    if go_child is None:
                        require(result.get("mutation_receipt", {}).get("outcome") == "completed",
                                "durable archive receipt missing")
                    deleted.append(sender)
                observation["after"] = inspect(daemon, deleted)
            finally:
                observation["exit_code"] = daemon.close()
            if go_child is None:
                require(observation["exit_code"] == 0, "current Rust shutdown failed")
                require("switchd-rs: native shutdown complete" in daemon.log.read_text(), "missing native exit receipt")
            else:
                log = daemon.log.read_text()
                require(observation["exit_code"] == 0 or
                        (observation["exit_code"] == 1 and
                         ("call_mode_shutdown_unconfirmed" in log or "call mode shutdown unconfirmed" in log)),
                        "unexpected legacy shutdown failure")
                observation["legacy_shutdown_issue"] = (
                    "call_mode_shutdown_unconfirmed" if observation["exit_code"] else None)
            observation["output_sha256"] = hashes(stage)
            require(hashes(initial) == initial_hashes, "original rollback payload changed")
            previous = stage
    report["original_fixture_unchanged"] = True
    require(report["binaries_sha256"] == {str(path): digest(path) for path in [current, legacy, child]},
            "an executable changed during the roundtrip")
    report["cleanup"] = "all owned process groups exited"


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--canary":
        canary(Path(sys.argv[2]), sys.argv[3], "--inherited" in sys.argv)
        return 0
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current", type=Path, required=True, help="freshly built current switchd-rs")
    parser.add_argument("--legacy-backend", type=Path, required=True, help="3.42 backend directory containing both executables")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    require(args.report.resolve().is_relative_to(BUILD.resolve()), "report must be under workspace build")
    report = {"passed": False, "synthetic_only": True,
              "scope": "four-store copied-payload read/write roundtrip; no installed-app rollback",
              "no_send_api_called": True, "full_p6_acceptance": False}
    try:
        run(args, report)
        report["passed"] = True
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"passed": report["passed"], "report": str(args.report), "error": report.get("error")}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
