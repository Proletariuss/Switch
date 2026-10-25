"""Offline launcher tests: only synthetic executables and Switch build roots."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "check-p6-guarded-legacy-launch.py"
SPEC = importlib.util.spec_from_file_location("guarded_legacy", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
RUNTIME = SCRIPT.parents[2] / "build/runtime/iteration562-launcher"


class GuardedLaunchTests(unittest.TestCase):
    def setUp(self):
        RUNTIME.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="test-", dir=RUNTIME)
        self.root = Path(self.temporary.name)
        self.data = self.root / "data"
        self.data.mkdir()
        (self.data / MODULE.FIXTURE_MARKER).write_bytes(MODULE.FIXTURE_SEED)
        self.binary = self.root / "synthetic-child"
        self.pending = self.data / MODULE.PENDING
        helper = mock.Mock()
        helper.environment.side_effect = lambda root: {"SWITCH_DATA_DIR": str(root)}
        self.sandbox_patch = mock.patch.object(MODULE, "mandatory_sandbox", return_value=(helper, self.data / "stub.sb"))
        self.sandbox_patch.start()

    def tearDown(self):
        self.sandbox_patch.stop()
        self.temporary.cleanup()

    def executable(self, body):
        self.binary.write_text(f"#!{sys.executable}\n" + body)
        self.binary.chmod(0o700)

    def command(self):
        return [sys.executable, str(SCRIPT), "--data-dir", str(self.data),
                "--binary", str(self.binary), "--shutdown-grace", "0.15"]

    def fixture_command(self, *arguments):
        # Existing pre-seeded fixtures are supported only by the test API. The
        # real CLI must itself create a fresh fixture and rejects this root.
        program = ("import importlib.util, pathlib, sys; "
                   f"s=importlib.util.spec_from_file_location('guard', {str(SCRIPT)!r}); "
                   "m=importlib.util.module_from_spec(s); s.loader.exec_module(m); "
                   f"sys.exit(m.launch(pathlib.Path({str(self.data)!r}), pathlib.Path({str(self.binary)!r}), "
                   f"{list(arguments)!r}, grace=0.15))")
        return [sys.executable, "-c", program]

    def evidence(self):
        record = json.loads(self.pending.read_text())
        self.assertEqual(record["schema"], MODULE.SCHEMA)
        self.assertEqual(record["phase"], "spawn_pending")
        self.assertEqual(record["binary_sha256"], hashlib.sha256(self.binary.read_bytes()).hexdigest())
        self.assertEqual(stat.S_IMODE(self.pending.stat().st_mode), 0o600)
        self.assertEqual(set(record), {"schema", "phase", "run_id", "binary_sha256"})
        self.assertTrue(record["run_id"])
        return self.pending.read_bytes()

    def wait_file(self, path, process, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if path.exists():
                return
            if process.poll() is not None:
                self.fail(f"synthetic process exited early: {process.returncode}")
            time.sleep(0.02)
        self.fail("synthetic process readiness timed out")

    def test_explicit_arguments_are_required_and_do_not_create_data(self):
        for arguments in ([], ["--data-dir", str(self.data)], ["--binary", str(self.binary)]):
            result = subprocess.run([sys.executable, str(SCRIPT), *arguments], capture_output=True)
            self.assertEqual(result.returncode, 2)
        self.assertEqual(list(self.data.iterdir()), [self.data / MODULE.FIXTURE_MARKER])

    def test_cli_rejects_existing_root_and_non_runtime_destination(self):
        self.executable("pass\n")
        self.assertEqual(subprocess.run(self.command(), capture_output=True).returncode, 125)
        self.assertFalse(self.pending.exists())
        with self.assertRaises(ValueError):
            MODULE.prepare_synthetic_root(Path("/not-a-runtime-fixture"))

    def test_new_root_parent_is_fsynced_before_fixture_initialization(self):
        fresh = self.root / "fresh"
        fsync = MODULE.os.fsync
        parents = []

        def sync(descriptor):
            self.assertTrue(fresh.is_dir())
            self.assertFalse((fresh / MODULE.FIXTURE_MARKER).exists())
            parents.append(os.fstat(descriptor).st_ino)
            fsync(descriptor)

        with mock.patch.object(MODULE.os, "fsync", side_effect=sync):
            self.assertEqual(MODULE.prepare_synthetic_root(fresh), fresh)
        self.assertEqual(parents, [self.root.stat().st_ino])
        self.assertEqual((fresh / MODULE.FIXTURE_MARKER).read_bytes(), MODULE.FIXTURE_SEED)

    def test_new_root_parent_fsync_failure_forbids_pending_or_spawn(self):
        self.executable("raise AssertionError('must not execute')\n")
        fresh = self.root / "fresh"
        with mock.patch.object(MODULE.os, "fsync", side_effect=OSError("parent fsync")), \
                mock.patch.object(MODULE, "persist_pending") as pending, \
                mock.patch.object(MODULE, "launch") as launch, \
                mock.patch.object(MODULE.sys, "stderr"):
            result = MODULE.main(["--data-dir", str(fresh), "--binary", str(self.binary)])
        self.assertEqual(result, 125)
        pending.assert_not_called()
        launch.assert_not_called()
        self.assertTrue(fresh.is_dir())
        self.assertEqual(list(fresh.iterdir()), [])

    def test_cli_creates_new_synthetic_root_and_runs_only_under_mandatory_sandbox(self):
        self.executable("import os, pathlib\n"
                        "(pathlib.Path(os.environ['SWITCH_DATA_DIR']) / 'ran').touch()\n")
        for path in self.data.iterdir():
            path.unlink()
        self.data.rmdir()
        result = subprocess.run(self.command(), capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.data / "ran").exists())
        self.assertEqual((self.data / MODULE.FIXTURE_MARKER).read_bytes(), MODULE.FIXTURE_SEED)
        self.evidence()

    def test_exit_zero_and_one_preserve_pending_pristine_and_environment_binding(self):
        self.executable("import json, os, pathlib, sys\n"
                        "root = pathlib.Path(os.environ['SWITCH_DATA_DIR'])\n"
                        "assert json.loads((root / '.legacy-execution-pending-v1.json').read_text())"
                        "['phase'] == 'spawn_pending'\n"
                        "(root / 'environment').write_text(str(root))\n"
                        "sys.exit(int(sys.argv[1]))\n")
        for result_code in (0, 1):
            with self.subTest(exit=result_code):
                pristine = self.data / ".call-mode-pristine-v1"
                pristine.touch()
                result = subprocess.run(self.fixture_command(str(result_code)),
                                        env={**os.environ, "SWITCH_DATA_DIR": "/must-not-be-used"},
                                        capture_output=True, timeout=30)
                self.assertEqual(result.returncode, result_code, result.stderr)
                self.evidence()
                self.assertEqual((self.data / "environment").read_text(), str(self.data))
                self.assertEqual(pristine.read_bytes(), b"")
                # Only the fixture owns and removes its evidence between cases.
                self.pending.unlink()
                (self.data / "hardware-denied.sb").unlink()

    def test_existing_evidence_of_every_shape_is_never_replaced_or_spawned(self):
        self.executable("raise AssertionError('must not execute')\n")
        for kind in ("valid", "invalid", "empty", "unreadable", "directory", "symlink", "dangling", "hardlink"):
            with self.subTest(kind=kind):
                target = self.root / "target"
                target.write_bytes(b"preserve-target")
                if kind == "directory":
                    self.pending.mkdir()
                elif kind in ("symlink", "dangling"):
                    self.pending.symlink_to(target if kind == "symlink" else self.root / "absent")
                elif kind == "hardlink":
                    os.link(target, self.pending)
                else:
                    self.pending.write_bytes({"valid": b'{"schema":"legacy_execution_pending_v1"}',
                                              "empty": b""}.get(kind, b"invalid-evidence"))
                    if kind == "unreadable":
                        self.pending.chmod(0)
                before = self.pending.lstat()
                with mock.patch.object(MODULE.subprocess, "Popen") as spawn:
                    with self.assertRaises(OSError):
                        MODULE.launch(self.data, self.binary)
                    spawn.assert_not_called()
                after = self.pending.lstat()
                self.assertEqual((before.st_ino, before.st_size, before.st_mtime_ns),
                                 (after.st_ino, after.st_size, after.st_mtime_ns))
                self.assertEqual(target.read_bytes(), b"preserve-target")
                if kind == "directory":
                    self.pending.rmdir()
                else:
                    self.pending.unlink()

    def test_binary_and_data_links_are_rejected_before_pending(self):
        self.executable("pass\n")
        link = self.root / "link"
        link.symlink_to(self.data)
        with self.assertRaises(OSError):
            MODULE.launch(link, self.binary)
        link.unlink()
        link.symlink_to(self.binary)
        with self.assertRaises(OSError):
            MODULE.launch(self.data, link)
        self.assertFalse(self.pending.exists())

    def test_both_fsync_barriers_precede_spawn_and_spawn_failure_keeps_record(self):
        self.executable("pass\n")
        events = []
        fsync = MODULE.os.fsync

        def sync(descriptor):
            events.append("directory" if stat.S_ISDIR(os.fstat(descriptor).st_mode) else "file")
            fsync(descriptor)

        def spawn(*_args, **_kwargs):
            events.append("spawn")
            self.evidence()
            raise OSError("injected spawn failure")

        with mock.patch.object(MODULE.os, "fsync", side_effect=sync), \
                mock.patch.object(MODULE.subprocess, "Popen", side_effect=spawn):
            with self.assertRaises(OSError):
                MODULE.launch(self.data, self.binary)
        self.assertEqual(events, ["file", "directory", "spawn"])
        self.evidence()

    def test_write_file_fsync_and_directory_fsync_failures_cannot_spawn_or_clear(self):
        self.executable("pass\n")
        for point in ("write", "file-fsync", "directory-fsync"):
            with self.subTest(point=point):
                fsync = MODULE.os.fsync

                def sync(descriptor):
                    directory = stat.S_ISDIR(os.fstat(descriptor).st_mode)
                    if directory == (point == "directory-fsync"):
                        raise OSError("injected fsync failure")
                    fsync(descriptor)

                with mock.patch.object(MODULE.subprocess, "Popen") as spawn:
                    if point == "write":
                        context = mock.patch.object(MODULE.os, "write", side_effect=OSError("write"))
                    else:
                        context = mock.patch.object(MODULE.os, "fsync", side_effect=sync)
                    with context, self.assertRaises(OSError):
                        MODULE.launch(self.data, self.binary)
                    spawn.assert_not_called()
                self.assertTrue(self.pending.exists())
                if point != "write":
                    self.evidence()
                self.pending.unlink()

    def test_partial_writes_are_completed_before_spawn(self):
        descriptor = MODULE.open_directory(self.data)
        write = os.write
        try:
            with mock.patch.object(MODULE.os, "write", side_effect=lambda fd, body: write(fd, body[:7])):
                MODULE.persist_pending(descriptor, "a" * 64)
            self.assertEqual(json.loads(self.pending.read_text())["binary_sha256"], "a" * 64)
        finally:
            os.close(descriptor)

    def test_launcher_crash_after_durable_record_retains_pending_without_spawn(self):
        self.executable("raise AssertionError('must not execute')\n")
        program = ("import importlib.util, os, pathlib; "
                   f"s=importlib.util.spec_from_file_location('guard', {str(SCRIPT)!r}); "
                   "m=importlib.util.module_from_spec(s); s.loader.exec_module(m); "
                   f"fd=m.open_directory(pathlib.Path({str(self.data)!r})); "
                   f"m.persist_pending(fd, {hashlib.sha256(self.binary.read_bytes()).hexdigest()!r}); "
                   "os._exit(77)")
        result = subprocess.run([sys.executable, "-c", program], timeout=5)
        self.assertEqual(result.returncode, 77)
        before = self.evidence()
        self.assertEqual(subprocess.run(self.command(), capture_output=True, timeout=5).returncode, 125)
        self.assertEqual(self.pending.read_bytes(), before)

    def test_signal_forwarding_and_bounded_kill_retain_pending(self):
        for signum in (signal.SIGTERM, signal.SIGINT):
            with self.subTest(signal=signum):
                self.executable("import os, pathlib, signal, time\n"
                                "root = pathlib.Path(os.environ['SWITCH_DATA_DIR'])\n"
                                "def observed(sig, frame):\n"
                                "    with (root / 'signal').open('a') as stream: stream.write(str(sig) + '\\n')\n"
                                "signal.signal(signal.SIGTERM, observed)\n"
                                "signal.signal(signal.SIGINT, observed)\n"
                                "(root / 'ready').touch()\n"
                                "while True: time.sleep(0.02)\n")
                process = subprocess.Popen(self.fixture_command(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    self.wait_file(self.data / "ready", process)
                    before = self.evidence()
                    process.send_signal(signum)
                    self.assertEqual(process.wait(timeout=5), 128 + signal.SIGKILL)
                    self.assertIn(str(int(signum)), (self.data / "signal").read_text().splitlines())
                    self.assertEqual(self.pending.read_bytes(), before)
                finally:
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=5)
                    for name in (MODULE.PENDING, "ready", "signal", "hardware-denied.sb"):
                        (self.data / name).unlink(missing_ok=True)

    def test_group_signal_targets_only_the_owned_child_group(self):
        child = mock.Mock(pid=987654)
        with mock.patch.object(MODULE.os, "killpg") as kill:
            MODULE.signal_group(child, signal.SIGINT)
        kill.assert_called_once_with(child.pid, signal.SIGINT)

    def test_observed_gone_group_is_reaped_without_another_signal(self):
        for reap_timeout in (False, True):
            with self.subTest(reap_timeout=reap_timeout):
                child = mock.Mock(pid=987654)
                if reap_timeout:
                    child.wait.side_effect = subprocess.TimeoutExpired("synthetic", 1)
                with mock.patch.object(MODULE.os, "killpg", side_effect=[None, ProcessLookupError()]) as kill:
                    if reap_timeout:
                        with self.assertRaisesRegex(RuntimeError, "termination unconfirmed"):
                            MODULE.drain_group(child, 0.1)
                    else:
                        MODULE.drain_group(child, 0.1)
                self.assertEqual(kill.call_args_list, [mock.call(child.pid, signal.SIGTERM),
                                                      mock.call(child.pid, 0)])
                child.wait.assert_called_once_with(timeout=1)

    def test_forced_cleanup_requires_group_disappearance_after_leader_reap(self):
        child = mock.Mock(pid=987654)
        with mock.patch.object(MODULE.os, "killpg", side_effect=[None, None, ProcessLookupError()]) as kill, \
                mock.patch.object(MODULE.time, "monotonic", side_effect=[0, 1, 1]):
            self.assertTrue(MODULE.drain_group(child, .1))
        self.assertEqual(kill.call_args_list, [mock.call(child.pid, signal.SIGTERM),
                                              mock.call(child.pid, signal.SIGKILL), mock.call(child.pid, 0)])
        child.wait.assert_called_once_with(timeout=1)

    def test_forced_cleanup_with_surviving_group_is_unconfirmed(self):
        child = mock.Mock(pid=987654)
        with mock.patch.object(MODULE.os, "killpg"), \
                mock.patch.object(MODULE.time, "monotonic", side_effect=[0, 1, 1, 3]):
            with self.assertRaisesRegex(RuntimeError, "group termination unconfirmed"):
                MODULE.drain_group(child, .1)

    def test_postkill_group_permission_failure_is_not_disappearance(self):
        child = mock.Mock(pid=987654)
        with mock.patch.object(MODULE.os, "killpg", side_effect=[None, None, PermissionError()]), \
                mock.patch.object(MODULE.time, "monotonic", side_effect=[0, 1, 1]):
            with self.assertRaisesRegex(RuntimeError, "group termination unconfirmed"):
                MODULE.drain_group(child, .1)

    def test_forced_descendant_cleanup_cannot_hide_behind_leader_zero(self):
        self.assertEqual(MODULE.exit_result(0, True), 128 + signal.SIGKILL)
        self.assertEqual(MODULE.exit_result(0, False), 0)
        self.assertEqual(MODULE.exit_result(1, True), 1)
        self.assertEqual(MODULE.exit_result(1, False), 1)

    def test_legacy_exit_one_after_forwarded_term_is_not_reclassified(self):
        self.executable("import os, pathlib, signal, sys, time\n"
                        "root = pathlib.Path(os.environ['SWITCH_DATA_DIR'])\n"
                        "signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))\n"
                        "(root / 'ready').touch()\n"
                        "while True: time.sleep(0.02)\n")
        process = subprocess.Popen(self.fixture_command(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            self.wait_file(self.data / "ready", process)
            before = self.evidence()
            process.terminate()
            self.assertEqual(process.wait(timeout=5), 1)
            self.assertEqual(self.pending.read_bytes(), before)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    def test_owned_descendant_is_drained_without_signalling_unrelated_process(self):
        self.executable("import os, pathlib, signal, time\n"
                        "root = pathlib.Path(os.environ['SWITCH_DATA_DIR'])\n"
                        "if os.fork() == 0:\n"
                        "    signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                        "    (root / 'descendant').write_text(str(os.getpid()))\n"
                        "    while True: time.sleep(0.02)\n"
                        "while not (root / 'descendant').exists(): time.sleep(0.01)\n")
        unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        try:
            result = subprocess.run(self.fixture_command(), capture_output=True, timeout=30)
            # The orphan group can be unobservable (EPERM) after SIGKILL on
            # macOS. Both forced cleanup and unconfirmed group exit must fail;
            # the actual descendant is independently checked below.
            self.assertNotEqual(result.returncode, 0, result.stderr)
            self.assertIsNone(unrelated.poll())
            self.evidence()
            descendant = int((self.data / "descendant").read_text())
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                try:
                    os.kill(descendant, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.02)
            else:
                self.fail("owned synthetic descendant survived group cleanup")
        finally:
            unrelated.terminate()
            unrelated.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
