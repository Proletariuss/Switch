"""Create-only local test receipts. No historical report may be resumed."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import shutil


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_json(path, value):
    with Path(path).open("x") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


def create_report(raw, workspace):
    path = Path(raw)
    reports = (workspace / "build/reports").resolve(strict=True)
    if not path.is_absolute() or not path.resolve().is_relative_to(reports) or path.resolve() == reports:
        raise ValueError("report must be a new absolute directory below workspace build/reports")
    # mkdir without exist_ok is the atomic ownership decision. No reads of an
    # existing report, compiler invocation, write or cleanup precedes it.
    path.mkdir(mode=0o700)
    return path


def offline_environment(output):
    original_home = Path.home()
    for name in ("home", "tmp"):
        (output / name).mkdir(mode=0o700)
    return {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
            "HOME": str(output / "home"), "CFFIXED_USER_HOME": str(output / "home"),
            "TMPDIR": str(output / "tmp"), "PYTHONDONTWRITEBYTECODE": "1",
            "CARGO_NET_OFFLINE": "true", "RUSTUP_AUTO_INSTALL": "0",
            "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(original_home / ".rustup")),
            "CARGO_HOME": os.environ.get("CARGO_HOME", str(original_home / ".cargo"))}


def run_command(output, label, argv, cwd, env, timeout=240, expected=0):
    command = [os.fsdecode(v) for v in argv]
    receipt = {"command": command, "network": "sandbox deny network*", "timeout_seconds": timeout}
    write_json(output / (label + ".command.json"), receipt)
    start = time.monotonic()
    code, failure = None, None
    with (output / (label + ".log")).open("xb") as stdout, (output / (label + ".stderr")).open("xb") as stderr:
        try:
            result = subprocess.run(["/usr/bin/sandbox-exec", "-p", "(version 1)(allow default)(deny network*)"] + list(argv),
                                    cwd=cwd, env=env, stdout=stdout, stderr=stderr, timeout=timeout)
            code = result.returncode
        except Exception as exc:
            failure = repr(exc)
    receipt.update(exit_code=code, failure=failure, seconds=round(time.monotonic() - start, 3),
                   stdout_sha256=sha(output / (label + ".log")), stderr_sha256=sha(output / (label + ".stderr")))
    write_json(output / (label + ".exit.json"), receipt)
    if failure is not None or (expected is not None and code != expected):
        raise RuntimeError("failed " + label + ": " + str(failure or code))
    return receipt


def tree_snapshot(roots):
    result = {}
    for root in roots:
        root = Path(root)
        if not root.exists():
            result[str(root)] = {"kind": "absent"}
            continue
        paths = [root] if root.is_file() else [root] + sorted(root.rglob("*"))
        for path in paths:
            if "__pycache__" in path.parts or "target" in path.parts:
                continue
            if path.is_symlink():
                if path.is_dir():
                    raise RuntimeError("directory symlink requires an explicit inventory root: " + str(path))
                result[str(path)] = {"kind": "symlink", "target": os.readlink(path),
                                     "sha256": sha(path) if path.is_file() else None}
            elif path.is_file():
                result[str(path)] = {"kind": "file", "sha256": sha(path)}
            else:
                result[str(path)] = {"kind": "directory"}
    return result


def finish_inputs(output, before, snapshot, report, error=None):
    try:
        after = snapshot()
        changed = sorted(k for k in before.keys() | after.keys() if before.get(k) != after.get(k))
    except Exception as exc:
        after, changed = {}, ["snapshot-error: " + repr(exc)]
    write_json(output / "inputs-after.json", after)
    source_stable = not changed
    actual = report.get("actual_generated_inputs")
    if actual is not None and not actual["stable_at_boundaries"]:
        changed.extend("actual-generated:" + path for path in actual["changed_paths"] or ["incomplete-baseline-or-snapshot"])
    report.update(passed=error is None and not changed, changed_inputs=changed,
                  source_inputs_stable_at_boundaries=source_stable, inputs_stable_for_entire_run=not changed)
    if error is not None:
        report["failure"] = repr(error)
        report["execution_failure"] = repr(error)
    if changed:
        report["failure"] = "input drift; no passing credit"
    write_json(output / "report.json", report)
    return 0 if report["passed"] else 1


def clean_owned_caches(output, names):
    rows = []
    for name in names:
        cache = output / name
        if not cache.exists():
            continue
        if cache.is_symlink() or cache.parent != output:
            raise RuntimeError("refusing non-owned cache")
        holders = subprocess.run(["/usr/sbin/lsof", "+D", str(cache)], capture_output=True, text=True)
        if holders.returncode not in (0, 1) or holders.stdout.strip():
            raise RuntimeError("compiler cache is held; leave for review")
        size = sum(p.stat().st_size for p in cache.rglob("*") if p.is_file())
        shutil.rmtree(cache)
        rows.append({"path": str(cache), "bytes": size, "lsof_exit": holders.returncode,
                     "exists_after_cleanup": cache.exists()})
    write_json(output / "cleanup.json", rows)
