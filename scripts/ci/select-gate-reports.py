#!/usr/bin/env python3
"""Select latest actual gate records across attempts, never fall back past a failure."""
import json
import os
from pathlib import Path
import re
import shutil
import urllib.request

workspace = Path(os.environ["GITHUB_WORKSPACE"])
commit = os.environ["EXPECTED_COMMIT"]
run = os.environ["GITHUB_RUN_ID"]
required = {"portable", "rust", "device-sms-send", "module-network-recovery", "api-request-policy", "sms-forwarding-client", "release-initialization"}
pattern = re.compile(r"software-report-(\d+)-(portable|rust|swift-models|swift-handoff)$")
artifact_ids = {}
page = 1
while True:
    url = os.environ["GITHUB_API_URL"] + "/repos/" + os.environ["GITHUB_REPOSITORY"] + "/actions/runs/" + run + "/artifacts?per_page=100&page=" + str(page)
    request = urllib.request.Request(url, headers={"Authorization": "Bearer " + os.environ["GH_TOKEN"], "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(request) as response:
        artifacts = json.load(response)["artifacts"]
    for artifact in artifacts:
        if pattern.fullmatch(artifact["name"]) and not artifact["expired"]:
            artifact_ids[artifact["name"]] = artifact["id"]
    if len(artifacts) < 100:
        break
    page += 1
records = {name: [] for name in required}
for folder in (workspace / "downloaded-reports").iterdir():
    match = pattern.fullmatch(folder.name)
    if not match or not folder.is_dir():
        continue  # Prior candidate uploads contain copied reports, not original gates.
    for path in folder.glob("*.json"):
        report = json.loads(path.read_text())
        gate = report.get("gate")
        if gate not in required:
            raise SystemExit("unexpected gate report in original artifact")
        if report["commit"] != commit or report["run_id"] != run or int(report["run_attempt"]) != int(match[1]):
            raise SystemExit("gate report source identity mismatch")
        records[gate].append((int(report["run_attempt"]), path, report))
output = workspace / "reports"
output.mkdir(exist_ok=False)
sources = {}
for gate, choices in records.items():
    if not choices:
        raise SystemExit("missing gate: " + gate)
    latest = max(choice[0] for choice in choices)
    chosen = [choice for choice in choices if choice[0] == latest]
    if len(chosen) != 1:
        raise SystemExit("ambiguous latest original gate report: " + gate)
    attempt, path, report = chosen[0]
    if report["status"] != "passed" or report["exit_code"] != 0:
        raise SystemExit("latest gate report failed: " + gate)
    artifact = path.parent.name
    if artifact not in artifact_ids:
        raise SystemExit("source artifact metadata missing")
    for suffix in [".json", ".log"]:
        shutil.copyfile(path.with_suffix(suffix), output / (gate + suffix))
    sources[gate] = {"artifact_name": artifact, "artifact_id": artifact_ids[artifact], "run_id": run, "run_attempt": attempt}
(output / "report-sources.json").write_text(json.dumps(sources, indent=2) + "\n")
