#!/usr/bin/env python3
"""Current native N-wait integration gate (no physical card or carrier).

Sources workspace-env.sh itself, then runs the Rust multistandby_* suites and
real loopback HTTP/IKE/protected IMS/SMS manager_exchange + facade_exchange
compositions. Their simulated card transport exercises the real Gate/Owner
workers. The facade HTTP harness calls the public handler directly; main router
authentication is covered separately by main/public HTTP tests, not this harness.
Success proves these software compositions, not physical-device or
carrier acceptance. Logs and the compatible multistandby-contract-results.json
live in Switch_MAC/build/reports.

--skip-build remains accepted by functional-check callers: cargo test always
checks source freshness and reuses its cache; it does NOT skip necessary compile
work. No separate daemon or Go fixture is built. The pre-native Go IPC runner was retired in iteration 511 after its active
coverage was mapped to these native suites; historical source remains in the
iteration baseline archive.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time

SOURCE = Path(__file__).resolve().parents[1]
SUITES = (
    ("multistandby_", "Native durable store/operations, card ownership and cleanup, inventory, manager, facade, retry/recovery/maintenance timers"),
    ("manager_exchange", "Real native Manager loopback IKE/IMS/SMS, renewal/recovery/cancel and confirmed physical cleanup"),
    ("facade_exchange", "Real public HTTP handler admission plus native inventory, Manager, IKE/IMS and N-wait SMS"),
)


def workspace_environment():
    env = dict(os.environ, SWITCH_SOURCE_ROOT=str(SOURCE))
    result = subprocess.run(
        ["/bin/sh", "-c", '. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"; env -0'],
        cwd=SOURCE, env=env, check=True, stdout=subprocess.PIPE,
    )
    return dict(item.split("=", 1) for item in os.fsdecode(result.stdout).split("\0") if item)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true",
                        help="compatibility flag: reuse cargo cache, but compile stale sources when necessary")
    args = parser.parse_args()
    env = workspace_environment()
    build = Path(env["SWITCH_BUILD_DIR"]).resolve()
    if not build.is_relative_to(SOURCE.parent.resolve()):
        raise RuntimeError("native integration output must stay inside this Switch workspace")
    reports = build / "reports"
    reports.mkdir(parents=True, exist_ok=True)
    results = []
    report = {
        "fixture": "native Rust suites and real loopback HTTP/IKE/protected IMS/SMS compositions",
        "scope": "Simulated card transport; no physical reader, card or carrier acceptance",
        "historical_oracle": "Pre-native Go IPC runner retired in iteration 511; source preserved in the historical baseline",
        "authentication_scope": "Facade harness calls the public handler directly; main router/authentication requires separate main/public HTTP tests",
        "legacy_fixture_routes_used": False,
        "go_runtime_rpc_used": False,
        "skip_build_requested": args.skip_build,
        "build_policy": "cargo test checks freshness and reuses cache; necessary compilation is never skipped",
        "checks": results,
        "passed": False,
    }
    try:
        if args.skip_build:
            print("--skip-build: reusing cargo cache; stale test sources will still compile.", flush=True)
        for name, scope in SUITES:
            log_path = reports / f"multistandby-native-{name.rstrip('_')}.log"
            command = [env.get("SWITCH_CARGO_BIN") or "cargo", "+1.98.0", "test", "--locked",
                       "-p", "proletariusd", name, "--", "--test-threads=2"]
            print(f"RUN {name}: {scope}", flush=True)
            started = time.monotonic()
            with log_path.open("w") as log:
                result = subprocess.run(command, cwd=SOURCE / "rust", env=env,
                                        stdout=log, stderr=subprocess.STDOUT)
            text = log_path.read_text(errors="replace")
            summaries = re.findall(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;", text)
            passed = sum(int(row[1]) for row in summaries)
            failed = sum(int(row[2]) for row in summaries)
            ignored = sum(int(row[3]) for row in summaries)
            ok = result.returncode == 0 and passed > 0 and failed == 0 and ignored == 0
            results.append({"name": name, "scope": scope, "passed": ok,
                            "tests_passed": passed, "tests_failed": failed, "tests_ignored": ignored,
                            "returncode": result.returncode, "log": str(log_path),
                            "duration_seconds": round(time.monotonic() - started, 3)})
            if not ok:
                raise RuntimeError(f"{name} failed or matched no executed tests; inspect {log_path}")
            print(f"PASS {name}: {passed} tests; {log_path}", flush=True)
        report["passed"] = True
    except Exception as error:
        report["error"] = str(error)
        raise
    finally:
        (reports / "multistandby-contract-results.json").write_text(
            json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(f"Passed {sum(row['tests_passed'] for row in results)} native tests; reports: {reports}", flush=True)


if __name__ == "__main__":
    main()
