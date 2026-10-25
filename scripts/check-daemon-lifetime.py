#!/usr/bin/env python3
"""Real-daemon lifetime checks on fresh synthetic roots in a mandatory sandbox.

No hardware, provider connection, original user data or legacy binary is used.
Timeouts, forced cleanup and unrecognized nonzero exits are failures. Only
process groups created by this invocation are eligible for final cleanup.
"""
import argparse
import ctypes
import fcntl
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import uuid

sys.dont_write_bytecode = True
SELF = Path(__file__).resolve()
SOURCE = SELF.parents[1]
WORKSPACE = SOURCE.parent
BUILD = WORKSPACE / "build"
LOCK = ".switchd-lifetime.lock"
BUSY = "Switch data directory is already owned by another daemon"
UNSAFE_LOCK = "Switch daemon lifetime lock is unsafe or changed"
ACTIVE_SOCKET = "backend_socket_in_use"
HELPER_PATH = SOURCE / "scripts/check-p6-data-roundtrip.py"
SPEC = importlib.util.spec_from_file_location("lifetime_p546_sandbox", HELPER_PATH)
BASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BASE)
CONTRACT_PATH = SOURCE / "scripts/check-rust-migration-contracts.py"
CONTRACT_REVIEW_PATH = SOURCE / "docs/optimization/all-rust/api-contract-review.json"
CONTRACT_INVENTORY_PATH = SOURCE / "docs/optimization/all-rust/api-contract-inventory.json"
CONTRACT_SPEC = importlib.util.spec_from_file_location("lifetime_contract_census", CONTRACT_PATH)
CONTRACT = importlib.util.module_from_spec(CONTRACT_SPEC)
CONTRACT_SPEC.loader.exec_module(CONTRACT)


def require(condition, detail):
    if not condition:
        raise AssertionError(detail)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def interface_ownership():
    """Return the validated current Rust HTTP ownership census.

    This report used to embed a point-in-time route count, so a successful
    daemon-lifetime run could contradict the migration gate after a route was
    added. Rebuild the inventory from source, compare it with the reviewed
    inventory and apply the same Rust-only policy before recording a count.
    """
    review = json.loads(CONTRACT_REVIEW_PATH.read_text())
    recorded = json.loads(CONTRACT_INVENTORY_PATH.read_text())
    actual, errors = CONTRACT.build_inventory(SOURCE, review)
    errors += CONTRACT.compare(recorded, actual)
    errors += CONTRACT.rust_only_errors(actual)
    require(not errors, "interface ownership census failed: " + "; ".join(errors))
    rust = sum(route["owner"] == "rust" for route in actual["routes"])
    go = sum(route["owner"] == "go" for route in actual["routes"])
    return {
        "implemented": rust,
        "total": len(actual["routes"]),
        "go": go,
        "head_aliases": sum(bool(route.get("alias_of")) for route in actual["routes"]),
        "swift_http_calls": sum(
            call["resolution"] != "reviewed_non_http" for call in actual["swift_calls"]
        ),
        "source": "live source checked against reviewed contract inventory",
    }


def policy_canary():
    library = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    for operation in [b"iokit-open", b"iokit-open-user-client", b"mach-lookup"]:
        require(library.sandbox_check(os.getpid(), operation, 0) == 1,
                "mandatory sandbox did not deny " + operation.decode())


def canary(root, blocked, inherited):
    policy_canary()
    BASE.canary(root, blocked, inherited=True)
    if not inherited:
        result = subprocess.run(
            [sys.executable, str(SELF), "--canary", str(root), blocked, "--inherited"],
            env=BASE.environment(root), capture_output=True, text=True, timeout=10)
        require(result.returncode == 0, "canary child lost sandbox: " + result.stderr)


def gated_exec(arguments):
    # Each contender checks the inherited sandbox before consuming its gate
    # byte. This schedules competing launches, not a proof of every possible
    # lstat/bind interleaving or that both children reached the read together.
    policy_canary()
    descriptor = int(arguments[0])
    require(os.read(descriptor, 1) == b"g", "concurrent start gate closed")
    os.close(descriptor)
    os.execve(arguments[1], arguments[1:], os.environ.copy())


def metadata(path):
    info = Path(path).lstat()
    result = {"device": info.st_dev, "inode": info.st_ino, "mode": info.st_mode,
              "size": info.st_size, "nlink": info.st_nlink,
              "mtime_ns": info.st_mtime_ns, "ctime_ns": info.st_ctime_ns}
    if stat.S_ISREG(info.st_mode):
        result["sha256"] = sha(path)
    elif stat.S_ISLNK(info.st_mode):
        result["target"] = os.readlink(path)
    return result


def evidence_tree(root):
    # lstat and walk(followlinks=False) preserve, rather than adopt, bad locks.
    result = {".": metadata(root)}
    for directory, folders, files in os.walk(root, followlinks=False):
        for name in sorted(folders + files):
            path = Path(directory) / name
            result[str(path.relative_to(root))] = metadata(path)
    return result


def socket_paths(public):
    return [public, Path(str(public) + ".rs"), Path(str(public) + ".in")]


def sockets(public):
    result = {}
    for path in socket_paths(public):
        info = path.lstat()
        require(stat.S_ISSOCK(info.st_mode), "expected live socket: " + str(path))
        require(stat.S_IMODE(info.st_mode) == 0o600,
                "live socket permissions must be 0600: " + str(path))
        result[str(path)] = {"device": info.st_dev, "inode": info.st_ino,
                             "mode": info.st_mode}
    return result


def request_at(path, route, headers=None):
    connection = http.client.HTTPConnection("localhost", timeout=2)
    stream = socket.socket(socket.AF_UNIX)
    stream.settimeout(2)
    try:
        stream.connect(str(path))
        connection.sock = stream
        connection.request("GET", route, headers=headers or {})
        response = connection.getresponse()
        return response.status, json.loads(response.read())
    finally:
        stream.close()
        connection.close()


def request(public, token):
    return request_at(public, "/api/v2/runtime", {
        "x-proletarius-app-token": token})


def unauthorized_surfaces(public, token):
    expected = {"public": "authentication_required",
                "callback": "authentication_required",
                "internal": "internal_authentication_required"}
    routes = {"public": "/api/v2/runtime", "callback": "/api/v2/runtime",
              "internal": "/api/internal/qmi/status"}
    observed = {}
    for role, path in zip(expected, socket_paths(public)):
        for variant, headers in [
            ("missing", {}),
            ("wrong", {"x-proletarius-app-token": "wrong-lifetime-token",
                       "x-switch-internal-token": "wrong-lifetime-token"}),
        ]:
            status, body = request_at(path, routes[role], headers)
            require(status == 401 and body.get("code") == expected[role],
                    role + " listener admitted or misclassified " + variant + " credentials")
            require(set(body).issubset({"code", "error", "retryable"}) and
                    token not in json.dumps(body, ensure_ascii=False) and
                    "wrong-lifetime-token" not in json.dumps(body, ensure_ascii=False),
                    role + " listener exposed credential or restricted state")
            observed[role + "_" + variant] = {"status": status, "code": body["code"]}
    return observed


class Daemon:
    def __init__(self, suite, label, root, public=None, gate=None):
        self.suite, self.root = suite, root
        self.public = public or root / "s"
        for path in socket_paths(self.public):
            require(len(os.fsencode(path)) < 100, "Unix socket fixture path too long")
        self.token = "synthetic-lifetime-" + uuid.uuid4().hex
        self.started = time.monotonic()
        self.group_gone = False
        self.ready_observed = False
        self.log = suite.logs / (label + ".log")
        self.record = {"label": label, "root": str(root), "public": str(self.public),
                       "token_sha256": hashlib.sha256(self.token.encode()).hexdigest(),
                       "log": str(self.log), "sandbox_profile_sha256": sha(suite.profile),
                       "forced_cleanup": False, "timeout": False}
        suite.report["processes"].append(self.record)
        arguments = [str(suite.binary), "-port", str(suite.path / "absent-serial"),
                     "-listen", "unix:" + str(self.public)]
        if gate is not None:
            arguments = [sys.executable, str(SELF), "--gated", str(gate), *arguments]
        environment = BASE.environment(root)
        environment["PROLETARIUS_APP_TOKEN"] = self.token
        environment["PYTHONDONTWRITEBYTECODE"] = "1"
        with self.log.open("wb") as output:
            self.process = subprocess.Popen(
                ["/usr/bin/sandbox-exec", "-f", str(suite.profile), *arguments],
                env=environment, cwd=root, stdin=subprocess.DEVNULL,
                stdout=output, stderr=output, start_new_session=True,
                pass_fds=() if gate is None else (gate,))
        self.record["pid"] = self.process.pid
        suite.children.append(self)

    def log_text(self):
        return self.log.read_text(errors="replace")

    def update(self):
        self.record["exit_code"] = self.process.poll()
        self.record["elapsed_seconds"] = round(time.monotonic() - self.started, 4)
        self.record["log_sha256"] = sha(self.log)

    def group_exists(self):
        if self.group_gone:
            return False
        try:
            os.killpg(self.process.pid, 0)
            return True
        except ProcessLookupError:
            self.group_gone = True
            return False

    def wait(self, timeout):
        try:
            result = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.record["timeout"] = True
            self.update()
            raise AssertionError("daemon exit timeout: " + self.record["label"]) from None
        self.update()
        require(not self.group_exists(), "owned descendants survived leader: " + self.record["label"])
        return result

    def expect_failure(self, text):
        require(self.wait(5) == 1, "expected exact exit 1: " + self.record["label"])
        require(time.monotonic() - self.started < 5,
                "startup rejection was not prompt: " + self.record["label"])
        require(text in self.log_text(), "wrong rejection reason: " + self.record["label"])
        require("native shutdown complete" not in self.log_text(), "startup rejection was scored as success")
        self.record["expected_rejection"] = text

    def ready(self):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "daemon exited before ready: " + self.record["label"])
            try:
                status, value = request(self.public, self.token)
                require(status == 200, "ready endpoint rejected this daemon's token")
                require(value.get("ok") is True and value.get("backend") == "rust" and
                        value.get("go_backend_connected") is False and
                        value.get("go_backend_phase") == "retired", "wrong daemon runtime")
                recovery = value.get("module_recovery", {})
                require(recovery.get("state") == "legacy_recovery_required" and
                        recovery.get("historical_recovery_confirmed") is False,
                        "synthetic unknown module history was not isolated")
                require(not Path(str(self.public) + ".go").exists(), "unexpected Go socket")
                self.ready_observed = True
                self.record["ready_runtime"] = value
                return self.snapshot()
            except (OSError, http.client.HTTPException):
                time.sleep(0.03)
        self.record["timeout"] = True
        raise AssertionError("daemon readiness timeout: " + self.record["label"])

    def snapshot(self):
        status, value = request(self.public, self.token)
        require(self.process.poll() is None and status == 200, "first daemon lost its authenticated runtime")
        return {"runtime": value, "sockets": sockets(self.public),
                "stores": BASE.hashes(self.root),
                "lock": metadata(self.root / LOCK),
                "token_sha256": self.record["token_sha256"]}

    def unchanged(self, before, rejected_token=None):
        require(self.snapshot() == before, "first daemon runtime/token/socket/store identity changed")
        if rejected_token is not None:
            status, _ = request(self.public, rejected_token)
            require(status == 401, "challenger token replaced or bypassed first daemon authentication")

    def close(self):
        require(self.ready_observed, "close requires a ready daemon")
        require(self.process.poll() is None, "ready daemon exited without requested SIGTERM")
        self.process.send_signal(signal.SIGTERM)
        require(self.wait(10) == 0, "SIGTERM shutdown did not exit zero")
        require("switchd-rs: native shutdown complete" in self.log_text(), "missing shutdown completion receipt")
        require(all(not path.exists() for path in socket_paths(self.public)), "owned socket survived SIGTERM")
        self.record["sigterm_shutdown_confirmed"] = True

    def cleanup(self):
        # This object's start_new_session group is the only eligible target.
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.record["timeout"] = True
                self.record["forced_cleanup"] = True
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=2)
        if self.group_exists():
            self.record["surviving_owned_descendants"] = True
            os.killpg(self.process.pid, signal.SIGTERM)
            until = time.monotonic() + 2
            while self.group_exists() and time.monotonic() < until:
                time.sleep(0.02)
            if self.group_exists():
                self.record["forced_cleanup"] = True
                os.killpg(self.process.pid, signal.SIGKILL)
                until = time.monotonic() + 2
                while self.group_exists() and time.monotonic() < until:
                    time.sleep(0.02)
        self.update()
        require(not self.group_exists(), "owned group cleanup unconfirmed")


class Suite:
    def __init__(self, binary, report, *, runtime_parent=None):
        runtime = BUILD / "runtime"
        runtime.mkdir(parents=True, exist_ok=True)
        canary_parent = runtime
        if runtime_parent is not None:
            # Cold capsules can exceed Darwin's AF_UNIX path limit. Accept
            # only an already-created private parent, never a shared temp root.
            canary_parent = Path(runtime_parent)
            metadata = canary_parent.lstat()
            require(canary_parent.is_absolute() and canary_parent.resolve(strict=True) == canary_parent
                    and stat.S_ISDIR(metadata.st_mode)
                    and metadata.st_uid == os.geteuid()
                    and stat.S_IMODE(metadata.st_mode) == 0o700,
                    "short runtime parent must be an owned private directory")
        self.runtime_parent = canary_parent
        self.path = Path(tempfile.mkdtemp(prefix="dl573-", dir=runtime)).resolve()
        # Rust crash fixtures retain compile-time build/runtime containment.
        # Only canary Unix sockets need the short private path in cold exports.
        self.canary_root = (self.path if runtime_parent is None else
                            Path(tempfile.mkdtemp(prefix="dlc-", dir=canary_parent)).resolve())
        self.logs = self.path / "logs"
        self.logs.mkdir(mode=0o700)
        self.binary, self.report, self.children = binary, report, []
        self.next_root = 0
        self.profile = self.path / "hardware-denied.sb"
        profile = BASE.sandbox_profile(self.path) + "(deny iokit-open-user-client)\n"
        if self.canary_root != self.path:
            canary_path = json.dumps(str(self.canary_root))
            profile += f"(allow file-write* (subpath {canary_path}))\n"
            profile += f"(allow network-outbound (subpath {canary_path}))\n"
        self.profile.write_text(profile)
        report["runtime_root"] = str(self.path)
        report["sandbox_profile"] = str(self.profile)
        report["sandbox_profile_sha256"] = sha(self.profile)
        report["sandbox_canary_root"] = str(self.canary_root)

    def canary(self):
        # The blocked socket is outside the suite allow-list, but still under
        # Switch build/runtime. This must pass before any daemon is spawned.
        with tempfile.TemporaryDirectory(prefix="dlb-", dir=self.runtime_parent) as outside:
            blocked_path = str(Path(outside) / "b")
            with socket.socket(socket.AF_UNIX) as blocked, socket.socket(socket.AF_UNIX) as allowed:
                blocked.bind(blocked_path)
                blocked.listen(4)
                allowed.bind(str(self.canary_root / "allowed"))
                allowed.listen(4)
                process = subprocess.Popen(
                    ["/usr/bin/sandbox-exec", "-f", str(self.profile), sys.executable,
                     str(SELF), "--canary", str(self.canary_root), blocked_path],
                    env=BASE.environment(self.path), stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE, text=True, start_new_session=True)
                self.canary_process = process
                row = {"pid": process.pid, "inherited_child": True,
                       "timeout": False, "forced_cleanup": False}
                self.report["sandbox_canary"] = row
                try:
                    output, errors = process.communicate(timeout=20)
                except subprocess.TimeoutExpired:
                    row["timeout"] = True
                    row["forced_cleanup"] = True
                    os.killpg(process.pid, signal.SIGKILL)
                    output, errors = process.communicate(timeout=2)
                try:
                    os.killpg(process.pid, 0)
                except ProcessLookupError:
                    row["owned_process_group_gone"] = True
                else:
                    row["surviving_owned_descendants"] = True
                    row["forced_cleanup"] = True
                    os.killpg(process.pid, signal.SIGKILL)
            (self.canary_root / "allowed").unlink()
        path = self.logs / "sandbox-canary.log"
        path.write_text(output + errors)
        row.update(exit_code=process.returncode, reaped=True, log=str(path), log_sha256=sha(path))
        require(process.returncode == 0 and not row["forced_cleanup"],
                "mandatory hardware/network sandbox canary failed")

    def cleanup_canary(self):
        process = getattr(self, "canary_process", None)
        if process is None:
            return
        row = self.report["sandbox_canary"]
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return
        row["interrupted_cleanup"] = True
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            row["timeout"] = True
            row["forced_cleanup"] = True
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=2)
        until = time.monotonic() + 2
        while time.monotonic() < until:
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.02)
        row["forced_cleanup"] = True
        os.killpg(process.pid, signal.SIGKILL)
        raise AssertionError("owned canary descendants did not terminate")

    def root(self):
        root = self.path / ("r" + str(self.next_root))
        self.next_root += 1
        root.mkdir(mode=0o700)
        BASE.seed(root)
        # No pristine marker: this known synthetic old lock forces module
        # isolation before radio/ADB/QMI startup, in addition to kernel denies.
        (root / "call-mode-operation.lock").write_bytes(b"")
        for directory, _, files in os.walk(root):
            for name in files:
                os.chmod(Path(directory) / name, 0o600)
        return root

    def spawn(self, label, root, public=None, gate=None):
        return Daemon(self, label, root, public, gate)

    def case(self, name, operation):
        row = {"name": name, "passed": False}
        self.report["scenarios"].append(row)
        before = len(self.report["processes"])
        try:
            operation(row)
            row["passed"] = True
        except Exception as error:
            row["error"] = type(error).__name__ + ": " + str(error)
            raise
        finally:
            # Keep exact exit/log records attached to each scenario as well as
            # in the top-level process ledger. Dicts update through final reap.
            row["processes"] = self.report["processes"][before:]


def run_matrix(suite):
    def before_recovery(row):
        # Claim both locks in the same order before re-seeding actual historic
        # submitting/running records. A real loser must not normalize them.
        root = suite.root()
        directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        lock = None
        try:
            fcntl.flock(directory, fcntl.LOCK_EX | fcntl.LOCK_NB)
            lock = os.open(root / LOCK, os.O_RDWR | os.O_CREAT | os.O_EXCL |
                           os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            BASE.seed(root)
            before = BASE.hashes(root)
            evidence = evidence_tree(root)
            child = suite.spawn("claim-before-recovery", root)
            child.expect_failure(BUSY)
            require(BASE.hashes(root) == before, "loser recovered submitting/running stores before lifetime claim")
            require(evidence_tree(root) == evidence, "loser mutated a claimed synthetic data root")
            row["original_store_sha256"] = before
            row["evidence_before"] = evidence
            row["store_recovery_not_reached"] = True
        finally:
            if lock is not None:
                os.close(lock)
            os.close(directory)
    suite.case("lifetime_claim_precedes_submitting_and_running_store_recovery", before_recovery)

    root = suite.root()
    first = suite.spawn("first", root)
    stable = first.ready()
    suite.report["first_baseline"] = stable

    def uds_auth(row):
        row["socket_modes"] = {role: oct(stat.S_IMODE(path.lstat().st_mode))
                               for role, path in zip(["public", "callback", "internal"],
                                                     socket_paths(first.public))}
        row["rejections"] = unauthorized_surfaces(first.public, first.token)
        first.unchanged(stable)
    suite.case("three_uds_modes_and_missing_or_wrong_credentials", uds_auth)

    for name, candidate_root, public in [
        ("same_root_same_socket", root, first.public),
        ("same_root_other_socket", root, root / "other"),
    ]:
        def check(row, name=name, candidate_root=candidate_root, public=public):
            second = suite.spawn(name, candidate_root, public)
            second.expect_failure(BUSY)
            first.unchanged(stable, second.token)
            row["first_unchanged"] = True
            if public != first.public:
                require(all(not path.exists() for path in socket_paths(public)), "busy loser created sockets")
        suite.case(name, check)
    alias = suite.path / "alias"
    alias.symlink_to(root, target_is_directory=True)

    def alias_case(row):
        second = suite.spawn("same_root_alias", alias, alias / "s")
        second.expect_failure(BUSY)
        first.unchanged(stable, second.token)
        row["first_unchanged"] = True
    suite.case("same_inode_directory_alias", alias_case)

    def parallel_roots(row):
        independent = suite.spawn("different_root", suite.root())
        before = independent.ready()
        first.unchanged(stable, independent.token)
        independent.unchanged(before, first.token)
        require(before["lock"]["inode"] != stable["lock"]["inode"], "different roots shared a lock entry")
        row["independent_baseline"] = before
        independent.close()
        first.unchanged(stable)
    suite.case("different_roots_can_run_in_parallel", parallel_roots)

    def concurrent_start(row):
        shared = suite.root()
        reader, writer = os.pipe()
        try:
            contenders = [suite.spawn("race-" + str(index), shared, gate=reader) for index in range(2)]
            require(os.write(writer, b"gg") == 2, "concurrent start gate short write")
        finally:
            os.close(reader)
            os.close(writer)
        until = time.monotonic() + 5
        while all(child.process.poll() is None for child in contenders) and time.monotonic() < until:
            time.sleep(0.02)
        winners = [child for child in contenders if child.process.poll() is None]
        losers = [child for child in contenders if child.process.poll() is not None]
        require(len(winners) == 1 and len(losers) == 1, "concurrent same-root start did not yield exactly one winner")
        losers[0].expect_failure(BUSY)
        row["winner_pid"] = winners[0].process.pid
        row["loser_pid"] = losers[0].process.pid
        winners[0].ready()
        require(request(winners[0].public, losers[0].token)[0] == 401, "race loser token was admitted")
        winners[0].close()
        first.unchanged(stable)
    suite.case("concurrent_same_root_exactly_one_winner", concurrent_start)

    def stale_concurrent_reclaim(row):
        shared = suite.path / "shared-stale"
        for path in socket_paths(shared):
            require(len(os.fsencode(path)) < 100, "shared stale socket path too long")
        with socket.socket(socket.AF_UNIX) as stale:
            stale.bind(str(shared))
        row["stale_socket_before"] = metadata(shared)
        roots = [suite.root(), suite.root()]
        reader, writer = os.pipe()
        try:
            contenders = [suite.spawn("stale-race-" + str(index), roots[index], shared, gate=reader)
                          for index in range(2)]
            require(os.write(writer, b"gg") == 2, "stale reclaim start gate short write")
        finally:
            os.close(reader)
            os.close(writer)
        until = time.monotonic() + 5
        while all(child.process.poll() is None for child in contenders) and time.monotonic() < until:
            time.sleep(0.02)
        winners = [child for child in contenders if child.process.poll() is None]
        losers = [child for child in contenders if child.process.poll() is not None]
        require(len(winners) == 1 and len(losers) == 1,
                "different-root concurrent stale reclaim did not yield exactly one winner")
        loser, winner = losers[0], winners[0]
        loser.expect_failure(ACTIVE_SOCKET)
        before = winner.ready()
        winner.unchanged(before, loser.token)
        require(winner.root != loser.root, "stale reclaim fixture reused the data root")
        row["winner_pid"] = winner.process.pid
        row["loser_pid"] = loser.process.pid
        row["winner_baseline"] = before
        companions = {str(path) + ".switch-listener.lock": metadata(Path(str(path) + ".switch-listener.lock"))
                      for path in socket_paths(shared)}
        for facts in companions.values():
            require(stat.S_ISREG(facts["mode"]) and facts["size"] == 0 and facts["nlink"] == 1,
                    "listener companion must be an empty single-link regular file")
        row["companion_locks_before_shutdown"] = companions
        winner.close()
        for name, facts in companions.items():
            after = metadata(Path(name))
            require((after["device"], after["inode"]) == (facts["device"], facts["inode"]),
                    "normal stop removed or replaced listener companion")
        restarted = suite.spawn("stale-race-restarted-other-root", suite.root(), shared)
        after = restarted.ready()
        require(request(shared, winner.token)[0] == 401,
                "different-root listener takeover retained prior authentication")
        for name, facts in companions.items():
            current = metadata(Path(name))
            require((current["device"], current["inode"]) == (facts["device"], facts["inode"]),
                    "different-root takeover replaced listener companion")
        require(BASE.hashes(winner.root) == before["stores"], "takeover changed old root's static stores")
        restarted.unchanged(after, loser.token)
        row["restart_baseline"] = after
        row["companion_inodes_retained"] = True
        restarted.close()
        first.unchanged(stable)
    suite.case("different_roots_concurrent_stale_reclaim_and_later_takeover", stale_concurrent_reclaim)

    lock_identity = (stable["lock"]["device"], stable["lock"]["inode"])

    def restart(row):
        first.close()
        require((root / LOCK).is_file(), "normal exit removed lifetime lock")
        restarted = suite.spawn("restarted", root)
        after = restarted.ready()
        require((after["lock"]["device"], after["lock"]["inode"]) == lock_identity,
                "normal restart replaced lifetime lock inode")
        require(after["stores"] == stable["stores"], "normal restart changed static store payload")
        require(request(restarted.public, first.token)[0] == 401, "restart retained prior process authentication")
        row["after"] = after
        row["process"] = restarted.record["label"]
        return restarted
    holder = {}
    suite.case("sigterm_exit_then_restart_preserves_lock_inode", lambda row: holder.update(child=restart(row)))
    current = holder["child"]
    current_stable = current.snapshot()

    for role, target in zip(["public", "callback", "internal"], socket_paths(current.public)):
        def collision(row, role=role, target=target):
            challenger = suite.spawn("foreign-" + role, suite.root(), target)
            challenger.expect_failure(ACTIVE_SOCKET)
            current.unchanged(current_stable, challenger.token)
            row["foreign_socket_role"] = role
            row["collision_at_bind_stage"] = "public"
            row["first_unchanged"] = True
        suite.case("foreign_active_" + role + "_socket_preserved", collision)

    # Exercise cleanup after this daemon has already bound its own earlier
    # listeners. The existing foreign public path is the callback/internal
    # candidate's exact target, without hiding an earlier public collision.
    for stage, suffix in [("callback", ".rs"), ("internal", ".in")]:
        def partial(row, stage=stage, suffix=suffix):
            owner_root = suite.root()
            own_prefix = owner_root / "p"
            foreign = suite.spawn("partial-owner-" + stage, owner_root, Path(str(own_prefix) + suffix))
            before = foreign.ready()
            challenger = suite.spawn("partial-challenger-" + stage, suite.root(), own_prefix)
            challenger.expect_failure(ACTIVE_SOCKET)
            foreign.unchanged(before, challenger.token)
            require(not own_prefix.exists(), "failed startup left its own public socket")
            if stage == "internal":
                require(not Path(str(own_prefix) + ".rs").exists(), "failed startup left its own callback socket")
            current.unchanged(current_stable)
            row["collision_at_bind_stage"] = stage
            row["foreign_baseline"] = before
            foreign.close()
        suite.case("partial_bind_" + stage + "_collision_cleans_only_owned_paths", partial)

    for kind in ["nonempty", "directory", "symlink", "hardlink"]:
        def invalid_lock(row, kind=kind):
            invalid_root = suite.root()
            lock = invalid_root / LOCK
            target = suite.path / ("evidence-" + kind)
            target.write_bytes(b"" if kind == "hardlink" else b"synthetic evidence; preserve")
            if kind == "nonempty":
                lock.write_bytes(b"synthetic nonempty lifetime evidence")
            elif kind == "directory":
                lock.mkdir()
            elif kind == "symlink":
                lock.symlink_to(target)
            else:
                os.link(target, lock)
            before = evidence_tree(invalid_root)
            target_before = metadata(target)
            child = suite.spawn("invalid-lock-" + kind, invalid_root)
            child.expect_failure(UNSAFE_LOCK)
            require(evidence_tree(invalid_root) == before, "rejected lock reached store recovery or rewrote evidence")
            require(metadata(target) == target_before, "rejected lock changed linked evidence")
            require(all(not path.exists() for path in socket_paths(child.public)), "rejected lock created listeners")
            current.unchanged(current_stable)
            row["evidence_before"] = before
            row["linked_evidence_before"] = target_before
            row["stores_before_and_after"] = BASE.hashes(invalid_root)
            row["store_recovery_not_reached"] = True
        suite.case("unsafe_" + kind + "_lock_rejected_before_stores", invalid_lock)
    current.close()


def source_hashes():
    paths = [SELF, HELPER_PATH, CONTRACT_PATH, CONTRACT_REVIEW_PATH,
             CONTRACT_INVENTORY_PATH, SOURCE / "rust/crates/proletariusd/src/main.rs",
             SOURCE / "rust/crates/proletariusd/src/daemon_lifetime.rs",
             SOURCE / "rust/crates/proletariusd/src/socket_lifetime.rs",
             SOURCE / "rust/crates/proletariusd/src/data_root.rs",
             SOURCE / "rust/crates/proletariusd/src/gateway.rs",
             SOURCE / "rust/crates/proletariusd/src/module_recovery.rs",
             SOURCE / "rust/crates/proletariusd/src/shutdown.rs",
             SOURCE / "rust/crates/proletariusd/tests/native_recovery_tests.rs"]
    return {str(path.relative_to(SOURCE)): sha(path) for path in paths}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    reports = (BUILD / "reports").resolve()
    reports.mkdir(parents=True, exist_ok=True)
    destination = args.report.absolute()
    require(not destination.is_symlink() and destination.resolve().is_relative_to(reports),
            "report must be a non-symlink JSON file under Switch build/reports")
    require(destination.suffix == ".json", "report extension must be .json")
    report = {"passed": False, "validation_completed": False, "synthetic_only": True,
              "full_p6_acceptance": False, "overall_stage": "P5完善中；P6最终验收未完成",
              "scope": "participating current daemon data-root and socket lifetime; no legacy execution or hardware recovery",
              "scenarios": [], "processes": []}
    suite = None
    try:
        require(sys.platform == "darwin", "mandatory Darwin sandbox required")
        binary = args.binary.resolve(strict=True)
        require(binary.is_relative_to(WORKSPACE) and binary.is_file() and os.access(binary, os.X_OK),
                "binary must be an executable file in this Switch workspace")
        report["binary"] = str(binary)
        report["binary_sha256"] = sha(binary)
        report["source_sha256"] = source_hashes()
        report["interface_ownership"] = interface_ownership()
        suite = Suite(binary, report)
        suite.canary()
        run_matrix(suite)
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
            try:
                suite.cleanup_canary()
            except Exception as error:
                cleanup_errors.append("sandbox canary: " + str(error))
            report["synthetic_evidence_retained"] = str(suite.path)
        report["cleanup_errors"] = cleanup_errors
        try:
            if "binary_sha256" in report:
                require(sha(Path(report["binary"])) == report["binary_sha256"], "binary changed during validation")
                require(source_hashes() == report["source_sha256"], "scoped source inputs changed during validation")
                require(interface_ownership() == report["interface_ownership"],
                        "interface ownership census changed during validation")
        except Exception as error:
            report["fingerprint_error"] = str(error)
        bad_cleanup = any(child.get("forced_cleanup") or child.get("timeout") or
                          child.get("surviving_owned_descendants") for child in report["processes"])
        report["passed"] = (report["validation_completed"] and not report.get("error") and
                            not report.get("fingerprint_error") and not cleanup_errors and not bad_cleanup and
                            all(row["passed"] for row in report["scenarios"]))
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "report": str(destination),
                      "scenario_count": len(report["scenarios"]), "error": report.get("error")}, ensure_ascii=False))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    if sys.argv[1:2] == ["--canary"]:
        canary(Path(sys.argv[2]), sys.argv[3], "--inherited" in sys.argv[4:])
    elif sys.argv[1:2] == ["--gated"]:
        gated_exec(sys.argv[2:])
    else:
        raise SystemExit(main())
