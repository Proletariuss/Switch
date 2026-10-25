#!/usr/bin/env python3
"""Offline scaffold tests with a fresh report and retained compiler receipts."""
import os
from pathlib import Path
import sys

from evidence import clean_owned_caches, create_report, finish_inputs, offline_environment, run_command, sha, tree_snapshot, write_json

SOURCE = Path(__file__).resolve().parents[2]


def main():
    raw = sys.argv[1] if len(sys.argv) == 2 else os.environ.get("SWITCH_PHASE_A_REPORT")
    if not raw or len(sys.argv) > 2:
        raise SystemExit("usage: test-acceptance-companion-phase-a.sh NEW_ABSOLUTE_REPORT (or explicit SWITCH_PHASE_A_REPORT)")
    output = create_report(raw, SOURCE.parent)
    env = offline_environment(output)
    for name in ("cache", "bin"):
        (output / name).mkdir(mode=0o700)
    paths = [SOURCE / "app/Sources/AcceptanceCompanionPhaseA.swift",
             SOURCE / "app/Tests/AcceptanceCompanionPhaseA", Path(__file__),
             SOURCE / "scripts/acceptance/evidence.py", SOURCE / "scripts/test-acceptance-companion-phase-a.sh",
             Path(sys.executable), Path("/usr/bin/xcrun"), Path("/usr/bin/sandbox-exec")]
    snapshot = lambda: tree_snapshot(paths)
    before = snapshot()
    write_json(output / "inputs-before.json", before)
    error = None
    try:
        run_command(output, "swift-version", ["/usr/bin/xcrun", "swiftc", "--version"], SOURCE, env)
        run_command(output, "swift-path", ["/usr/bin/xcrun", "--find", "swiftc"], SOURCE, env)
        swift = Path((output / "swift-path.log").read_text().strip())
        run_command(output, "sdk-path", ["/usr/bin/xcrun", "--show-sdk-path"], SOURCE, env)
        env["SDKROOT"] = (output / "sdk-path.log").read_text().strip()
        paths.extend([Path(env["SDKROOT"]) / "SDKSettings.json", Path(env["SDKROOT"]) / "SDKSettings.plist"])
        paths.append(swift)
        before = snapshot()
        write_json(output / "compiler-inputs-before.json", before)
        binary = output / "bin/acceptance-companion-phase-a"
        run_command(output, "compile", [str(swift), "-parse-as-library", "-swift-version", "5", "-D", "SWITCH_ACCEPTANCE_PHASE_A_TESTS",
                    "-module-cache-path", str(output / "cache"), str(paths[0]),
                    str(SOURCE / "app/Tests/AcceptanceCompanionPhaseA/main.swift"), "-o", str(output / "cache/compiled-binary")], SOURCE, env)
        with binary.open("xb") as stream:
            stream.write((output / "cache/compiled-binary").read_bytes())
        binary.chmod(0o700)
        binary_before = sha(binary)
        run_command(output, "tests", [str(binary)], SOURCE, env)
        write_json(output / "binary-hashes.json", {"before_tests": binary_before, "after_tests": sha(binary)})
        if binary_before != sha(binary):
            raise RuntimeError("binary drift")
    except Exception as exc:
        error = exc
    try:
        clean_owned_caches(output, ["cache"])
    except Exception as exc:
        error = exc
    return finish_inputs(output, before, snapshot, {"scope": "phase-A-scaffold-only", "formal_duration_credit_seconds": 0,
                                                  "g8_g9_admission": "NO-GO"}, error)


if __name__ == "__main__":
    raise SystemExit(main())
