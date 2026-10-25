#!/usr/bin/env python3
"""Offline P6 fixture: persist unknown history before a sandboxed legacy spawn.

Pending evidence is intentionally never removed, including after exit 0. This
launcher is not a physical recovery receipt and does not cover unwrapped runs.
Only fresh synthetic roots below Switch build/runtime are supported. This is
not a production rollback launcher: concurrent daemons and mutable directory
or executable ancestry remain a P6 blocker pending a shared lifetime lease.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import stat
import subprocess
import sys
import time
import tempfile
import uuid


PENDING = ".legacy-execution-pending-v1.json"
SCHEMA = "legacy_execution_pending_v1"
SOURCE = Path(__file__).resolve().parents[1]
RUNTIME = SOURCE.parent / "build/runtime"
FIXTURE_MARKER = ".p562-offline-synthetic-v1"
FIXTURE_SEED = b"p562 synthetic fixture only; no physical recovery receipt\n"


def sandbox_helper():
    spec = importlib.util.spec_from_file_location("p546_guarded_helper", SOURCE / "scripts/check-p6-data-roundtrip.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    return helper


def prepare_synthetic_root(data_dir):
    """CLI never adopts an existing directory, even a previous test fixture."""
    data_dir = Path(os.path.abspath(data_dir))
    if sys.platform != "darwin" or not data_dir.is_relative_to(RUNTIME) or data_dir == RUNTIME:
        raise ValueError("fresh macOS Switch build/runtime fixture required")
    parent = open_directory(data_dir.parent)
    try:
        os.mkdir(data_dir.name, 0o700, dir_fd=parent)
        # The later file/root barriers do not persist this new entry in its
        # parent. Refuse further initialization if the ancestry barrier fails.
        os.fsync(parent)
    finally:
        os.close(parent)
    (data_dir / FIXTURE_MARKER).write_bytes(FIXTURE_SEED)
    return data_dir


def validate_fixture(data_dir):
    if not data_dir.is_relative_to(RUNTIME) or data_dir == RUNTIME:
        raise ValueError("not a synthetic runtime root")
    directory = open_directory(data_dir)
    try:
        descriptor = os.open(FIXTURE_MARKER, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        try:
            metadata = os.fstat(descriptor)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or os.read(descriptor, 1024) != FIXTURE_SEED:
                raise ValueError("fixture seed invalid")
        finally:
            os.close(descriptor)
    finally:
        os.close(directory)


def mandatory_sandbox(data_dir):
    """Reuse 546 policy and kernel canary, including inherited-child checks."""
    if sys.platform != "darwin":
        raise ValueError("macOS mandatory sandbox required")
    validate_fixture(data_dir)
    helper = sandbox_helper()
    profile = data_dir / "hardware-denied.sb"
    # A previously existing profile must not redirect writes through a link.
    descriptor = os.open(profile, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        stream.write(helper.sandbox_profile(data_dir))
    with tempfile.TemporaryDirectory(prefix="p562-denied-", dir=RUNTIME) as denied:
        with socket.socket(socket.AF_UNIX) as blocked:
            address = str(Path(denied) / "blocked")
            blocked.bind(address)
            blocked.listen(1)
            helper.validate_sandbox(data_dir, profile, address)
    return helper, profile


def open_directory(path):
    """Pin existing ancestry without following any symlink components."""
    path = Path(os.path.abspath(path))
    descriptor = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in path.parts[1:]:
            next_descriptor = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                      dir_fd=descriptor)
            os.close(descriptor)
            descriptor = next_descriptor
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def binary_handle(binary):
    """Hash a no-follow handle; offline fixtures require static trusted paths."""
    parent = open_directory(binary.parent)
    try:
        descriptor = os.open(binary.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=parent)
    finally:
        os.close(parent)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or not before.st_mode & 0o111:
            raise ValueError("binary must be a regular executable")
        digest = hashlib.sha256()
        while block := os.read(descriptor, 1024 * 1024):
            digest.update(block)
        after = os.fstat(descriptor)
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError("binary changed during fingerprinting")
        os.lseek(descriptor, 0, os.SEEK_SET)
        return descriptor, digest.hexdigest()
    except BaseException:
        os.close(descriptor)
        raise


def persist_pending(directory, binary_sha256):
    """Never truncate, replace, follow, parse, or clear existing evidence."""
    descriptor = os.open(PENDING, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=directory)
    try:
        os.fchmod(descriptor, 0o600)
        body = (json.dumps({"schema": SCHEMA, "run_id": str(uuid.uuid4()),
                            "binary_sha256": binary_sha256, "phase": "spawn_pending"},
                           sort_keys=True) + "\n").encode("ascii")
        while body:
            written = os.write(descriptor, body)
            if written <= 0:
                raise OSError("pending evidence write did not advance")
            body = body[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    # Spawn is forbidden if either durability barrier fails. Partial/empty
    # records are retained as unknown evidence after every failure/crash point.
    os.fsync(directory)


def signal_group(child, signum):
    try:
        os.killpg(child.pid, signum)
    except ProcessLookupError:
        pass


def drain_group(child, grace, signum=signal.SIGTERM):
    """Return whether forced cleanup was needed; require the whole group gone."""
    signal_group(child, signum)
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        try:
            os.killpg(child.pid, 0)
        except ProcessLookupError:
            # An observed-gone group needs no further signal. Still require
            # the owned leader's bounded reap before reporting completion.
            try:
                child.wait(timeout=1)
            except subprocess.TimeoutExpired:
                raise RuntimeError("legacy child termination unconfirmed") from None
            return False
        child.poll()
        time.sleep(min(0.05, max(0, deadline - time.monotonic())))
    signal_group(child, signal.SIGKILL)
    try:
        child.wait(timeout=1)
    except subprocess.TimeoutExpired:
        raise RuntimeError("legacy child termination unconfirmed") from None
    # Reaping the leader after SIGKILL is insufficient: descendants can still
    # exist in this exact group. Bound confirmation independently of the grace.
    deadline = time.monotonic() + 1.0
    while True:
        try:
            os.killpg(child.pid, 0)
        except ProcessLookupError:
            return True
        except OSError:
            raise RuntimeError("legacy process group termination unconfirmed") from None
        if time.monotonic() >= deadline:
            raise RuntimeError("legacy process group termination unconfirmed")
        time.sleep(min(0.02, max(0, deadline - time.monotonic())))


def exit_result(result, forced):
    # Preserve the actual nonzero leader result; a leader's exit 0 cannot hide
    # a descendant which required SIGKILL. Pending remains durable in all cases.
    if result == 0 and forced:
        return 128 + signal.SIGKILL
    return 128 - result if result < 0 else result


def launch(data_dir, binary, arguments=(), grace=5.0):
    data_dir = Path(os.path.abspath(data_dir))
    binary = Path(os.path.abspath(binary))
    helper, profile = mandatory_sandbox(data_dir)
    directory = open_directory(data_dir)
    binary_fd = None
    child = None
    requested_signal = []
    previous_handlers = {}

    def interrupted(signum, _frame):
        if not requested_signal:
            requested_signal.append(signum)
        if child is not None:
            signal_group(child, signum)

    try:
        for signum in (signal.SIGTERM, signal.SIGINT):
            previous_handlers[signum] = signal.signal(signum, interrupted)
        binary_fd, digest = binary_handle(binary)
        persist_pending(directory, digest)
        if requested_signal:
            return 128 + requested_signal[0]
        environment = helper.environment(data_dir)
        # macOS cannot exec /dev/fd handles. Static fixture paths are an
        # explicit offline boundary, not a race-free production guarantee.
        if os.stat(binary, follow_symlinks=False) != os.fstat(binary_fd):
            raise ValueError("fixture binary changed before spawn")
        child = subprocess.Popen(["/usr/bin/sandbox-exec", "-f", str(profile), str(binary), *arguments],
                                 env=environment, cwd=data_dir, stdin=subprocess.DEVNULL,
                                 start_new_session=True)
        while child.poll() is None and not requested_signal:
            time.sleep(0.05)
        if requested_signal:
            forced = drain_group(child, grace, requested_signal[0])
            # Preserve a legacy shutdown failure (including exit 1). A signal
            # sent to this wrapper must not reclassify the child's result.
            result = child.returncode
            return exit_result(result, forced)
        result = child.returncode
        # Descendants can outlive the selected binary. They remain our group
        # and do not justify discarding pending or claiming physical recovery.
        forced = drain_group(child, grace)
        return exit_result(result, forced)
    finally:
        if child is not None and child.poll() is None:
            drain_group(child, grace)
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
        if binary_fd is not None:
            os.close(binary_fd)
        os.close(directory)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", required=True, type=Path,
                        help="new synthetic directory below Switch build/runtime; must not exist")
    parser.add_argument("--binary", required=True, type=Path,
                        help="explicit regular executable; no symlink ancestry")
    parser.add_argument("--shutdown-grace", type=float, default=5.0)
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    options = parser.parse_args(argv)
    if not 0 < options.shutdown_grace <= 60:
        parser.error("--shutdown-grace must be greater than 0 and at most 60 seconds")
    arguments = options.arguments
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    try:
        data_dir = prepare_synthetic_root(options.data_dir)
        return launch(data_dir, options.binary, arguments, options.shutdown_grace)
    except (OSError, ValueError, RuntimeError, AssertionError, subprocess.SubprocessError):
        # Do not expose child arguments, environment, paths, or exception text:
        # callers may have supplied actual identities or keys in those values.
        print("guarded legacy launch refused or unconfirmed; pending evidence retained",
              file=sys.stderr)
        return 125


if __name__ == "__main__":
    sys.exit(main())
