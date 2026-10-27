#!/usr/bin/env python3
"""Reviewed DJI-356 current-baseline/live-diagnostics normalization; GitHub only.

Only declared Rust files may be formatted. Reviewed Swift scopes may receive
contract fingerprint metadata only; Swift, Xcode and project sources are never
outputs. The current-baseline path performs fixed same-device read-only checks,
retains physical debts, and does not assert historical restoration. Download
lineage covers only controlled local-only data before Enable/physical intent.
No device, user-data or product acceptance is performed by this preparation.
This temporary control documentation is excluded from the source output list.
"""
import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

sys.dont_write_bytecode = True
SOURCE_SHA = "388b830af1a61581056e1b270a7057caeb4c4b97"
SOURCE_TREE = "f346f75fcf02f7fca602a6f2c53758dd3d5c1b1f"
FORMAT_PATHS = [
    "rust/crates/proletariusd/src/call_mode_adb.rs",
    "rust/crates/proletariusd/src/call_mode_adb/legacy_current.rs",
    "rust/crates/proletariusd/src/call_mode_adb/legacy_current/tests.rs",
    "rust/crates/proletariusd/src/call_mode_adb/startup_recovery.rs",
    "rust/crates/proletariusd/src/call_mode_backups.rs",
    "rust/crates/proletariusd/src/main.rs",
    "rust/crates/proletariusd/src/module_radio.rs",
    "rust/crates/proletariusd/src/module_radio/native_startup.rs",
    "rust/crates/proletariusd/src/module_recovery.rs",
    "rust/crates/proletariusd/src/module_recovery/legacy_locks.rs",
    "rust/crates/proletariusd/src/module_recovery/reconciliation.rs",
    "rust/crates/proletariusd/src/module_recovery/runtime_download.rs",
    "rust/crates/proletariusd/src/module_recovery/runtime_download/tests.rs",
    "rust/crates/proletariusd/src/module_startup.rs",
    "rust/crates/proletariusd/src/module_startup/tests.rs",
    "rust/crates/proletariusd/src/startup_recovery.rs",
    "rust/crates/proletariusd/src/startup_recovery/evidence.rs",
    "rust/crates/proletariusd/src/startup_recovery/legacy_current.rs",
    "rust/crates/proletariusd/src/startup_recovery/legacy_current/tests.rs",
    "rust/crates/proletariusd/src/startup_recovery/reconcile/backup_lifecycle.rs",
    "rust/crates/proletariusd/src/support_diagnostics.rs",
    "rust/crates/proletariusd/src/support_diagnostics/live.rs",
    "rust/crates/proletariusd/src/support_diagnostics/recovery.rs",
    "rust/crates/proletariusd/src/usb_at.rs",
    "rust/crates/proletariusd/src/usb_at_transport.rs",
    "rust/crates/proletariusd/src/usb_at_transport/legacy_current.rs",
    "rust/crates/proletariusd/src/usb_at_transport/legacy_current/tests.rs"
]
SCOPE_PATHS = set([
    "rust/crates/proletariusd/src/call_mode_adb.rs",
    "rust/crates/proletariusd/src/call_mode_adb/legacy_current.rs",
    "rust/crates/proletariusd/src/call_mode_adb/startup_recovery.rs",
    "rust/crates/proletariusd/src/call_mode_backups.rs",
    "rust/crates/proletariusd/src/main.rs",
    "rust/crates/proletariusd/src/module_radio.rs",
    "rust/crates/proletariusd/src/module_radio/native_startup.rs",
    "rust/crates/proletariusd/src/module_recovery.rs",
    "rust/crates/proletariusd/src/module_recovery/legacy_locks.rs",
    "rust/crates/proletariusd/src/module_recovery/reconciliation.rs",
    "rust/crates/proletariusd/src/module_recovery/runtime_download.rs",
    "rust/crates/proletariusd/src/module_startup.rs",
    "rust/crates/proletariusd/src/startup_recovery.rs",
    "rust/crates/proletariusd/src/startup_recovery/evidence.rs",
    "rust/crates/proletariusd/src/startup_recovery/legacy_current.rs",
    "rust/crates/proletariusd/src/startup_recovery/reconcile/backup_lifecycle.rs",
    "rust/crates/proletariusd/src/support_diagnostics.rs",
    "rust/crates/proletariusd/src/support_diagnostics/live.rs",
    "rust/crates/proletariusd/src/support_diagnostics/recovery.rs",
    "rust/crates/proletariusd/src/usb_at.rs",
    "rust/crates/proletariusd/src/usb_at_transport.rs",
    "rust/crates/proletariusd/src/usb_at_transport/legacy_current.rs",
    "app/Sources/SupportBundleBuilder.swift",
    "app/Sources/BackendProcess.swift",
    "app/Sources/ClientDiagnostics.swift",
    "app/Sources/LiveDiagnostics.swift",
    "app/Sources/Views/DiagnosticsView.swift",
    "app/Sources/Views/SupportDiagnosticsView.swift"
])
NEW_WHOLE_SCOPES = [
    "rust/crates/proletariusd/src/call_mode_adb/legacy_current.rs",
    "rust/crates/proletariusd/src/startup_recovery/legacy_current.rs",
    "rust/crates/proletariusd/src/usb_at_transport/legacy_current.rs",
    "rust/crates/proletariusd/src/support_diagnostics/live.rs",
    "app/Sources/LiveDiagnostics.swift",
    "app/Sources/Views/SupportDiagnosticsView.swift"
]
REVIEW = "docs/optimization/all-rust/api-contract-review.json"
INVENTORY = "docs/optimization/all-rust/api-contract-inventory.json"
OUTPUT_PATHS = set(FORMAT_PATHS) | {REVIEW, INVENTORY}
LIMITS = {path: 524288 for path in FORMAT_PATHS}
LIMITS.update({REVIEW: 2097152, INVENTORY: 4194304})
NOTE = (
    "DJI-356 current baseline and live diagnostics: reviewed source " + SOURCE_SHA + ". "
    "Same-device finite read-only inspection preserves current CFUN, checked release, "
    "cancellation, original evidence and same-Gate first-open binding; it never asserts "
    "historical restoration or exempts typed physical obligations. Bounded memory-only "
    "live diagnostics and owner-preserving startup observation retain closed schemas. "
    "Download provenance is local-only before Enable or physical intent. "
    "Only approved fingerprints are refreshed; full final-source cloud gates and "
    "actual device acceptance remain outstanding."
)

def require(condition, message):
    if not condition:
        raise RuntimeError(message)

def run(*args):
    return subprocess.check_output(args, cwd=root)

def git(*args):
    return run("git", "-c", "core.hooksPath=/dev/null", *args)

def read_json(path):
    return json.loads((root / path).read_text(encoding="utf-8"))

def digest(data):
    return hashlib.sha256(data).hexdigest()

def safe_bytes(path):
    target = root / path
    require(target.resolve().is_relative_to(root), "path escaped source")
    require(stat.S_ISREG(target.lstat().st_mode), "nonregular output")
    data = target.read_bytes()
    require(0 < len(data) <= LIMITS[path], "output size outside reviewed bound")
    data.decode("utf-8")
    return data

def changed():
    return set(filter(None, git("diff", "--name-only", "-z", "HEAD").decode().split("\0")))

def signature(inventory):
    keys = ("id", "method", "path", "owner", "category", "audience",
            "migration_status", "go_dependencies", "alias_of")
    routes = [{key: row.get(key) for key in keys} for row in inventory["routes"]]
    policy = [{key: value for key, value in row.items() if key != "source"}
              for row in inventory["public_method_policy"]]
    return {"routes": routes, "fallback": inventory["fallback"], "policy": policy}

require(os.environ.get("GITHUB_ACTIONS") == "true", "GitHub-hosted workflow only")
root = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
require(out != root and not out.is_relative_to(root), "artifact must be outside source")
require(git("rev-parse", "HEAD").decode().strip() == SOURCE_SHA, "wrong source SHA")
require(git("rev-parse", "HEAD^{tree}").decode().strip() == SOURCE_TREE, "wrong source tree")
require(not git("status", "--porcelain", "--untracked-files=all").strip(), "source is not clean")
before = {path: safe_bytes(path) for path in OUTPUT_PATHS}
review = read_json(REVIEW)
original_review = copy.deepcopy(review)
original_inventory = read_json(INVENTORY)
before_routes = signature(original_inventory)
require(len(original_inventory["routes"]) == 314, "reviewed route census changed")
require(len(original_inventory["public_method_policy"]) == 155, "reviewed method census changed")

# Existing checker is loaded only in this contents:read job from the exact source.
spec = importlib.util.spec_from_file_location("migration_contracts", root / "scripts/check-rust-migration-contracts.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
run("cargo", "+1.98.0", "fmt", "--all", "--manifest-path", "rust/Cargo.toml")
run("cargo", "+1.98.0", "fmt", "--all", "--manifest-path",
    "scripts/acceptance/daemon-capsule-fixture/Cargo.toml")
require(changed() <= set(FORMAT_PATHS), "rustfmt touched an unapproved path")
require(not git("ls-files", "--others", "--exclude-standard").strip(), "unexpected untracked source")

seen = set()
updated = []
for scope in review["scopes"]:
    key = (scope["file"], scope.get("function"))
    require(key not in seen, "duplicate old scope")
    seen.add(key)
    text = checker.production(checker.read(root, scope["file"]),
                              "rust" if scope["file"].endswith(".rs") else "swift")
    body = checker.function_scope(text, scope["function"])[0] if scope.get("function") else text
    actual = digest(body.encode()) if scope.get("fingerprint") == "sha256-raw" else checker.digest(body)
    if actual != scope["sha256"]:
        require(scope["file"] in SCOPE_PATHS, "unapproved changed source scope: " + scope["file"])
        scope["sha256"] = actual
        scope["review"] = scope.get("review", "") + "\n\n" + NOTE
        updated.append({"file": scope["file"], "function": scope.get("function")})
for path in NEW_WHOLE_SCOPES:
    if (path, None) not in seen:
        text = checker.production(checker.read(root, path), "rust" if path.endswith(".rs") else "swift")
        review["scopes"].append({"file": path, "sha256": checker.digest(text), "review": NOTE})
        seen.add((path, None))

# Preserve every existing record, order, function and policy annotation.
require({k: v for k, v in review.items() if k != "scopes"} ==
        {k: v for k, v in original_review.items() if k != "scopes"},
        "review policy changed")
for old, new in zip(original_review["scopes"], review["scopes"]):
    expected = copy.deepcopy(old)
    if old["sha256"] != new["sha256"]:
        require(old["file"] in SCOPE_PATHS, "scope update escaped allowlist")
        expected["sha256"] = new["sha256"]
        expected["review"] = old.get("review", "") + "\n\n" + NOTE
    require(new == expected, "existing scope annotation changed")
(root / REVIEW).write_text(checker.encode(review), encoding="utf-8")
run(sys.executable, "scripts/check-rust-migration-contracts.py", "--root", str(root),
    "--write-inventory", "--require-rust-only")
require(signature(read_json(INVENTORY)) == before_routes, "route, owner or method policy changed")
run(sys.executable, "scripts/check-rust-migration-contracts.py", "--root", str(root),
    "--require-rust-only")
require(changed() <= OUTPUT_PATHS, "generated changes escaped allowlist")
require(not git("ls-files", "--others", "--exclude-standard").strip(), "unexpected untracked source")

files = []
for path in sorted(changed()):
    data = safe_bytes(path)
    old = before[path]
    record = git("ls-tree", "HEAD", "--", path).decode().strip().split("\t")
    require(len(record) == 2 and record[1] == path, "source blob missing")
    mode, kind, blob_sha = record[0].split()
    require(mode == "100644" and kind == "blob", "unexpected source mode")
    files.append({"path": path, "mode": mode, "base_blob_sha": blob_sha,
                  "before_sha256": digest(old), "size": len(data), "sha256": digest(data),
                  "content_base64": base64.b64encode(data).decode("ascii")})
require(files, "normalization produced no changes")
require(sum(row["size"] for row in files) <= 8388608, "aggregate size exceeded")
bundle = {"schema": 1, "source_sha": SOURCE_SHA, "source_tree": SOURCE_TREE,
          "workflow_sha": os.environ["GITHUB_SHA"], "run_id": os.environ["GITHUB_RUN_ID"],
          "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"], "files": files}
out.mkdir(parents=True, exist_ok=False)
payload = (json.dumps(bundle, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
require(len(payload) <= 16777216, "artifact JSON too large")
(out / "changes.json").write_bytes(payload)
with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
    print("bundle_sha256=" + digest(payload), file=output)
print(json.dumps({"source_sha": SOURCE_SHA, "updated_scopes": updated,
                  "new_whole_scopes": NEW_WHOLE_SCOPES,
                  "files": [{k:v for k,v in row.items() if k != "content_base64"} for row in files]},
                 ensure_ascii=False, indent=2))
