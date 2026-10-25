#!/usr/bin/env python3
"""Run local fixture tests with exclusive roots; retain every failed audit."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time


# Reviewed allowance: the expanded native service filter takes about 280s.
# 420s permits 50% headroom; other filters retain the original 180s budget.
FILTER_TIMEOUTS = {"native_vowifi_session::service::tests::": 420}
SUMMARY = re.compile(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;(?: finished in .*)?$", re.MULTILINE)


def parent_summary(text):
    matches = [{"status": m[1], "passed": int(m[2]), "failed": int(m[3]),
                "ignored": int(m[4]), "measured": int(m[5]), "filtered_out": int(m[6]),
                "offset": m.start(), "text": m[0]} for m in SUMMARY.finditer(text)]
    # libtest prints the parent's summary after every joined nested child.
    # A final FAILED summary remains authoritative even if a child passed.
    counts = re.findall(r"^running (\d+) tests?$", text, re.MULTILINE)
    final = matches[-1] if matches else None
    if not counts or final is None or sum(final[k] for k in ("passed", "failed", "ignored", "measured")) != int(counts[0]):
        final = None
    return matches, final


def inventory(root):
    rows = []
    for path in sorted(root.rglob("*")):
        mode = path.lstat().st_mode
        kind = "directory" if stat.S_ISDIR(mode) else "file"
        if stat.S_ISLNK(mode):
            kind = "symlink"
        row = {"path": str(path.relative_to(root)), "kind": kind}
        if kind == "directory":
            row["empty"] = not any(path.iterdir())
        rows.append(row)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report-dir", type=Path, required=True)
    parser.add_argument("--runtime-parent", type=Path, required=True)
    parser.add_argument("--filter", action="append", dest="filters")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.report_dir.mkdir(parents=True, exist_ok=True)
    args.runtime_parent.mkdir(parents=True, exist_ok=True)
    # Keep TMPDIR short enough for Darwin Unix sockets. Full PID/time remain
    # recorded below; exclusive mkdir refuses a collision instead of reusing it.
    batch = f"g3{os.getpid():x}{time.time_ns() & 0xffff:04x}"
    root = args.runtime_parent / batch
    root.mkdir()  # Never reuse an existing runtime or a historical fallback.
    report = args.report_dir / f"{batch}.json"
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    result = {"schema": 1, "runner_pid": os.getpid(), "created_ns": time.time_ns(),
              "owned_root": str(root), "binary": str(binary),
              "binary_sha256": binary_hash, "filters": []}
    filters = args.filters or [
        "g3_", "native_vowifi_session::service::tests::",
        "native_vowifi_session::service::startup_http_tests::",
        "native_vowifi_session::module::persistence_tests::",
    ]
    for index, test_filter in enumerate(filters):
        case = root / str(index)
        case.mkdir()
        roots = {name: case / name for name in ("tmp", "build", "data")}
        for directory in roots.values():
            directory.mkdir()
        before = {name: inventory(path) for name, path in roots.items()}
        assert not any(before.values())
        env = {**os.environ, "TMPDIR": str(roots["tmp"]),
               "SWITCH_BUILD_DIR": str(roots["build"]),
               "SWITCH_DATA_DIR": str(roots["data"]),
               "PYTHONDONTWRITEBYTECODE": "1"}
        log_path = args.report_dir / f"{batch}-{index}.log"
        argv = [str(binary), test_filter, "--nocapture", "--test-threads=1"]
        start = time.monotonic()
        with log_path.open("x") as log:
            process = subprocess.Popen(argv, env=env, stdout=log, stderr=subprocess.STDOUT)
            timed_out = False
            try:
                code = process.wait(timeout=FILTER_TIMEOUTS.get(test_filter, 180))
            except subprocess.TimeoutExpired:
                timed_out = True
                process.kill()
                code = process.wait()
        after = {name: inventory(path) for name, path in roots.items()}
        summaries, completed = parent_summary(log_path.read_text())
        passed_tests = completed["passed"] if completed else 0
        row = {"filter": test_filter, "pid": process.pid, "argv": argv,
               "roots": {name: str(path) for name, path in roots.items()},
               "before": before, "after": after, "exit": code,
               "timed_out": timed_out, "seconds": round(time.monotonic() - start, 3),
               "timeout_seconds": FILTER_TIMEOUTS.get(test_filter, 180),
               "all_summaries": summaries, "parent_final_summary": completed,
               "passed_tests": passed_tests,
               "log": str(log_path),
               "zero_residuals": not any(after.values())}
        result["filters"].append(row)
        report.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({"filter": test_filter, "exit": code,
                          "zero_residuals": row["zero_residuals"],
                          "report": str(report)}), flush=True)
    result["binary_unchanged"] = hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash
    result["passed"] = result["binary_unchanged"] and all(
        row["exit"] == 0 and not row["timed_out"] and row["parent_final_summary"] is not None
        and row["parent_final_summary"]["status"] == "ok" and row["parent_final_summary"]["failed"] == 0
        and row["passed_tests"] > 0 and row["zero_residuals"]
        for row in result["filters"])
    report.write_text(json.dumps(result, indent=2) + "\n")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
