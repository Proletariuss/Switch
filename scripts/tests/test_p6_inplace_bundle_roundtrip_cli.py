"""The P6 bundle test accepts an explicit current candidate version."""
import importlib.util
import os
from pathlib import Path
import socket
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "check-p6-inplace-bundle-roundtrip.py"
SPEC = importlib.util.spec_from_file_location("p6_inplace_bundle_roundtrip", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class InplaceBundleRoundtripCLITests(unittest.TestCase):
    def required(self):
        return ["--legacy-app", "/tmp/legacy.app", "--candidate-app", "/tmp/candidate.app",
                "--candidate-label", "fixture", "--report", "/tmp/report.json"]

    def test_historical_candidate_version_remains_default(self):
        args = MODULE.parse_args(self.required())
        self.assertEqual(args.expected_candidate_version, "3.43")

    def test_current_candidate_version_can_be_selected_explicitly(self):
        args = MODULE.parse_args(self.required() + ["--expected-candidate-version", "3.47"])
        self.assertEqual(args.expected_candidate_version, "3.47")


class LifecycleEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def test_current_protocol_namespaces_and_nested_receipts_are_captured(self):
        for name in MODULE.LIFECYCLE_NAMES:
            (self.root / name).write_bytes(b"preserve")
        (self.root / "module-recovery-v1").unlink()
        (self.root / "module-recovery-v1").mkdir()
        (self.root / "module-recovery-v1" / "receipt").write_bytes(b"immutable")
        footprint = MODULE.lifecycle_footprints(self.root)
        self.assertEqual(set(footprint), set(MODULE.LIFECYCLE_NAMES) | {"module-recovery-v1/receipt"})
        before = footprint[".legacy-execution-pending-v1.json"]
        (self.root / ".legacy-execution-pending-v1.json").write_bytes(b"changed!")
        self.assertNotEqual(MODULE.lifecycle_footprints(self.root)[".legacy-execution-pending-v1.json"], before)

    def test_nonregular_evidence_is_recorded_without_following_or_blocking(self):
        external = self.root / "external"
        external.mkdir()
        (external / "secret").write_bytes(b"must not read")
        pending = self.root / ".legacy-execution-pending-v1.json"
        pending.symlink_to(external, target_is_directory=True)
        os.mkfifo(self.root / "call-mode-operation.lock")
        with socket.socket(socket.AF_UNIX) as stream:
            stream.bind(str(self.root / "voice-control"))
            result = MODULE.lifecycle_footprints(self.root)
        self.assertEqual(result[pending.name]["type"], "symlink")
        self.assertEqual(result["call-mode-operation.lock"]["type"], "fifo")
        self.assertEqual(result["voice-control"]["type"], "socket")
        self.assertNotIn(pending.name + "/secret", result)
        self.assertNotIn("sha256", result[pending.name])
        pending.unlink()
        pending.mkdir()
        self.assertFalse(MODULE.recovery_evidence_preserved(result, MODULE.lifecycle_footprints(self.root)))

    def test_new_lifetime_lock_is_the_only_allowed_added_evidence(self):
        before = MODULE.lifecycle_footprints(self.root)
        lock = self.root / ".switchd-lifetime.lock"
        lock.touch(mode=0o600)
        after = MODULE.lifecycle_footprints(self.root)
        self.assertTrue(MODULE.recovery_evidence_preserved(before, after))
        (self.root / ".legacy-execution-pending-v1.json").touch()
        self.assertFalse(MODULE.recovery_evidence_preserved(before, MODULE.lifecycle_footprints(self.root)))
        self.assertFalse(MODULE.recovery_evidence_preserved(after, before))
        for field, value in (("type", "fifo"), ("size", 1), ("links", 2), ("mode", 0o644)):
            with self.subTest(field=field):
                invalid = {lock.name: {**after[lock.name], field: value}}
                self.assertFalse(MODULE.recovery_evidence_preserved(before, invalid))

    def test_existing_lock_mutation_and_oversized_evidence_are_not_accepted(self):
        lock = self.root / ".switchd-lifetime.lock"
        lock.touch(mode=0o600)
        before = MODULE.lifecycle_footprints(self.root)
        lock.chmod(0o644)
        self.assertFalse(MODULE.recovery_evidence_preserved(before, MODULE.lifecycle_footprints(self.root)))
        lock.write_bytes(b"large")
        with mock.patch.object(MODULE, "MAX_EVIDENCE_BYTES", 1):
            with self.assertRaisesRegex(AssertionError, "snapshot bound"):
                MODULE.lifecycle_footprints(self.root)


class ShutdownAcceptanceTests(unittest.TestCase):
    def clean(self):
        return {"exit_code": 0, "owned_group_exit_confirmed": True, "shutdown_timeout": False,
                "forced_owned_group_cleanup": False, "sockets_remaining": [],
                "native_shutdown_receipt": True}

    def test_default_mode_requires_all_lifecycle_conditions(self):
        self.assertTrue(MODULE.shutdown_accepted(self.clean(), native=True))
        for field, value in (("exit_code", 1), ("owned_group_exit_confirmed", False),
                             ("shutdown_timeout", True), ("forced_owned_group_cleanup", True),
                             ("sockets_remaining", ["s"]), ("native_shutdown_receipt", False)):
            with self.subTest(field=field):
                self.assertFalse(MODULE.shutdown_accepted({**self.clean(), field: value}, native=True))
        for field in self.clean():
            missing = self.clean()
            del missing[field]
            self.assertFalse(MODULE.shutdown_accepted(missing, native=True))

    def test_legacy_exit_one_is_never_accepted_and_does_not_require_native_receipt(self):
        legacy = {**self.clean(), "native_shutdown_receipt": False}
        self.assertTrue(MODULE.shutdown_accepted(legacy))
        self.assertFalse(MODULE.shutdown_accepted({**legacy, "exit_code": 1}))

    def test_nonquarantine_exercise_records_dangling_socket_and_cleanup_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = root / "report"
            report.mkdir()
            (root / "s.go").symlink_to(root / "absent")
            daemon = mock.Mock(shutdown_timeout=False, forced_owned_group_cleanup=False,
                               owned_group_exit_confirmed=True)
            daemon.log = root / "daemon.log"
            daemon.log.write_text("switchd-rs: native shutdown complete\n")
            daemon.close.return_value = 0
            with mock.patch.object(MODULE.BASE, "validate_sandbox"), \
                    mock.patch.object(MODULE.BASE, "hashes", return_value={}), \
                    mock.patch.object(MODULE.BASE, "inspect", return_value={}), \
                    mock.patch.object(MODULE.BASE, "Daemon", return_value=daemon):
                observation = MODULE.exercise(root, report, "candidate", Path("unused"), None, [])
            self.assertEqual(observation["sockets_remaining"], ["s.go"])
            self.assertTrue(observation["owned_group_exit_confirmed"])
            self.assertFalse(MODULE.shutdown_accepted(observation, native=True))


if __name__ == "__main__":
    unittest.main()
