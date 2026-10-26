#!/usr/bin/env python3
"""Run a real gate, retain its log/status/timing, and propagate its exit code."""
import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import time

name, *command = sys.argv[1:]
if not command or not name.replace("-", "").isalnum():
    raise SystemExit("usage: run-report.py NAME COMMAND [ARGS...]")
root = Path(os.environ["GITHUB_WORKSPACE"]) / "reports"
root.mkdir(exist_ok=True)
started = datetime.datetime.now(datetime.timezone.utc).isoformat()
start = time.monotonic()
status = 125
try:
    with (root / (name + ".log")).open("w") as log:
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in process.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            log.write(line)
            log.flush()
        status = process.wait()
finally:
    (root / (name + ".json")).write_text(json.dumps({
        "gate": name, "command": command, "exit_code": status,
        "status": "passed" if status == 0 else "failed",
        "started_at": started, "duration_seconds": round(time.monotonic() - start, 3),
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "run_id": os.environ["GITHUB_RUN_ID"], "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"],
    }, indent=2) + "\n")
raise SystemExit(status if status >= 0 else 128 - status)
