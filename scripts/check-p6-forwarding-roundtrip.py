#!/usr/bin/env python3
"""Opt-in synthetic SWF1 reverse write test under the Iteration 546 sandbox."""
import argparse
import http.client
import importlib.util
import json
import os
from pathlib import Path
import socket
import sys
import tempfile

HELPER = Path(__file__).with_name("check-p6-data-roundtrip.py").resolve()
SPEC = importlib.util.spec_from_file_location("p546_roundtrip", HELPER)
BASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BASE)
FIXTURE = BASE.SOURCE / "rust/crates/proletarius-forwarding/tests/fixtures/go-state.swf1.hex"
QUEUE_ID = "71e946a085e000926dd82c21e14be42ec3a3bb91a76a84b3762e4922bbc1b5cf"
# Committed synthetic golden's bytes 00..1f; never a Keychain/user key.
KEY = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="
STATE = "sms-forwarding/state.enc"


def configure(key, modern=False):
    request = {"state_key": key, "telegram": {"enabled": False}, "bark": {"enabled": False}}
    if modern:
        correct = key == KEY
        request.update({
            "client_request_id": "p6-forwarding-config" if correct else "p6-forwarding-wrong-key",
            "client_sequence": 2 if correct else 1,
        })
    return request


def wrong_key_rejected(daemon, modern=False):
    before = BASE.digest(daemon.root / STATE)
    connection = http.client.HTTPConnection("localhost", timeout=5)
    stream = socket.socket(socket.AF_UNIX)
    stream.settimeout(5)
    stream.connect(str(daemon.root / "s"))
    connection.sock = stream
    try:
        connection.request("POST", "/api/sms/forwarding/configure",
                           json.dumps(configure("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", modern)),
                           {"x-proletarius-app-token": BASE.TOKEN, "content-type": "application/json"})
        response = connection.getresponse()
        body = json.loads(response.read())
        BASE.require(response.status == 400 and body.get("code") == "sms_forwarding_queue_unavailable",
                     "wrong key was not rejected with the expected closed-queue error")
    finally:
        connection.close()
    BASE.require(BASE.digest(daemon.root / STATE) == before, "wrong key replaced ciphertext")


def inspect(daemon, cancelled):
    status = daemon.request("GET", "/api/sms/forwarding/status")
    for name in ["configured", "telegram_enabled", "bark_enabled"]:
        BASE.require(status[name] is False, "provider became enabled/configured")
    tasks = [task for task in status["tasks"] if task["id"] == QUEUE_ID]
    BASE.require(len(tasks) == 1, "SWF1 task missing or duplicated")
    task = tasks[0]
    for provider, attempts in [("telegram", 2), ("bark", 1)]:
        channel = task[provider]
        BASE.require(channel["state"] == ("cancelled" if provider in cancelled else "paused_disabled"),
                     "provider cancellation or disabled state changed")
        BASE.require(channel["attempts"] == attempts, "a provider submission attempt was added/lost")
    BASE.require(task["telegram"]["confirmed_parts"] == 1 and task["telegram"]["total_parts"] == 2,
                 "confirmed Telegram chunk progress changed")
    BASE.require(task["telegram"]["error_code"] == "telegram_submission_unconfirmed",
                 "unknown Telegram submission outcome lost")
    legacy = [task for task in status["tasks"] if task["id"] == "fixture-legacy"]
    BASE.require(len(legacy) == 1 and legacy[0]["telegram"]["state"] == "paused_disabled"
                 and legacy[0]["telegram"]["attempts"] == 0, "legacy unbound task lost or replayed")
    BASE.require(status["pending"] == (1 if len(cancelled) == 2 else 2), "terminal task not archived exactly once")
    return {"telegram_state": task["telegram"]["state"], "bark_state": task["bark"]["state"],
            "telegram_attempts": 2, "bark_attempts": 1, "confirmed_parts": 1, "total_parts": 2,
            "pending": status["pending"]}


def run(args, report):
    BASE.require(sys.platform == "darwin", "mandatory sandbox requires macOS")
    current = args.current.resolve(strict=True)
    old = args.legacy_backend.resolve(strict=True)
    legacy, child = old / "switchd-rs", old / "switchd"
    for binary in [current, legacy, child]:
        BASE.require(binary.is_file() and os.access(binary, os.X_OK), "missing binary: " + str(binary))
    report["binaries_sha256"] = {str(binary): BASE.digest(binary) for binary in [current, legacy, child]}
    report["harness_sha256"] = BASE.digest(Path(__file__).resolve())
    report["sandbox_helper_sha256"] = BASE.digest(HELPER)
    report["golden_sha256"] = BASE.digest(FIXTURE)
    runtime = BASE.BUILD / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="p549-", dir=runtime)).resolve()
    BASE.require(len(os.fsencode(root / "4/s.in")) < 100, "Unix socket path too long")
    report["runtime_root"] = str(root)
    previous = root / "seed"
    state = previous / STATE
    state.parent.mkdir(parents=True, mode=0o700)
    state.write_bytes(bytes.fromhex(FIXTURE.read_text().strip()))
    state.chmod(0o600)
    BASE.require(state.read_bytes().startswith(b"SWF1"), "golden format not SWF1")
    seed_hash = BASE.digest(state)
    report["original_ciphertext_sha256"] = seed_hash
    report["stages"] = []
    cancelled = set()
    with socket.socket(socket.AF_UNIX) as blocked:
        blocked.bind(str(root / "blocked"))
        blocked.listen(8)
        for index, (label, binary, go_child, cancel) in enumerate([
            ("legacy_golden_read", legacy, child, None),
            ("rust_upgrade_cancel_telegram", current, None, "telegram"),
            ("legacy_downgrade_cancel_bark", legacy, child, "bark"),
            ("rust_reupgrade_terminal_read", current, None, None),
        ], 1):
            stage = root / str(index)
            destination = stage / STATE
            destination.parent.mkdir(parents=True, mode=0o700)
            destination.write_bytes((previous / STATE).read_bytes())
            destination.chmod(0o600)
            profile = stage / "hardware-denied.sb"
            profile.write_text(BASE.sandbox_profile(stage))
            BASE.validate_sandbox(stage, profile, root / "blocked")
            observation = {"name": label, "sandbox_canary_and_child_inheritance": "passed",
                           "sandbox_profile_sha256": BASE.digest(profile),
                           "input_ciphertext_sha256": BASE.digest(destination)}
            report["stages"].append(observation)
            daemon = BASE.Daemon(binary, stage, go_child)
            try:
                observation["runtime"] = daemon.ready()
                modern = go_child is None
                wrong_key_rejected(daemon, modern)
                observation["wrong_key_ciphertext_unchanged"] = True
                daemon.request("POST", "/api/sms/forwarding/configure", configure(KEY, modern))
                observation["before"] = inspect(daemon, cancelled)
                if cancel:
                    action = {"id": QUEUE_ID, "provider": cancel, "action": "cancel"}
                    if go_child is None:
                        status = daemon.request("GET", "/api/sms/forwarding/status")
                        task = next(item for item in status["tasks"] if item["id"] == QUEUE_ID)
                        channel = task[cancel]
                        action.update({
                            "client_request_id": "p6-forwarding-" + label + "-" + cancel,
                            "expected_configuration_id": status["configuration_id"],
                            "expected_attempts": channel["attempts"],
                            "expected_state": channel["state"],
                            "expected_destination_revision": channel["revision"],
                        })
                    result = daemon.request("POST", "/api/sms/forwarding/queue-action",
                                            action)
                    BASE.require(result.get("accepted") is True, "cancellation not acknowledged")
                    if go_child is None:
                        BASE.require(result.get("mutation_receipt", {}).get("outcome") == "completed",
                                     "durable cancellation receipt missing")
                    cancelled.add(cancel)
                observation["after"] = inspect(daemon, cancelled)
            finally:
                observation["exit_code"] = daemon.close()
            if go_child is None:
                BASE.require(observation["exit_code"] == 0 and
                             "switchd-rs: native shutdown complete" in daemon.log.read_text(),
                             "current Rust shutdown failed")
            else:
                log = daemon.log.read_text()
                BASE.require(observation["exit_code"] == 0 or
                             (observation["exit_code"] == 1 and
                              ("call_mode_shutdown_unconfirmed" in log or "call mode shutdown unconfirmed" in log)),
                             "unexpected legacy shutdown failure")
                observation["legacy_shutdown_issue"] = (
                    "call_mode_shutdown_unconfirmed" if observation["exit_code"] else None)
            observation["output_ciphertext_sha256"] = BASE.digest(destination)
            BASE.require(BASE.digest(state) == seed_hash, "original rollback ciphertext changed")
            previous = stage
    BASE.require(report["binaries_sha256"] ==
                 {str(binary): BASE.digest(binary) for binary in [current, legacy, child]},
                 "an executable changed during the roundtrip")
    report["original_ciphertext_unchanged"] = True
    report["cleanup"] = "all owned process groups exited"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current", type=Path, required=True)
    parser.add_argument("--legacy-backend", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    BASE.require(args.report.resolve().is_relative_to(BASE.BUILD.resolve()), "report must be under workspace build")
    report = {"passed": False, "synthetic_only": True, "providers_disabled": True,
              "scope": "synthetic encrypted queue copied-payload reverse write compatibility",
              "full_p6_acceptance": False}
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
