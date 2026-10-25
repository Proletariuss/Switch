#!/usr/bin/env python3
"""Real offline production-run_batch negative controls in a new private cache."""
import importlib.util
import json
from pathlib import Path
import sys

from evidence import sha, write_json


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("stable", "edit", "add", "delete", "early-failure"):
        raise SystemExit("usage: test-daemon-generated-input-drift.py stable|edit|add|delete|early-failure NEW_ABSOLUTE_REPORT")
    case, raw = sys.argv[1:]
    output = Path(raw)
    spec = importlib.util.spec_from_file_location("production_daemon_runner", Path(__file__).with_name("test-daemon-capsule.py"))
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    original = runner.run_command
    injected = False

    def command(out, label, *args, **kwargs):
        nonlocal injected
        if label == "swift-live-kernel-chain" and case != "stable":
            if injected:
                raise RuntimeError("duplicate drift injection")
            injected = True
            # The production run_batch and real compilers/fixtures remain intact.
            # Mutate only this new attempt's actual serde OUT_DIR after its baseline.
            candidates = sorted((out / "compiler-cache/debug/build").glob("serde-*/out/private.rs"))
            if len(candidates) != 1:
                raise RuntimeError("expected one actual generated serde private.rs")
            path = candidates[0]
            receipt = {"case": case, "path": str(path.relative_to(out / "compiler-cache")),
                       "before_sha256": sha(path), "production_run_batch_replaced": False}
            if case in ("edit", "early-failure"):
                with path.open("ab") as stream:
                    stream.write(b"\n// private actual OUT_DIR persistent drift negative control\n")
            elif case == "add":
                path = path.with_name("negative-control-added.rs")
                with path.open("xb") as stream:
                    stream.write(b"// private newly added actual OUT_DIR input\n")
                receipt["path"] = str(path.relative_to(out / "compiler-cache"))
                receipt["before_sha256"] = None
            elif case == "delete":
                path.unlink()
            receipt["after_sha256"] = sha(path) if path.exists() else None
            write_json(out / "injection.json", receipt)
            if case == "early-failure":
                raise RuntimeError("negative control: command failure after actual OUT_DIR mutation")
        return original(out, label, *args, **kwargs)

    runner.run_command = command
    sys.argv = [str(Path(__file__).with_name("test-daemon-capsule.py")), raw]
    status = runner.main()
    report = json.loads((output / "report.json").read_text())
    before = json.loads((output / "actual-generated-before.json").read_text())
    after = json.loads((output / "actual-generated-after.json").read_text())
    expected = status == 0 and report["passed"] is True if case == "stable" else (
        injected and status != 0 and report["passed"] is False and before["entries"] != after["entries"])
    write_json(output / "negative-control-verdict.json", {"case": case, "runner_exit": status,
               "runner_passed": report["passed"], "actual_before_after_differ": before["entries"] != after["entries"],
               "control_expectation_met": expected, "real_compilers_and_production_run_batch": True})
    return status


if __name__ == "__main__":
    raise SystemExit(main())
