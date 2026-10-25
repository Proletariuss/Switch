#!/usr/bin/env python3
"""Opt-in real-daemon log backpressure check in the mandatory P6 sandbox.

Synthetic data only. A timeout/forced cleanup is a failure, never a successful
shutdown receipt. This does not prove any physical device or carrier behavior.
"""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time

SOURCE = Path(__file__).resolve().parents[1]
BUILD = SOURCE.parent / "build"
SPEC = importlib.util.spec_from_file_location("p546_log_check", SOURCE / "scripts/check-p6-data-roundtrip.py")
BASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BASE)


def exercise(binary, parent, mode):
    data = parent / mode
    data.mkdir(mode=0o700)
    # Quarantine module history, rather than asserting unavailable hardware was
    # restored. Reader/store startup and their actual exit owners still run.
    (data / "call-mode-operation.lock").write_text("synthetic unknown legacy history")
    future = '{"large":18446744073709551617, "precise":0.12345678901234567890123456789}'
    (data / "profile-notes.json").write_text(
        '{"8901":{"label":"before","phone":"","tags":"","future":' + future + '}}')
    profile = data / "hardware-denied.sb"
    profile.write_text(BASE.sandbox_profile(data))
    blocked = parent / (mode + "-blocked")
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(str(blocked))
        listener.listen(1)
        BASE.validate_sandbox(data, profile, blocked)
    read_fd, write_fd = os.pipe()
    filled = 0
    if mode == "full":
        flags = fcntl.fcntl(write_fd, fcntl.F_GETFL)
        fcntl.fcntl(write_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
        while True:
            try:
                filled += os.write(write_fd, b"x" * 4096)
            except BlockingIOError:
                break
        fcntl.fcntl(write_fd, fcntl.F_SETFL, flags)
    if mode == "closed":
        os.close(read_fd)
        read_fd = None
    process = None
    observation = {"mode": mode, "sandbox_canary_and_child_inheritance": "passed",
                   "prefilled_bytes": filled, "passed": False}
    log = data / "normal-stderr.log"
    try:
        with (data / "stdout.log").open("wb") as output, log.open("wb") as errors:
            process = subprocess.Popen(
                ["/usr/bin/sandbox-exec", "-f", str(profile), str(binary),
                 "-port", str(data / "absent-serial"), "-listen", "unix:" + str(data / "s")],
                cwd=data, env=BASE.environment(data), stdin=subprocess.DEVNULL,
                stdout=output, stderr=errors if mode == "available" else write_fd,
                start_new_session=True)
        daemon = BASE.Daemon.__new__(BASE.Daemon)
        daemon.root, daemon.process, daemon.legacy, daemon.log = data, process, None, log
        observation["runtime"] = daemon.ready()
        # The API stays responsive while the real logging thread is blocked.
        health = daemon.request("GET", "/api/health")
        BASE.require(health.get("ok") is True, "healthy API unavailable with stalled logging")
        for label in ("edited", ""):
            note = {"label": label, "phone": "", "tags": ""}
            saved = daemon.request("PUT", "/api/esim/notes", {"iccid": "8901", **note})
            BASE.require(saved.get("note") == note, "public note response changed or leaked extensions")
            BASE.require(daemon.request("GET", "/api/esim/notes")["notes"]["8901"] == note,
                         "public note snapshot changed or leaked extensions")
            BASE.require(future in (data / "profile-notes.json").read_text(), "opaque note numbers changed")
        observation["note_raw_extensions_and_dto_preserved"] = True
        started = time.monotonic()
        process.send_signal(signal.SIGTERM)
        observation["exit_code"] = process.wait(timeout=10)
        observation["shutdown_seconds"] = round(time.monotonic() - started, 4)
        BASE.require(observation["exit_code"] == 0, "real daemon failed normal owner shutdown")
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            pass
        else:
            os.killpg(process.pid, signal.SIGKILL)
            raise AssertionError("owned child survived daemon exit")
        observation["sockets_remaining"] = [name for name in ("s", "s.rs", "s.in", "s.go") if (data / name).exists()]
        BASE.require(not observation["sockets_remaining"], "daemon IPC was not retired")
        if mode == "available":
            observation["native_shutdown_log_receipt"] = "switchd-rs: native shutdown complete" in log.read_text()
            BASE.require(observation["native_shutdown_log_receipt"], "available log lost final receipt")
        if mode == "full":
            # Read only after exit. The original sentinel prefix proves this
            # was an undrained, open pipe rather than an early reader close.
            BASE.require(os.read(read_fd, filled) == b"x" * filled, "pipe sentinel was drained or changed")
            observation["sentinel_undrained_until_after_exit"] = True
        observation["passed"] = True
        return observation
    finally:
        if process and process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
        os.close(write_fd)
        if read_fd is not None:
            os.close(read_fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    BASE.require(binary.is_file() and binary.is_relative_to(SOURCE.parent), "binary must be inside the Switch workspace")
    (BUILD / "runtime").mkdir(parents=True, exist_ok=True)
    (BUILD / "reports").mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="log564-", dir=BUILD / "runtime")).resolve()
    report = {"binary": str(binary), "binary_sha256": BASE.digest(binary),
              "synthetic_only": True, "no_hardware_or_external_network": True,
              "passed": False, "observations": []}
    try:
        for mode in ("available", "full", "closed"):
            try:
                report["observations"].append(exercise(binary, root, mode))
            except Exception as error:
                report["observations"].append({"mode": mode, "passed": False, "error": str(error)})
                raise
        report["passed"] = True
    finally:
        shutil.rmtree(root)
        report["runtime_removed"] = True
        (BUILD / "reports/iteration-564-daemon-log-backpressure.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
