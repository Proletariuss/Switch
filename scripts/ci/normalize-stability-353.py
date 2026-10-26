#!/usr/bin/env python3
"""One-shot GitHub-only normalization of an explicit, reviewed source snapshot."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
ROOT = Path(__file__).resolve().parents[2]
AUTH = "docs/ci/stability-normalization-353.json"
REVIEW = "docs/optimization/all-rust/api-contract-review.json"
INVENTORY = "docs/optimization/all-rust/api-contract-inventory.json"
BRANCH = "automation/stability-cloud-353"
SELF = "scripts/ci/normalize-stability-353.py"
WORKFLOW = ".github/workflows/stability-normalization-353.yml"
os.chdir(ROOT)
def require(condition, message):
    if not condition:
        raise SystemExit(message)
def git(*arguments):
    return subprocess.check_output(["git", *arguments], text=True).strip()
def run(*arguments):
    subprocess.run(arguments, check=True)
def api(path, value=None):
    command = ["gh", "api", "repos/Proletariuss/Switch/" + path]
    if value is not None:
        command += ["-X", "PATCH" if path.startswith("git/refs/heads/") else "POST", "--input", "-"]
    result = subprocess.run(command, input=None if value is None else json.dumps(value),
                            text=True, stdout=subprocess.PIPE, check=True)
    return json.loads(result.stdout)
expected = os.environ["EXPECTED_COMMIT"]
require(os.environ.get("GITHUB_ACTIONS") == "true", "GitHub runner required")
require(os.environ["GITHUB_REPOSITORY"] == "Proletariuss/Switch", "repository mismatch")
require(os.environ["GITHUB_REF"] == "refs/heads/" + BRANCH, "normalization branch mismatch")
require(git("rev-parse", "HEAD") == expected == os.environ["GITHUB_SHA"], "source SHA mismatch")
require(not git("status", "--porcelain"), "checkout must start clean")
auth = json.loads(Path(AUTH).read_text())
require(auth["schema_version"] == 1 and auth["functional_acceptance_awarded"] is False,
        "authorization scope mismatch")
require(git("rev-parse", "HEAD^") == auth["source_commit"], "preparation must directly follow its source snapshot")
require(git("rev-parse", "HEAD:" + REVIEW) == auth["review_blob"], "review baseline changed")
for path, blob in auth["source_blobs"].items():
    require(git("rev-parse", "HEAD:" + path) == blob, "approved source blob changed: " + path)
require(api("git/ref/heads/" + BRANCH)["object"]["sha"] == expected, "remote branch advanced")
spec = importlib.util.spec_from_file_location("contracts", ROOT / "scripts/check-rust-migration-contracts.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
rust_files = {path for path in auth["source_blobs"] if path.endswith(".rs")}
before_tokens = {path: [token for token, _, _ in checker.lex(Path(path).read_text())] for path in rust_files}
run("cargo", "+1.98.0", "fmt", "--manifest-path", "rust/Cargo.toml", "--all")
run("cargo", "+1.98.0", "fmt", "--manifest-path",
    "scripts/acceptance/daemon-capsule-fixture/Cargo.toml", "--all")
formatted = set(git("diff", "--name-only").splitlines())
require(formatted <= rust_files, "rustfmt changed a file outside the authorized Rust source set")
token_changes = [path for path in sorted(formatted)
                 if before_tokens[path] != [token for token, _, _ in checker.lex(Path(path).read_text())]]
format_patch = subprocess.check_output(["git", "diff", "--", *sorted(formatted)], text=True) if formatted else ""
review = json.loads(Path(REVIEW).read_text())
rust_texts = {path.relative_to(ROOT).as_posix(): path.read_text()
              for path in (ROOT / "rust/crates/proletariusd/src").rglob("*.rs")}
excluded = checker.rust_test_sources(ROOT, rust_texts)
approved = {path for path in auth["source_blobs"]
            if path not in excluded and (path.startswith("app/Sources/") or path.startswith("rust/"))}
changed_scopes = []
def fingerprint(row):
    path = row["file"]
    text = checker.production(Path(path).read_text(), "rust" if path.endswith(".rs") else "swift")
    scope = checker.function_scope(text, row["function"])[0] if row.get("function") else text
    return hashlib.sha256(scope.encode()).hexdigest() if row.get("fingerprint") == "sha256-raw" else checker.digest(scope)
for row in review["scopes"]:
    actual = fingerprint(row)
    if actual == row["sha256"]:
        continue
    require(row["file"] in approved, "unapproved review fingerprint drift: " + row["file"])
    changed_scopes.append({"file":row["file"],"function":row.get("function"),
                           "old_sha256":row["sha256"],"new_sha256":actual})
    row["sha256"] = actual
    row["review"] = row.get("review", "") + "\n\n" + auth["review_note"] + " Source snapshot: " + auth["source_commit"] + "."
for path in sorted(approved):
    if any(row["file"] == path and not row.get("function") for row in review["scopes"]):
        continue
    row = {"file":path, "review":auth["review_note"] + " Whole production helper scope at " + auth["source_commit"] + "."}
    row["sha256"] = fingerprint(row)
    review["scopes"].append(row)
    changed_scopes.append({"file":path,"function":None,"old_sha256":None,"new_sha256":row["sha256"]})
Path(REVIEW).write_text(json.dumps(review, ensure_ascii=False, indent=2) + "\n")
run("python3", "scripts/check-rust-migration-contracts.py", "--write-inventory", "--require-rust-only")
run("xcodegen", "generate", "--spec", "project.yml")
run("scripts/preflight-source.sh")
run("cargo", "+1.98.0", "fmt", "--manifest-path", "rust/Cargo.toml", "--all", "--", "--check")
run("cargo", "+1.98.0", "fmt", "--manifest-path",
    "scripts/acceptance/daemon-capsule-fixture/Cargo.toml", "--all", "--", "--check")
run("git", "diff", "--check")
require(not git("ls-files", "--others", "--exclude-standard"), "unexpected generated untracked source")
changed = set(git("diff", "--name-only").splitlines())
derived = {REVIEW, INVENTORY, "Switch.xcodeproj/project.pbxproj",
           "Switch.xcodeproj/project.xcworkspace/contents.xcworkspacedata",
           "Switch.xcodeproj/xcshareddata/xcschemes/Switch.xcscheme"}
require(changed <= rust_files | derived, "normalization changed an unapproved file")
require(api("git/ref/heads/" + BRANCH)["object"]["sha"] == expected, "remote source advanced before publication")
tree_entries = []
for path in sorted(changed):
    mode = git("ls-tree", "HEAD", "--", path).split()[0]
    require(mode in ("100644", "100755") and Path(path).is_file() and not Path(path).is_symlink(),
            "unexpected source entry type")
    tree_entries.append({"path":path,"mode":mode,"type":"blob","content":Path(path).read_text()})
# Retire this write-capable entry point in the commit it produces.
for path in (SELF, WORKFLOW):
    tree_entries.append({"path":path,"mode":"100755" if path == SELF else "100644","type":"blob","sha":None})
tree = api("git/trees", {"base_tree":git("rev-parse", "HEAD^{tree}"),"tree":tree_entries})["sha"]
commit = api("git/commits", {
    "message":"chore(ci): normalize reviewed stability source and derived contracts",
    "tree":tree,"parents":[expected]})["sha"]
api("git/refs/heads/" + BRANCH, {"sha":commit,"force":False})
report = {"source_commit":auth["source_commit"],"normalization_input":expected,"prepared_commit":commit,
          "changed_files":sorted(changed),"retired_files":[SELF,WORKFLOW],"fingerprint_updates":changed_scopes,
          "rustfmt_token_changes":token_changes,"functional_acceptance_awarded":False}
evidence = Path(os.environ["GITHUB_WORKSPACE"]) / "normalization-evidence"
evidence.mkdir(exist_ok=True)
(evidence / "normalization.json").write_text(json.dumps(report, indent=2) + "\n")
(evidence / "rustfmt.patch").write_text(format_patch)
(evidence / "source.patch").write_text(subprocess.check_output(["git", "diff"], text=True))
with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
    stream.write("prepared_sha=" + commit + "\n")
with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
    stream.write("Prepared exact source: " + commit + "\n\nRun ordinary quality from this branch/SHA. No tests/build/release claimed by normalization.\n")
print(json.dumps(report, indent=2))
