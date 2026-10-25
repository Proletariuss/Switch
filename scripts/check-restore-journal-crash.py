#!/usr/bin/env python3
"""Kill/restart the cfg(test) daemon restore owner at real journal boundaries.

This requires a daemon *test executable*, not a release binary. The ignored
child tests run the real Service/journal/Plan with scripted physical I/O.
Every child inherits the lifetime checker's mandatory hardware/network sandbox.
No child is run by importing this file. Root owns execution and acceptance.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import threading
import time

sys.dont_write_bytecode = True
SELF = Path(__file__).resolve()
SOURCE = SELF.parents[1]
WORKSPACE = SOURCE.parent
BUILD = WORKSPACE / "build"
SPEC = importlib.util.spec_from_file_location(
    "restore_crash_lifetime", SOURCE / "scripts/check-daemon-lifetime.py")
LIFETIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIFETIME)
require = LIFETIME.require
sha = LIFETIME.sha

WORKER = "call_mode_lifecycle::journal_process_tests::crash_worker"
OBSERVER = "call_mode_lifecycle::journal_process_tests::restart_observer"
EVENT = "SWITCH_RESTORE_JOURNAL_EVENT "
PHASES = (
    "intent", "prepared_before_io", "after_write_before_terminal",
    "second_voice", "verified_before_release", "released_before_receipt",
    "receipt_before_archive", "archive_before_publish",
)


class Child:
    def __init__(self, suite, test, root, phase, label, run_id=None):
        env = LIFETIME.BASE.environment(root)
        env.update(SWITCH_RESTORE_JOURNAL_TEST_ROOT=str(root),
                   SWITCH_RESTORE_JOURNAL_TEST_PHASE=phase)
        if run_id is not None:
            env["SWITCH_RESTORE_JOURNAL_TEST_RUN_ID"] = run_id
        self.log_path = suite.logs / (label + ".log")
        self.log = self.log_path.open("x", encoding="utf-8")
        self.events = queue.Queue()
        self.output_errors = []
        self.process = subprocess.Popen(
            ["/usr/bin/sandbox-exec", "-f", str(suite.profile), str(suite.binary),
             "--exact", test, "--ignored", "--nocapture", "--test-threads=1"],
            env=env, cwd=root, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="strict",
            start_new_session=True)
        self.record = {"label": label, "pid": self.process.pid, "test": test,
                       "phase": phase, "root": str(root), "log": str(self.log_path),
                       "intentional_sigkill": False, "forced_cleanup": False,
                       "timeout": False, "reaped": False}
        suite.report["processes"].append(self.record)
        self.reader = None
        try:
            self.reader = threading.Thread(target=self._read, daemon=True)
            self.reader.start()
        except BaseException as error:
            # Popen has already returned an owned process, but the caller
            # cannot register this Child until the constructor returns.
            self.record["startup_error"] = type(error).__name__ + ": " + str(error)
            try:
                self.cleanup()
            except BaseException as cleanup_error:
                self.record["startup_cleanup_error"] = repr(cleanup_error)
                suite.report.setdefault("cleanup_errors", []).append(
                    "child constructor: " + repr(cleanup_error))
            raise

    def _read(self):
        try:
            for line in self.process.stdout:
                self.log.write(line)
                self.log.flush()
                if EVENT in line:
                    # libtest can prefix a test name before the first write.
                    payload = json.loads(line.partition(EVENT)[2])
                    require(payload.get("pid") == self.process.pid,
                            "event does not belong to the owned child PID")
                    self.record.setdefault("event_kinds", []).append(payload.get("kind"))
                    self.events.put(payload)
        except Exception as error:
            self.output_errors.append(repr(error))

    def event(self, kind, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            require(not self.output_errors, "invalid child output: " + repr(self.output_errors))
            try:
                value = self.events.get(timeout=min(0.1, max(0.001, deadline - time.monotonic())))
            except queue.Empty:
                if self.process.poll() is not None:
                    # Exit can beat the reader publishing its flushed final
                    # event. Drain that actual pipe before deciding it is lost.
                    self.reader.join(timeout=3)
                    require(not self.reader.is_alive(), "exited child retained its log pipe")
                    try:
                        value = self.events.get_nowait()
                    except queue.Empty:
                        raise AssertionError("child exited before " + kind
                                             + "; inspect " + str(self.log_path))
                    require(value.get("kind") == kind
                            and value.get("phase") == self.record["phase"],
                            "wrong final child event: " + repr(value))
                    return value
                continue
            require(value.get("kind") == kind, "unexpected child event: " + repr(value))
            require(value.get("phase") == self.record["phase"], "wrong phase event")
            return value
        self.record["timeout"] = True
        raise AssertionError("bounded child boundary wait expired: " + str(self.log_path))

    def reap(self, expected):
        try:
            code = self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.record["timeout"] = True
            raise AssertionError("child did not terminate within the bounded reap interval")
        self.record.update(exit_code=code, reaped=True)
        self.reader.join(timeout=3)
        require(not self.reader.is_alive(), "child log pipe did not reach EOF")
        self.log.close()
        self.record["log_sha256"] = sha(self.log_path)
        require(not self.output_errors, "invalid child output: " + repr(self.output_errors))
        require(self.events.empty(), "duplicate or unconsumed child event")
        require(code == expected, "unexpected child exit: " + repr(self.record))
        try:
            os.killpg(self.process.pid, 0)
        except ProcessLookupError:
            self.record["owned_process_group_gone"] = True
            return
        self.record["surviving_owned_descendants"] = True
        raise AssertionError("owned process group survived the reaped child")

    def crash(self):
        require(self.process.poll() is None, "child exited before deliberate crash")
        self.record["intentional_sigkill"] = True
        # This PID came only from this Popen. Never signal a discovered process.
        self.process.send_signal(signal.SIGKILL)
        self.reap(-signal.SIGKILL)

    def cleanup(self):
        if self.process.poll() is None:
            self.record["forced_cleanup"] = True
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait(timeout=5)
            self.record.update(exit_code=self.process.returncode, reaped=True)
        elif not self.record["reaped"]:
            # poll() may already have observed/reaped a failed child. Still
            # account for its terminal status; never leave cleanup ambiguous.
            code = self.process.wait(timeout=1)
            self.record.update(exit_code=code, reaped=True)
        reader = getattr(self, "reader", None)
        if reader is not None and reader.is_alive():
            reader.join(timeout=3)
        if self.process.stdout is not None and (reader is None or not reader.is_alive()):
            self.process.stdout.close()
        if not self.log.closed:
            self.log.close()
        self.record["log_sha256"] = sha(self.log_path)
        try:
            os.killpg(self.process.pid, 0)
        except ProcessLookupError:
            self.record["owned_process_group_gone"] = True
            return
        self.record.update(forced_cleanup=True, surviving_owned_descendants=True)
        try:
            os.killpg(self.process.pid, signal.SIGKILL)
        except ProcessLookupError:
            self.record["owned_process_group_gone"] = True
            return
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                os.killpg(self.process.pid, 0)
            except ProcessLookupError:
                self.record["owned_process_group_gone"] = True
                return
            time.sleep(0.02)
        raise AssertionError("owned process group survived cleanup")


def boundary(value, phase):
    require(value.get("source") == "production_restore_journal", "wrong journal source")
    require(isinstance(value.get("run_id"), str) and value["run_id"], "missing real run ID")
    for field in ("sequence", "prepared_steps", "unknown_steps", "physical_writes"):
        require(type(value.get(field)) is int and value[field] >= 0, "invalid " + field)
    require(value["sequence"] > 0, "boundary lacks a durable record sequence")
    require(value.get("success_published") is False, "success publication preceded the crash boundary")
    if phase != "archive_before_publish":
        require(value.get("active_present") is True, "active journal disappeared early")
    if phase == "intent":
        require(value["prepared_steps"] == value["physical_writes"] == 0,
                "intent was not before preparation and physical writes")
    if phase == "prepared_before_io":
        require(value["prepared_steps"] >= 1 and value["physical_writes"] == 0,
                "prepared boundary did not precede physical writes")
    if phase == "after_write_before_terminal":
        require(value["prepared_steps"] >= 1 and value["unknown_steps"] >= 1
                and value["physical_writes"] >= 1,
                "written payload lacks its outstanding durable preparation")
    if phase == "second_voice":
        require(value["prepared_steps"] >= 2 and value["physical_writes"] >= 1,
                "second voice boundary did not retain the prior step")
    if phase == "verified_before_release":
        require(value.get("verified") is True and value.get("released") is False,
                "verified boundary does not precede actual release")
    if phase == "released_before_receipt":
        require(value.get("released") is True and value.get("receipt_present") is False,
                "release boundary is not before the durable receipt")
    if phase == "receipt_before_archive":
        require(value.get("released") is True and value.get("receipt_present") is True
                and value.get("archived") is False, "invalid receipt-before-archive boundary")
    if phase == "archive_before_publish":
        require(value.get("released") is True and value.get("receipt_present") is True
                and value.get("archived") is True and value.get("active_present") is False,
                "archive boundary is not a completed durable move")


def inventory(suite, report, helpers, label="test-inventory"):
    """Own and reap each inventory child exactly like the worker children."""
    command = ["/usr/bin/sandbox-exec", "-f", str(suite.profile), str(suite.binary),
               "--list", "--ignored", "--format", "terse"]
    process = subprocess.Popen(command, env=LIFETIME.BASE.environment(suite.path), cwd=suite.path,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                               start_new_session=True)
    row = dict(pid=process.pid, command=command, reaped=False, forced_cleanup=False, timeout=False)
    report["inventory_process"] = row
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
    path = suite.logs / (label + ".log")
    path.write_text(raw)
    row.update(log=str(path), log_sha256=sha(path))
    report.update(test_inventory_log=str(path), test_inventory_log_sha256=sha(path))
    require(process.returncode == 0 and row.get("owned_process_group_gone") is True
            and not row["timeout"] and not row["forced_cleanup"], "test inventory process failed")
    require(all(name + ": test" in raw.splitlines() for name in helpers),
            "both actual ignored child selectors required")


def run(suite, report):
    # A release daemon cannot satisfy this ignored libtest inventory contract.
    inventory(suite, report, (WORKER, OBSERVER))
    children = []
    try:
        for number, phase in enumerate(PHASES):
            root = suite.path / ("r" + str(number))
            root.mkdir(mode=0o700)
            row = {"phase": phase, "passed": False}
            report["scenarios"].append(row)
            child = Child(suite, WORKER, root, phase, str(number) + "-crash")
            children.append(child)
            value = child.event("boundary")
            boundary(value, phase)
            row["boundary"] = value
            child.crash()
            restart = Child(suite, OBSERVER, root, phase, str(number) + "-restart", value["run_id"])
            children.append(restart)
            observed = restart.event("restart")
            require(observed.get("ordinary_admission_denied") is True,
                    "restart admitted ordinary module operations")
            require(observed.get("historical_recovery_confirmed") is False,
                    "restart invented historical recovery")
            require(observed.get("physical_writes") == 0 and observed.get("factory_calls") == 0,
                    "restart replayed/opened a physical device")
            require(observed.get("evidence_unchanged") is True,
                    "read-only restart changed journal or old legacy evidence")
            require(observed.get("run_id") == value["run_id"]
                    and observed.get("sequence") == value["sequence"],
                    "restart did not inspect the same durable journal lineage")
            reasons = observed.get("reason_codes", [])
            require(isinstance(reasons, list), "missing startup reason codes")
            if phase == "archive_before_publish":
                require("restore_journal_pending" not in reasons
                        and observed.get("archived") is True,
                        "archived operation was treated as an active journal")
            else:
                require("restore_journal_pending" in reasons,
                        "unfinished production journal did not quarantine startup")
            restart.reap(0)
            row.update(restart=observed, passed=True)
    finally:
        for child in reversed(children):
            child.cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test-binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    report = {"passed": False, "validation_completed": False, "synthetic_only": True,
              "full_recovery_acceptance": False, "processes": [], "scenarios": [],
              "overall_stage": "P0–P6未完成；持久恢复所有者接线验收中"}
    suite = None
    destination = args.report.absolute()
    reports = (BUILD / "reports").resolve()
    require(not destination.exists(), "refusing to overwrite an earlier report")
    require(not destination.is_symlink() and destination.resolve().is_relative_to(reports),
            "report must remain under this workspace build/reports")
    require(destination.suffix == ".json", "report extension must be .json")
    try:
        require(sys.platform == "darwin", "mandatory Darwin sandbox required")
        binary = args.test_binary.resolve(strict=True)
        require(binary.is_relative_to(WORKSPACE) and binary.is_file() and os.access(binary, os.X_OK),
                "test executable must remain in the Switch workspace")
        report.update(binary=str(binary), binary_sha256=sha(binary), driver_sha256=sha(SELF))
        suite = LIFETIME.Suite(binary, report)
        suite.canary()
        run(suite, report)
        require(sha(binary) == report["binary_sha256"], "test executable changed during run")
        report["validation_completed"] = True
    except Exception as error:
        report["error"] = type(error).__name__ + ": " + str(error)
    finally:
        if suite is not None:
            suite.cleanup_canary()
            report["synthetic_evidence_retained"] = str(suite.path)
        bad = any(p.get("forced_cleanup") or p.get("timeout") or not p.get("reaped")
                  or p.get("surviving_owned_descendants") for p in report["processes"])
        report["passed"] = (report["validation_completed"] and not report.get("error")
                            and not bad and len(report["scenarios"]) == len(PHASES)
                            and all(row["passed"] for row in report["scenarios"]))
        destination.parent.mkdir(parents=True, exist_ok=True)
        with destination.open("x") as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write("\n")
    print(json.dumps({"passed": report["passed"], "report": str(destination),
                      "scenario_count": len(report["scenarios"]), "error": report.get("error")},
                     ensure_ascii=False))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
