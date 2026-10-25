#!/usr/bin/env python3
"""Pure offline failure-path test; mutates only fresh mock input files."""
import json
from pathlib import Path
import subprocess
import sys


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: test-capsule-app-drift.py NEW_ABSOLUTE_REPORT_DIRECTORY")
    output = Path(sys.argv[1])
    output.mkdir(mode=0o700)
    driver = Path(__file__).with_name("test-capsule-app.py")
    cases = []
    for case in ["stable", "source", "fixture", "driver", "added", "deleted"]:
        target = output / case
        command = [sys.executable, str(driver), "--input-drift-fixture", case, str(target)]
        result = subprocess.run(command, text=True, capture_output=True)
        summary = json.loads((target / "summary.json").read_text())
        hashes = json.loads((target / "input-hashes.json").read_text())
        expected_pass = case == "stable"
        assert (result.returncode == 0) == expected_pass, (case, result.stderr)
        assert summary["passed"] is expected_pass, case
        assert hashes["inputs_stable_for_entire_run"] is expected_pass, case
        assert bool(hashes["changed_inputs"]) != expected_pass, case
        if not expected_pass:
            assert '\"passed\": true' not in (target / "summary.json").read_text().lower(), case
        cases.append({"case": case, "command": command, "exit_code": result.returncode,
                      "summary_passed": summary["passed"], "changed_inputs": hashes["changed_inputs"]})
    (output / "selftest.json").write_text(json.dumps({"passed": True, "count": len(cases),
        "real_source_mutated": False, "cases": cases}, indent=2) + "\n")
    print("PASS: 6 offline mock cases: stable control; source/fixture/driver/add/delete drift fail nonzero without passed=true")


if __name__ == "__main__":
    main()
