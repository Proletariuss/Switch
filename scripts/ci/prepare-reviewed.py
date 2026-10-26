#!/usr/bin/env python3
"""Apply only root-authorized source review updates on a GitHub-hosted runner."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
AUTH = "docs/ci/reviewed-prepare-authorization.json"
REVIEW = "docs/optimization/all-rust/api-contract-review.json"
INVENTORY = "docs/optimization/all-rust/api-contract-inventory.json"
BRANCH = "automation/cloud-four-batches"
os.chdir(ROOT)
def require(condition, message):
    if not condition:
        raise SystemExit(message)
def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()
def content(revision, path):
    return subprocess.check_output(["git", "show", revision + ":" + path])
def key(row):
    return row["file"], row.get("function")
expected = os.environ["EXPECTED_COMMIT"]
require(os.environ.get("GITHUB_ACTIONS") == "true", "GitHub Actions runner required")
require(os.environ["GITHUB_REF"] == "refs/heads/" + BRANCH, "preparation branch is not allowed")
require(re.fullmatch(r"[0-9a-f]{40}", expected) and git("rev-parse", "HEAD") == expected, "expected HEAD mismatch")
require(not git("status", "--porcelain"), "checkout is dirty")
auth = json.loads((ROOT / AUTH).read_text())
require(auth["schema_version"] == 1 and auth["functional_acceptance_awarded"] is False, "invalid authorization scope")
previous = auth["reviewed_source_commit"]
baseline = auth["baseline_commit"]
require(all(re.fullmatch(r"[0-9a-f]{40}", value) for value in (previous, baseline)), "invalid source commit")
require(git("rev-list", "--parents", "-n", "1", expected).split() == [expected, previous], "authorization must directly follow reviewed source")
require(git("diff", "--name-only", previous, expected).splitlines() == [AUTH], "authorization commit changed other source")
subprocess.run(["git", "merge-base", "--is-ancestor", baseline, previous], check=True)
for path in [REVIEW, INVENTORY]:
    require((ROOT / path).read_bytes() == content(baseline, path), "review metadata changed before authorized preparation")
allowed = {}
for row in auth["reviewed_files"]:
    path = row["path"]
    require(re.fullmatch(r"(?:rust/.+\.rs|app/.+\.swift)", path) and ".." not in Path(path).parts, "unsupported reviewed file")
    require(path not in allowed and re.fullmatch(r"[0-9a-f]{40}", row["blob_sha"]), "duplicate or invalid reviewed file")
    require(git("rev-parse", previous + ":" + path) == row["blob_sha"], "reviewed blob differs from source")
    allowed[path] = row["blob_sha"]
changed_sources = {p for p in git("diff", "--name-only", baseline, previous).splitlines() if p.endswith((".rs", ".swift"))}
require(set(allowed) == changed_sources, "reviewed file allowlist does not match changed Rust/Swift source")
spec = importlib.util.spec_from_file_location("contracts", ROOT / "scripts/check-rust-migration-contracts.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
before_tokens = {path: checker.normalized((ROOT / path).read_text()) for path in allowed if path.endswith(".rs")}
for manifest in ["rust/Cargo.toml", "scripts/acceptance/daemon-capsule-fixture/Cargo.toml"]:
    subprocess.run(["cargo", "+1.98.0", "fmt", "--manifest-path", manifest, "--all"], check=True)
formatted = set(git("diff", "--name-only").splitlines())
require(formatted <= set(before_tokens), "formatting changed a file outside the reviewed Rust allowlist")
for path, tokens in before_tokens.items():
    require(checker.normalized((ROOT / path).read_text()) == tokens, "formatting changed reviewed tokens")
review = json.loads((ROOT / REVIEW).read_text())
scopes = {key(row): row for row in review["scopes"]}
require(len(scopes) == len(review["scopes"]), "duplicate source review scope")
permitted = {}
for row in auth["scopes"]:
    ident = key(row)
    require(ident not in permitted and ident in scopes and row["file"] in allowed, "unauthorized or duplicate review scope")
    require(row["old_sha256"] == scopes[ident]["sha256"], "old reviewed fingerprint mismatch")
    require(isinstance(row["review_note"], str) and len(row["review_note"].strip()) >= 30, "missing concrete manual review note")
    permitted[ident] = row
updates = []
for ident, row in scopes.items():
    text = checker.production((ROOT / row["file"]).read_text(), "rust" if row["file"].endswith(".rs") else "swift")
    scope = checker.function_scope(text, row["function"])[0] if row.get("function") else text
    actual = hashlib.sha256(scope.encode()).hexdigest() if row.get("fingerprint") == "sha256-raw" else checker.digest(scope)
    if actual == row["sha256"]:
        continue
    require(ident in permitted, "unreviewed fingerprint drift: " + repr(ident))
    authorization = permitted[ident]
    updates.append({"file": row["file"], "function": row.get("function"), "old_sha256": row["sha256"], "new_sha256": actual})
    row["sha256"] = actual
    row["review"] = row.get("review", "") + "\n\nCloud reviewed preparation at " + expected + ": " + authorization["review_note"] + " No functional, device or product acceptance is awarded."
require({(row["file"], row["function"]) for row in updates} == set(permitted), "authorization contains an unchanged scope; review the exact delta")
(ROOT / REVIEW).write_text(json.dumps(review, indent=2, ensure_ascii=False) + "\n")
subprocess.run(["python3", "scripts/check-rust-migration-contracts.py", "--write-inventory", "--require-rust-only"], check=True)
subprocess.run(["scripts/preflight-source.sh"], check=True)
for manifest in ["rust/Cargo.toml", "scripts/acceptance/daemon-capsule-fixture/Cargo.toml"]:
    subprocess.run(["cargo", "+1.98.0", "fmt", "--manifest-path", manifest, "--all", "--", "--check"], check=True)
changed = set(git("diff", "--name-only").splitlines())
require(changed <= set(before_tokens) | {REVIEW, INVENTORY}, "preparation changed unapproved files")
require(not git("ls-files", "--others", "--exclude-standard"), "preparation created unapproved source files")
subprocess.run(["git", "diff", "--check"], check=True)
require(changed, "preparation has no changes")
subprocess.run(["git", "add", "--", *sorted(changed)], check=True)
subprocess.run(["git", "-c", "user.name=github-actions[bot]", "-c", "user.email=41898282+github-actions[bot]@users.noreply.github.com", "commit", "-m", "ci: prepare explicitly reviewed source scopes and Rust formatting", "-m", "Authorized source: " + previous + "\nAuthorization commit: " + expected], check=True)
prepared = git("rev-parse", "HEAD")
require(git("rev-list", "--parents", "-n", "1", prepared).split() == [prepared, expected], "prepared commit is not a direct child")
report = {"expected_commit": expected, "prepared_commit": prepared, "reviewed_source_commit": previous, "authorization_blob": git("rev-parse", expected + ":" + AUTH), "changed_files": sorted(changed), "updated_scopes": updates, "functional_acceptance_awarded": False}
evidence = Path(os.environ["GITHUB_WORKSPACE"]) / "prepared-evidence"
evidence.mkdir(exist_ok=True)
(evidence / "reviewed-preparation.json").write_text(json.dumps(report, indent=2) + "\n")
with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
    stream.write("prepared_sha=" + prepared + "\n")
print(json.dumps(report, indent=2))
