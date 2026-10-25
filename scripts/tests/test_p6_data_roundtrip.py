"""Validate the mandatory hardware-denial boundary without opening hardware."""
import importlib.util
from pathlib import Path
import socket
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "check-p6-data-roundtrip.py"
SPEC = importlib.util.spec_from_file_location("p6_roundtrip", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OwnedGroupShutdownTests(unittest.TestCase):
    def daemon(self):
        daemon = MODULE.Daemon.__new__(MODULE.Daemon)
        daemon.process = mock.Mock(pid=987654, returncode=0)
        daemon.process.poll.return_value = 0
        daemon.process.wait.return_value = 0
        daemon.shutdown_timeout = False
        daemon.forced_owned_group_cleanup = False
        daemon.owned_group_exit_confirmed = False
        return daemon

    def test_clean_leader_and_gone_group_are_confirmed(self):
        daemon = self.daemon()
        with mock.patch.object(MODULE.os, "killpg", side_effect=ProcessLookupError()):
            self.assertEqual(daemon.close(), 0)
        self.assertTrue(daemon.owned_group_exit_confirmed)
        self.assertFalse(daemon.forced_owned_group_cleanup)

    def test_surviving_descendant_is_forced_and_cannot_pass_after_leader_zero(self):
        daemon = self.daemon()
        with mock.patch.object(MODULE.os, "killpg", side_effect=[None, None, ProcessLookupError()]) as kill:
            with self.assertRaisesRegex(AssertionError, "child process survived"):
                daemon.close()
        self.assertTrue(daemon.owned_group_exit_confirmed)
        self.assertTrue(daemon.forced_owned_group_cleanup)
        self.assertEqual(kill.call_args_list, [mock.call(daemon.process.pid, 0),
                                              mock.call(daemon.process.pid, signal.SIGKILL),
                                              mock.call(daemon.process.pid, 0)])

    def test_reaped_leader_does_not_confirm_group_surviving_kill(self):
        daemon = self.daemon()
        with mock.patch.object(MODULE.os, "killpg"), \
                mock.patch.object(MODULE.time, "monotonic", side_effect=[0, 2]):
            with self.assertRaisesRegex(AssertionError, "group termination unconfirmed"):
                daemon.close()
        self.assertFalse(daemon.owned_group_exit_confirmed)

    def test_postkill_group_observation_permission_failure_is_not_disappearance(self):
        daemon = self.daemon()
        with mock.patch.object(MODULE.os, "killpg", side_effect=[None, None, PermissionError()]):
            with self.assertRaisesRegex(AssertionError, "group termination unconfirmed"):
                daemon.close()
        self.assertFalse(daemon.owned_group_exit_confirmed)

    def test_timeout_is_recorded_after_bounded_forced_cleanup(self):
        daemon = self.daemon()
        daemon.process.wait.side_effect = [subprocess.TimeoutExpired("synthetic", 85), 0]
        with mock.patch.object(MODULE.os, "killpg", side_effect=[None, ProcessLookupError()]):
            with self.assertRaisesRegex(AssertionError, "shutdown timed out"):
                daemon.close()
        self.assertTrue(daemon.shutdown_timeout)
        self.assertTrue(daemon.forced_owned_group_cleanup)
        self.assertTrue(daemon.owned_group_exit_confirmed)
        self.assertEqual(daemon.process.wait.call_args_list, [mock.call(timeout=85), mock.call(timeout=1)])


@unittest.skipUnless(sys.platform == "darwin", "macOS kernel sandbox required")
class HardwareBoundaryTests(unittest.TestCase):
    def check_policy(self, transform=lambda value: value):
        runtime = MODULE.BUILD / "runtime"
        runtime.mkdir(parents=True, exist_ok=True)
        # AF_UNIX sun_path is short on macOS. A cold-build capsule path plus
        # the sandbox fixture name can exceed that limit, so keep only this
        # synthetic socket fixture under the private system temp directory.
        with tempfile.TemporaryDirectory(prefix="p546guard-", dir="/private/tmp") as directory:
            root = Path(directory).resolve()
            stage = root / "s"
            stage.mkdir()
            profile = stage / "guard.sb"
            profile.write_text(transform(MODULE.sandbox_profile(stage)))
            with socket.socket(socket.AF_UNIX) as blocked:
                blocked.bind(str(root / "blocked"))
                blocked.listen(4)
                MODULE.validate_sandbox(stage, profile, root / "blocked")

    def test_parent_and_inherited_child_deny_hardware_and_external_connections(self):
        self.check_policy()

    def test_removed_iokit_guard_rejected_before_any_device_probe(self):
        with self.assertRaisesRegex(AssertionError, "did not deny b'iokit-open'"):
            self.check_policy(lambda policy: policy.replace("(deny iokit-open)", ""))

    def test_removed_mach_guard_rejected_before_any_device_probe(self):
        with self.assertRaisesRegex(AssertionError, "did not deny b'mach-lookup'"):
            self.check_policy(lambda policy: policy.replace("(deny mach-lookup)", ""))

    def test_removed_device_file_guard_is_rejected_using_only_dev_zero(self):
        with self.assertRaisesRegex(AssertionError, "did not deny device-file access"):
            self.check_policy(lambda policy: policy.replace(
                '(deny file-read* file-write* (subpath "/dev"))', ""))

    def test_removed_network_guard_is_rejected_using_only_synthetic_socket(self):
        with self.assertRaisesRegex(AssertionError, "did not deny non-stage connection"):
            self.check_policy(lambda policy: policy.replace("(deny network-outbound)", ""))


if __name__ == "__main__":
    unittest.main()
