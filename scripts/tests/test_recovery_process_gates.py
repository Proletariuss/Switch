#!/usr/bin/env python3
"""Negative gate receipts; no builds or daemon workers, one harmless ownership fixture."""
import copy
import importlib.util
import json
import io
import queue
from types import SimpleNamespace
import os
from pathlib import Path
import signal
import shutil
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "check-recovery-process-gates.py"
SPEC = importlib.util.spec_from_file_location("recovery_process_gate_tests", SCRIPT)
CHECKER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECKER
SPEC.loader.exec_module(CHECKER)


class RecoveryGateTests(unittest.TestCase):
    def setUp(self):
        reports = CHECKER.BUILD / "reports"
        reports.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="recovery-gate-unit-", dir=reports)
        self.root = Path(self.temporary.name).resolve()
        self.binary = self.root / "exact-cargo-test-artifact"
        self.binary.write_bytes(b"synthetic executable identity only")
        self.binary.chmod(0o700)
        self.log = self.root / "child.log"
        self.log.write_text("synthetic receipt log\n")
        self.log_fields = {"log": str(self.log), "log_sha256": CHECKER.sha(self.log)}
        self.artifact = {
            "reason": "compiler-artifact",
            "manifest_path": str(CHECKER.SOURCE / "rust/crates/proletariusd/Cargo.toml"),
            "target": {"name": "switchd-rs", "kind": ["bin"],
                       "src_path": str(CHECKER.SOURCE / "rust/crates/proletariusd/src/main.rs")},
            "profile": {"test": True}, "executable": str(self.binary),
        }

    def tearDown(self):
        self.temporary.cleanup()

    def cargo_output(self, artifacts=None, success=True):
        rows = [self.artifact] if artifacts is None else artifacts
        return "\n".join(json.dumps(row) for row in
                         [*rows, {"reason": "build-finished", "success": success}])

    def driver(self, name):
        return CHECKER.load("unit_driver_" + name, "check-restore-journal-crash.py"
                            if name == "restore" else f"recovery_process_{name}.py")

    def receipt(self, name="restore"):
        driver = self.driver(name)
        expected = CHECKER.expected_cases(name, driver)
        report = {"validation_completed": True, "mode": "full", "synthetic_only": True,
                  "hardware_acceptance": False, "full_p6_acceptance": False,
                  "overall_product_complete": False,
                  "sandbox_canary": {"exit_code": 0, "inherited_child": True, "reaped": True,
                                     "owned_process_group_gone": True, **self.log_fields},
                  "scenarios": [{**case, "passed": True,
                                 **({"case_index": index} if name == "publication" else {})}
                                for index, case in enumerate(expected)], "processes": []}
        for index, case in enumerate(expected):
            for worker in (True, False):
                code = (-signal.SIGKILL if name == "restore" else 86) if worker else 0
                if name == "finite":
                    code = 0
                row = {**case, "test": driver.WORKER if worker else driver.OBSERVER,
                       "pid": 1000 + 2 * index + int(worker), "exit_code": code,
                       "reaped": True, "owned_process_group_gone": True,
                       "intentional_sigkill": name == "restore" and worker, **self.log_fields}
                if name == "publication":
                    row["case_index"] = index
                    if worker:
                        row["negative"] = ""
                row["event_kinds"] = (["worker_complete"] if worker else ["observer_complete"]) if name == "finite" else (
                    ["boundary"] if worker and name == "restore" else
                    ["ready", "boundary"] if worker else ["restart"])
                if name == "finite":
                    row["libtest"] = {"passed": 1, "failed": 0, "ignored": 0}
                report["processes"].append(row)
        if name in CHECKER.SUITES:
            report["inventory_process"] = {"exit_code": 0, "reaped": True,
                                           "owned_process_group_gone": True, **self.log_fields}
        if name == "restore":
            report["ignored_daemon_helpers"] = sorted(CHECKER.required_helpers() | {"call_mode_adb::child_tests::real_child_fixture"})
            report["ignored_inventory_process"] = {"exit_code": 0, "reaped": True,
                                                   "owned_process_group_gone": True, **self.log_fields}
        return report, driver

    def test_cargo_selects_exact_reported_target_and_rejects_ambiguity(self):
        self.assertEqual(CHECKER.select_test_binary(self.cargo_output()), self.binary)
        for rows in ([], [self.artifact, self.artifact]):
            with self.assertRaises(ValueError):
                CHECKER.select_test_binary(self.cargo_output(rows))

    def test_cargo_rejects_failed_build_wrong_kind_source_and_non_test(self):
        with self.assertRaises(ValueError):
            CHECKER.select_test_binary(self.cargo_output(success=False))
        for mutate in (lambda row: row["target"].update(kind=["lib"]),
                       lambda row: row["target"].update(src_path="/other/main.rs"),
                       lambda row: row["profile"].update(test=False)):
            row = copy.deepcopy(self.artifact)
            mutate(row)
            with self.assertRaises(ValueError):
                CHECKER.select_test_binary(self.cargo_output([row]))

    def test_cargo_rejects_symlink_and_executable_outside_build(self):
        alias = self.root / "alias"
        alias.symlink_to(self.binary)
        row = copy.deepcopy(self.artifact)
        row["executable"] = str(alias)
        with self.assertRaises(ValueError):
            CHECKER.select_test_binary(self.cargo_output([row]))
        with self.assertRaises(ValueError):
            CHECKER.select_test_binary(self.cargo_output(), build=self.root / "other-build")

    def test_all_four_exact_matrices_accept_complete_receipts(self):
        self.assertEqual(sum(len(CHECKER.expected_cases(name, self.driver(name)))
                             for name in CHECKER.SUITES), 168)
        for name in CHECKER.SUITES:
            report, driver = self.receipt(name)
            CHECKER.validate_suite(name, report, driver)

    def test_missing_reordered_or_failed_scenario_is_rejected(self):
        for mutation in (lambda rows: rows.pop(), lambda rows: rows.reverse(),
                         lambda rows: rows[0].update(passed=False)):
            report, driver = self.receipt()
            mutation(report["scenarios"])
            with self.assertRaises(ValueError):
                CHECKER.validate_suite("restore", report, driver)

    def test_added_or_missing_ignored_helper_cannot_silently_skip(self):
        for replacement in ("new::uncovered_software_test", None):
            report, driver = self.receipt()
            report["ignored_daemon_helpers"].pop()
            if replacement:
                report["ignored_daemon_helpers"].append(replacement)
            with self.assertRaises(ValueError):
                CHECKER.validate_suite("restore", report, driver)

    def test_exact_nine_inventory_includes_all_eight_recovery_helpers(self):
        report, driver = self.receipt()
        report["ignored_daemon_helpers"].reverse()
        CHECKER.validate_suite("restore", report, driver)
        self.assertEqual(len(CHECKER.required_helpers()), 8)
        self.assertEqual(len(report["scenarios"]), 8)
        self.assertEqual(len(report["processes"]), 16)

    def test_inventory_collection_accepts_exact_known_fixture_without_running_it(self):
        names = sorted(CHECKER.required_helpers()) + ["call_mode_adb::child_tests::real_child_fixture"]
        raw = "".join(name + ": test\n" for name in names)
        process = SimpleNamespace(pid=123456789, returncode=0,
                                  communicate=mock.Mock(return_value=(raw, None)))
        suite = SimpleNamespace(profile=self.root / "unused-profile", binary=self.binary,
                                path=self.root, logs=self.root)
        crash = SimpleNamespace(LIFETIME=SimpleNamespace(BASE=SimpleNamespace(
            environment=lambda path: {})))
        report = {}
        with mock.patch.object(CHECKER.subprocess, "Popen", return_value=process) as popen, \
                mock.patch.object(CHECKER.os, "killpg", side_effect=ProcessLookupError):
            CHECKER.ignored_inventory(suite, report, crash)
        self.assertEqual(popen.call_args.args[0][-4:], ["--list", "--ignored", "--format", "terse"])
        self.assertTrue(popen.call_args.kwargs["start_new_session"])
        process.communicate.assert_called_once_with(timeout=20)
        self.assertEqual(report["ignored_daemon_helpers"], names)
        self.assertTrue(report["ignored_inventory_process"]["owned_process_group_gone"])

    def test_retained_inventory_rejects_missing_known_non_recovery_fixture(self):
        report, driver = self.receipt()
        report["ignored_daemon_helpers"].remove("call_mode_adb::child_tests::real_child_fixture")
        self.assertEqual(set(report["ignored_daemon_helpers"]), CHECKER.required_helpers())
        with self.assertRaises(ValueError):
            CHECKER.validate_suite("restore", report, driver)

    def test_inventory_collection_rejects_missing_known_non_recovery_fixture(self):
        names = sorted(CHECKER.required_helpers())
        raw = "".join(name + ": test\n" for name in names)
        process = SimpleNamespace(pid=123456789, returncode=0,
                                  communicate=mock.Mock(return_value=(raw, None)))
        suite = SimpleNamespace(profile=self.root / "unused-profile", binary=self.binary,
                                path=self.root, logs=self.root)
        crash = SimpleNamespace(LIFETIME=SimpleNamespace(BASE=SimpleNamespace(
            environment=lambda path: {})))
        with mock.patch.object(CHECKER.subprocess, "Popen", return_value=process) as popen, \
                mock.patch.object(CHECKER.os, "killpg", side_effect=ProcessLookupError):
            with self.assertRaises(ValueError):
                CHECKER.ignored_inventory(suite, {}, crash)
        self.assertEqual(popen.call_args.args[0][-4:], ["--list", "--ignored", "--format", "terse"])
        process.communicate.assert_called_once_with(timeout=20)

    def test_known_fixture_cannot_hide_missing_duplicate_or_unknown_ignored_names(self):
        required = sorted(CHECKER.required_helpers())
        known = "call_mode_adb::child_tests::real_child_fixture"
        invalid = [required[1:] + [known], required + [known, known],
                   required + [required[0]], required + ["new::uncovered_software_test"],
                   required + [known, "new::uncovered_software_test"],
                   required + [known + "_other"], required + [True], required + [[known]]]
        for names in invalid:
            report, driver = self.receipt()
            report["ignored_daemon_helpers"] = names
            with self.subTest(names=names), self.assertRaises(ValueError):
                CHECKER.validate_suite("restore", report, driver)

    def test_publication_all_negative_observers_require_normal_worker_baselines(self):
        report, driver = self.receipt("publication")
        negatives = [case for case in CHECKER.expected_cases("publication", driver) if case["negative"]]
        self.assertEqual(len(negatives), 8)
        self.assertEqual({case["negative"] for case in negatives}, set(driver.NEGATIVES))
        self.assertEqual(len(report["scenarios"]), 42)
        self.assertEqual(len(report["processes"]), 84)
        CHECKER.validate_suite("publication", report, driver)
        for number, case in enumerate(CHECKER.expected_cases("publication", driver)):
            worker, observer = report["processes"][2 * number:2 * number + 2]
            self.assertEqual(worker["negative"], "")
            self.assertEqual(observer["negative"], case["negative"])
            if case["negative"]:
                changed = copy.deepcopy(report)
                changed["processes"][2 * number]["negative"] = case["negative"]
                with self.subTest(wrong_worker_negative=case["negative"]), self.assertRaises(ValueError):
                    CHECKER.validate_suite("publication", changed, driver)

    def test_publication_missing_duplicate_or_wrong_negative_observer_is_rejected(self):
        report, driver = self.receipt("publication")
        indices = [index for index, row in enumerate(report["processes"])
                   if row["test"] == driver.OBSERVER and row["negative"]]
        self.assertEqual(len(indices), 8)
        for index in indices:
            for replacement in ("", "journal:unreviewed-negative"):
                changed = copy.deepcopy(report)
                changed["processes"][index]["negative"] = replacement
                with self.subTest(index=index, replacement=replacement), self.assertRaises(ValueError):
                    CHECKER.validate_suite("publication", changed, driver)
            changed = copy.deepcopy(report)
            changed["processes"].pop(index)
            with self.subTest(missing_observer=index), self.assertRaises(ValueError):
                CHECKER.validate_suite("publication", changed, driver)
        for first, second in zip(indices, indices[1:]):
            changed = copy.deepcopy(report)
            changed["processes"][second] = copy.deepcopy(changed["processes"][first])
            with self.subTest(duplicate_observer=(first, second)), self.assertRaises(ValueError):
                CHECKER.validate_suite("publication", changed, driver)

    def test_publication_duplicate_same_phase_worker_cannot_replace_missing_case(self):
        report, driver = self.receipt("publication")
        first_negative = next(index for index, row in enumerate(report["scenarios"]) if row["negative"])
        self.assertEqual(first_negative, 34)
        original_worker, negative_worker = report["processes"][0], report["processes"][2 * first_negative]
        self.assertEqual(original_worker["phase"], negative_worker["phase"])
        self.assertEqual(original_worker["negative"], negative_worker["negative"])
        self.assertNotEqual(original_worker["case_index"], negative_worker["case_index"])
        changed = copy.deepcopy(report)
        # This is the independent review's actual case0 -> case34 receipt
        # replacement: the same-phase baseline must not conceal a missing owner.
        changed["processes"][2 * first_negative] = copy.deepcopy(changed["processes"][0])
        with self.assertRaises(ValueError):
            CHECKER.validate_suite("publication", changed, driver)

    def test_publication_child_and_scenario_indices_are_canonical_integers(self):
        report, driver = self.receipt("publication")
        for index in (68, 69):
            for value in (None, True, 34.0, -1, 42, 0, 35):
                changed = copy.deepcopy(report)
                changed["processes"][index]["case_index"] = value
                with self.subTest(process=index, value=value), self.assertRaises(ValueError):
                    CHECKER.validate_suite("publication", changed, driver)
            changed = copy.deepcopy(report)
            del changed["processes"][index]["case_index"]
            with self.subTest(missing_index=index), self.assertRaises(ValueError):
                CHECKER.validate_suite("publication", changed, driver)
        for value in (None, True, 34.0, 0, 35):
            changed = copy.deepcopy(report)
            changed["scenarios"][34]["case_index"] = value
            with self.subTest(scenario_index=value), self.assertRaises(ValueError):
                CHECKER.validate_suite("publication", changed, driver)
        changed = copy.deepcopy(report)
        changed["processes"][68]["phase"] = "voice-control:before-prepare"
        with self.assertRaises(ValueError):
            CHECKER.validate_suite("publication", changed, driver)

    def test_duplicate_child_cannot_replace_required_observer(self):
        report, driver = self.receipt("backup")
        report["processes"][1] = report["processes"][0].copy()
        with self.assertRaises(ValueError):
            CHECKER.validate_suite("backup", report, driver)

    def test_sandbox_and_child_cleanup_failures_are_rejected(self):
        for key, value in (("exit_code", 1), ("inherited_child", False), ("forced_cleanup", True),
                           ("reaped", False), ("owned_process_group_gone", False), ("timeout", True)):
            report, driver = self.receipt()
            report["sandbox_canary"][key] = value
            with self.assertRaises(ValueError):
                CHECKER.validate_suite("restore", report, driver)
        for key, value in (("exit_code", 0), ("reaped", False),
                           ("owned_process_group_gone", False), ("timeout", True)):
            report, driver = self.receipt()
            report["processes"][0][key] = value
            with self.assertRaises(ValueError):
                CHECKER.validate_suite("restore", report, driver)

    def test_changed_log_and_skipped_finite_child_are_rejected(self):
        report, driver = self.receipt("finite")
        report["processes"][0]["libtest"]["ignored"] = 1
        with self.assertRaises(ValueError):
            CHECKER.validate_suite("finite", report, driver)
        report, driver = self.receipt()
        self.log.write_text("changed after receipt\n")
        with self.assertRaises(ValueError):
            CHECKER.validate_suite("restore", report, driver)

    def test_short_runtime_requires_owned_private_directory(self):
        lifetime = self.driver("restore").LIFETIME
        for mode in (0o755, 0o777):
            self.root.chmod(mode)
            with self.assertRaises(AssertionError):
                lifetime.Suite(self.binary, {}, runtime_parent=self.root)
        self.root.chmod(0o700)
        alias = self.root / "runtime-alias"
        alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(AssertionError):
            lifetime.Suite(self.binary, {}, runtime_parent=alias)
        suite = lifetime.Suite(self.binary, {}, runtime_parent=self.root)
        self.assertTrue(suite.canary_root.is_relative_to(self.root))
        self.assertTrue(suite.path.is_relative_to(lifetime.BUILD / "runtime"))
        self.assertEqual(suite.runtime_parent, self.root)
        shutil.rmtree(suite.path)
        shutil.rmtree(suite.canary_root)


    def full_fixture(self):
        capsule = self.root / "capsule"
        build = capsule / "build"
        reports = build / "reports"
        reports.mkdir(parents=True)
        original = build / "rust-target" / "exact-cargo-bin"
        original.parent.mkdir()
        original.write_bytes(self.binary.read_bytes())
        original.chmod(0o700)
        digest = "a" * 64
        binary = CHECKER.retain_test_binary(original, digest, build)
        artifact = copy.deepcopy(self.artifact)
        artifact["executable"] = str(original)
        artifact["manifest_path"] = str(capsule / "src/rust/crates/proletariusd/Cargo.toml")
        artifact["target"]["src_path"] = str(capsule / "src/rust/crates/proletariusd/src/main.rs")
        discovery = reports / "cargo-artifacts.jsonl"
        discovery.write_text(self.cargo_output([artifact]))
        summary = dict(mode="full", passed=True, source_digest=digest, binary=str(binary),
                       binary_sha256=CHECKER.sha(binary), scenario_count=168, child_count=336,
                       synthetic_only=True, hardware_acceptance=False, full_p6_acceptance=False,
                       overall_product_complete=False, suites=[],
                       cargo_discovered_binary=str(original),
                       cargo_discovered_binary_sha256=CHECKER.sha(original),
                       cargo_artifact_log=str(discovery), cargo_artifact_log_sha256=CHECKER.sha(discovery))
        retained_log = reports / "retained-unit-log"
        retained_log.write_bytes(self.log.read_bytes())
        for name in CHECKER.SUITES:
            receipt, driver = self.receipt(name)
            evidence_rows = [receipt["sandbox_canary"], receipt["inventory_process"], *receipt["processes"]]
            if name == "restore":
                evidence_rows.append(receipt["ignored_inventory_process"])
            for row in evidence_rows:
                row.update(retained_log=str(retained_log), retained_log_sha256=CHECKER.sha(retained_log))
            receipt.update(passed=True, source_digest=digest, binary=str(binary),
                           binary_sha256=CHECKER.sha(binary), driver_sha256=CHECKER.sha(Path(driver.__file__)))
            path = reports / (name + ".json")
            path.write_text(json.dumps(receipt))
            summary["suites"].append(dict(suite=name, path=str(path), sha256=CHECKER.sha(path),
                                         scenario_count=len(receipt["scenarios"]),
                                         child_count=len(receipt["processes"])))
        path = reports / "summary.json"
        path.write_text(json.dumps(summary))
        marker = {key: summary[key] for key in ("mode", "passed", "source_digest", "binary_sha256",
                                               "scenario_count", "child_count")}
        marker.update(report=str(path), sha256=CHECKER.sha(path))
        log = CHECKER.MARKER + json.dumps(marker) + "\n"
        return capsule, summary, marker, log

    def test_full_marker_and_both_acceptance_consumers_require_complete_evidence(self):
        capsule, summary, marker, log = self.full_fixture()
        digest = summary["source_digest"]
        bound = CHECKER.verify_full_marker(log, digest, reports_root=capsule / "build/reports")
        execution = {"recovery_process_gate": bound}
        artifact = {"recovery_process_gate": bound}
        (capsule / "build.log").write_text(log)
        auditor = CHECKER.load("unit_recovery_auditor", "audit-clean-build.py")
        attestation = CHECKER.load("unit_recovery_attestation", "check-no-device-software-acceptance.py")
        self.assertEqual(auditor.verify_recovery_evidence(capsule, digest, execution), bound)
        attestation.verify_recovery_evidence(log, digest, capsule, execution, artifact)
        for invalid in ("", log + log,
                        CHECKER.MARKER + json.dumps({**marker, "mode": "smoke"}),
                        CHECKER.MARKER + json.dumps({**marker, "scenario_count": 167}),
                        CHECKER.MARKER + json.dumps({**marker, "child_count": 335}),
                        CHECKER.MARKER + json.dumps({**marker, "source_digest": "b" * 64}),
                        CHECKER.MARKER + json.dumps({**marker, "sha256": "0" * 64}),
                        CHECKER.MARKER + json.dumps({**marker, "binary_sha256": "0" * 64})):
            (capsule / "build.log").write_text(invalid)
            with self.subTest(marker=invalid[:90]):
                with self.assertRaises(ValueError):
                    auditor.verify_recovery_evidence(capsule, digest, execution)
                with self.assertRaises(ValueError):
                    attestation.verify_recovery_evidence(invalid, digest, capsule, execution, artifact)
        (capsule / "build.log").write_text(log)
        with self.assertRaises(ValueError):
            auditor.verify_recovery_evidence(capsule, digest, {})
        with self.assertRaises(ValueError):
            attestation.verify_recovery_evidence(log, digest, capsule, execution, {})

    def test_retained_copy_survives_target_cleanup_and_rejects_hash_path_symlink_drift(self):
        capsule, summary, marker, log = self.full_fixture()
        reports = capsule / "build/reports"
        binary = Path(summary["binary"])
        original = Path(summary["cargo_discovered_binary"])
        before = binary.stat()
        self.assertEqual(CHECKER.retain_test_binary(original, summary["source_digest"], capsule / "build"), binary)
        self.assertEqual(before.st_mtime_ns, binary.stat().st_mtime_ns)
        self.assertEqual(before.st_mode, binary.stat().st_mode)
        original.unlink()
        CHECKER.verify_full_marker(log, summary["source_digest"], reports_root=reports)
        alternate = binary.with_name("other")
        alternate.write_bytes(binary.read_bytes())
        alternate.chmod(0o500)
        with self.assertRaises(ValueError):
            CHECKER.verify_retained_binary({**summary, "binary": str(alternate)}, summary["source_digest"], reports)
        binary.unlink()
        binary.symlink_to(alternate)
        with self.assertRaises(ValueError):
            CHECKER.verify_full_marker(log, summary["source_digest"], reports_root=reports)
        binary.unlink()
        binary.write_bytes(b"changed retained bytes")
        binary.chmod(0o500)
        with self.assertRaises(ValueError):
            CHECKER.verify_full_marker(log, summary["source_digest"], reports_root=reports)

    def test_copy_rejects_existing_mismatch_symlink_and_hardlink_without_overwrite(self):
        for case in ("mismatch", "symlink", "hardlink", "unsafe-mode"):
            build = self.root / case
            build.mkdir()
            original = build / "exact-original"
            original.write_bytes(self.binary.read_bytes())
            original.chmod(0o700)
            destination = CHECKER.retain_test_binary(original, "c" * 64, build)
            if case == "mismatch":
                destination.chmod(0o700)
                destination.write_bytes(b"old evidence must remain")
                destination.chmod(0o500)
            elif case == "symlink":
                destination.unlink()
                destination.symlink_to(self.binary)
            elif case == "hardlink":
                os.link(destination, destination.with_name("hardlink"))
            else:
                destination.chmod(0o777)
            prior = destination.read_bytes()
            with self.assertRaises(ValueError):
                CHECKER.retain_test_binary(original, "c" * 64, build)
            self.assertEqual(destination.read_bytes(), prior)

    def test_summary_drift_and_parent_event_count_are_rejected(self):
        capsule, summary, marker, log = self.full_fixture()
        reports = capsule / "build/reports"
        summary["suites"][1]["scenario_count"] -= 1
        path = Path(marker["report"])
        path.write_text(json.dumps(summary))
        marker["sha256"] = CHECKER.sha(path)
        with self.assertRaises(ValueError):
            CHECKER.verify_full_marker(CHECKER.MARKER + json.dumps(marker), summary["source_digest"], reports_root=reports)
        for name in CHECKER.SUITES:
            report, driver = self.receipt(name)
            report["processes"].pop()
            with self.assertRaises(ValueError):
                CHECKER.validate_suite(name, report, driver)
            report, driver = self.receipt(name)
            report["processes"][0]["event_kinds"].append("unexpected-extra")
            with self.assertRaises(ValueError):
                CHECKER.validate_suite(name, report, driver)

    def test_thread_start_failure_reaps_actual_owned_child_and_preserves_exception(self):
        crash = self.driver("restore")
        real_popen, real_cleanup = crash.subprocess.Popen, crash.Child.cleanup
        launched = []

        def harmless_owned_process(command, **kwargs):
            self.assertEqual(command[:1], ["/usr/bin/sandbox-exec"])
            self.assertTrue(kwargs["start_new_session"])
            # Exercise real Popen/wait/killpg ownership without running any
            # daemon test, sandbox canary, network access or physical I/O.
            process = real_popen([sys.executable, "-B", "-c", "import time; time.sleep(30)"], **kwargs)
            launched.append(process)
            return process

        def cleanup_then_fail(child):
            real_cleanup(child)
            raise RuntimeError("injected cleanup note after actual reap")

        try:
            for fault in ("thread-constructor", "thread-start", "cleanup-error"):
                suite = SimpleNamespace(logs=self.root, profile=self.root / "unused-profile",
                                        binary=self.binary, report={"processes": []})
                expected_error = "injected reader constructor failure" if fault == "thread-constructor" else "injected reader start failure"
                thread_fault = (mock.patch.object(crash.threading, "Thread", side_effect=RuntimeError(expected_error))
                                if fault == "thread-constructor" else
                                mock.patch.object(crash.threading.Thread, "start", side_effect=RuntimeError(expected_error)))
                cleanup = cleanup_then_fail if fault == "cleanup-error" else real_cleanup
                with self.subTest(fault=fault), \
                        mock.patch.object(crash.subprocess, "Popen", side_effect=harmless_owned_process), \
                        thread_fault, mock.patch.object(crash.Child, "cleanup", cleanup):
                    with self.assertRaisesRegex(RuntimeError, expected_error):
                        crash.Child(suite, crash.WORKER, self.root, "intent", "thread-fault-" + fault)
                    process = launched[-1]
                    row = suite.report["processes"][0]
                    self.assertEqual(row["pid"], process.pid)
                    self.assertEqual(row["startup_error"], "RuntimeError: " + expected_error)
                    self.assertTrue(row["reaped"])
                    self.assertTrue(row["forced_cleanup"])
                    self.assertFalse(row["timeout"])
                    self.assertTrue(row["owned_process_group_gone"])
                    self.assertEqual(process.returncode, -signal.SIGKILL)
                    self.assertTrue(process.stdout.closed)
                    self.assertEqual(row["log_sha256"], CHECKER.sha(Path(row["log"])))
                    if fault == "cleanup-error":
                        self.assertIn("injected cleanup note", row["startup_cleanup_error"])
                        self.assertIn("injected cleanup note", suite.report["cleanup_errors"][0])
                    else:
                        self.assertNotIn("startup_cleanup_error", row)
                    with self.assertRaises(ProcessLookupError):
                        os.killpg(process.pid, 0)
            self.assertEqual(len(launched), 3)
        finally:
            # Keep a regressed checker from leaking the actual unit fixtures.
            for process in launched:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
                if process.stdout is not None:
                    process.stdout.close()

    def test_wrong_event_kind_phase_or_pid_is_rejected_without_children(self):
        crash = self.driver("restore")
        for value in ({"kind": "wrong", "phase": "intent"},
                      {"kind": "boundary", "phase": "wrong"}):
            child = crash.Child.__new__(crash.Child)
            child.output_errors, child.events = [], queue.Queue()
            child.events.put(value)
            child.record = {"phase": "intent"}
            with self.assertRaises(AssertionError):
                child.event("boundary")
        child = crash.Child.__new__(crash.Child)
        child.process = SimpleNamespace(pid=123, stdout=io.StringIO(crash.EVENT + json.dumps({"pid": 456}) + "\n"))
        child.log, child.events, child.output_errors = io.StringIO(), queue.Queue(), []
        child.record = {}
        child._read()
        self.assertTrue(child.output_errors)
        self.assertTrue(child.events.empty())

    def test_smoke_is_explicit_and_never_validates_as_full(self):
        for name in CHECKER.SUITES:
            report, driver = self.receipt(name)
            report["mode"] = "smoke"
            if name != "restore":
                report["scenarios"] = report["scenarios"][:1]
                report["processes"] = report["processes"][:2]
            CHECKER.validate_suite(name, report, driver, mode="smoke")
            with self.assertRaises(ValueError):
                CHECKER.validate_suite(name, report, driver)


if __name__ == "__main__":
    unittest.main()
