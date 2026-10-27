#!/usr/bin/env python3
"""One-shot reviewed live-diagnostics normalization; GitHub runners only.

This operation formats the fixed reviewed source, refreshes only its reviewed
contract fingerprints and regenerates the checked-in Xcode project. It does not
run device recovery, change admission or establish software/product acceptance.
"""
import base64
import copy
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path
import stat
import subprocess
import sys

sys.dont_write_bytecode = True
SOURCE_SHA = "ea172cb9b685377895c16e7c18f05c121718c53a"
SOURCE_TREE = "dcb088fe7d69cb5faf8f7b71d878859c2d9d86a3"
FORMAT_PATHS = [
    "rust/crates/proletariusd/src/main.rs",
    "rust/crates/proletariusd/src/support_diagnostics.rs",
    "rust/crates/proletariusd/src/support_diagnostics/live.rs"
]
SCOPE_PATHS = set([
    "rust/crates/proletariusd/src/main.rs",
    "rust/crates/proletariusd/src/support_diagnostics.rs",
    "rust/crates/proletariusd/src/support_diagnostics/live.rs",
    "app/Sources/SupportBundleBuilder.swift",
    "app/Sources/BackendProcess.swift",
    "app/Sources/ClientDiagnostics.swift",
    "app/Sources/LiveDiagnostics.swift",
    "app/Sources/Views/DiagnosticsView.swift",
    "app/Sources/Views/SupportDiagnosticsView.swift"
])
NEW_WHOLE_SCOPES = [
    "rust/crates/proletariusd/src/support_diagnostics/live.rs",
    "app/Sources/LiveDiagnostics.swift",
    "app/Sources/Views/SupportDiagnosticsView.swift"
]
REMOVED_SCOPE_FILE = "app/Sources/Views/DiagnosticEventsView.swift"
PROJECT = "Switch.xcodeproj/project.pbxproj"
REVIEW = "docs/optimization/all-rust/api-contract-review.json"
INVENTORY = "docs/optimization/all-rust/api-contract-inventory.json"
OUTPUT_PATHS = set(FORMAT_PATHS) | {REVIEW, INVENTORY, PROJECT}
LIMITS = {path: 524288 for path in FORMAT_PATHS}
LIMITS.update({REVIEW: 2097152, INVENTORY: 4194304, PROJECT: 524288})
NOTE = (
    "Live-Diagnose 3.56: unabhaengig gepruefte Quellbereiche bei " + SOURCE_SHA + ". "
    "Die aktuelle Speicheransicht vereinigt begrenzte sichere Ereignisse, ohne Datei- "
    "oder Geraetezugriffe und ohne Aenderung der Modulzulassung. "
    "Der optionale geschlossene Status legacy_current bleibt ohne Rohdaten in "
    "Export und Live-Ansicht erhalten; fehlende Altversion-Felder bedeuten not_observed. "
    "Die Startbeobachtung bleibt auf 210 Sekunden begrenzt, behaelt den lebenden "
    "Prozesseigentuemer bei und ersetzt keine physische Abschlussbestaetigung. "
    "Diese Normalisierung bestaetigt weder historische Wiederherstellung noch "
    "Software- oder Geraeteabnahme; die vollstaendigen finalen Cloud-Gates stehen aus."
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


def project_json():
    return json.loads(run("plutil", "-convert", "json", "-o", "-", str(root / PROJECT)))

def source_reference(project, path):
    objects = project["objects"]
    main = objects[project["rootObject"]]["mainGroup"]
    parents = {}
    for identity, obj in objects.items():
        if obj.get("isa") == "PBXGroup":
            for child in obj.get("children", []):
                parents.setdefault(child, []).append(identity)
    found = []
    for identity, obj in objects.items():
        if obj.get("isa") != "PBXFileReference" or obj.get("path") != path.rsplit("/", 1)[-1]:
            continue
        parts, current, visited = [], identity, set()
        while current != main:
            require(current not in visited, "Xcode group cycle")
            visited.add(current)
            node = objects[current]
            require(node.get("sourceTree") == "<group>", "unexpected source reference root")
            if node.get("path"):
                parts.append(node["path"])
            require(len(parents.get(current, [])) == 1, "ambiguous source reference parent")
            current = parents[current][0]
        require("/".join(reversed(parts)) == path, "unexpected source reference path")
        require(obj == {"isa": "PBXFileReference", "lastKnownFileType": "sourcecode.swift",
                        "path": path.rsplit("/", 1)[-1], "sourceTree": "<group>"},
                "unexpected source reference attributes")
        found.append(identity)
    require(len(found) <= 1, "duplicate source reference")
    return found

def strip_source(project, path):
    refs = source_reference(project, path)
    objects = project["objects"]
    builds = [identity for identity, obj in objects.items()
              if obj.get("isa") == "PBXBuildFile" and obj.get("fileRef") in refs]
    require(len(builds) == len(refs), "missing or duplicated source build entry")
    for identity in builds:
        require(objects[identity] == {"isa": "PBXBuildFile", "fileRef": refs[0]},
                "source build flags changed")
        owners = [obj["isa"] for obj in objects.values() if identity in obj.get("files", [])]
        require(owners == ["PBXSourcesBuildPhase"], "source is not compiled exactly once")
    remove = set(refs + builds)
    for obj in objects.values():
        for key in ["children", "files"]:
            if key in obj:
                obj[key] = [value for value in obj[key] if value not in remove]
    for identity in remove:
        del objects[identity]
    return len(refs)

def verify_project(before, after):
    old, new = copy.deepcopy(before), copy.deepcopy(after)
    removed = "app/Sources/Views/DiagnosticEventsView.swift"
    added = "app/Sources/LiveDiagnostics.swift"
    require(not (root / removed).exists() and (root / added).is_file(),
            "frozen source addition/removal differs")
    require(strip_source(old, removed) == 1 and strip_source(new, removed) == 0,
            "obsolete diagnostic view was not removed exactly once")
    require(strip_source(old, added) == 0 and strip_source(new, added) == 1,
            "new live diagnostics source was not added exactly once")
    tool_metadata = []
    for project in [old, new]:
        attributes = project["objects"][project["rootObject"]].get("attributes", {})
        values = {}
        for key in ["LastUpgradeCheck", "LastSwiftUpdateCheck"]:
            if key in attributes:
                value = attributes.pop(key)
                require(re.fullmatch(r"[0-9]{3,6}", str(value)), "invalid Xcode tool marker")
                values[key] = value
        tool_metadata.append(values)
    # Includes all target settings, bundle IDs, signing, entitlements, build
    # phases/scripts, resources, dependencies and every unaffected file object.
    require(old == new, "Xcode generation changed unapproved project semantics")
    print(json.dumps({"xcode_tool_metadata_before": tool_metadata[0],
                      "xcode_tool_metadata_after": tool_metadata[1],
                      "removed_source": removed, "added_source": added}, indent=2))

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
original_project = project_json()

# Existing checker is loaded only in this contents:read job from the exact source.
spec = importlib.util.spec_from_file_location("migration_contracts", root / "scripts/check-rust-migration-contracts.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
run("cargo", "+1.98.0", "fmt", "--all", "--manifest-path", "rust/Cargo.toml")
run("cargo", "+1.98.0", "fmt", "--all", "--manifest-path",
    "scripts/acceptance/daemon-capsule-fixture/Cargo.toml")
require(changed() <= set(FORMAT_PATHS), "rustfmt touched an unapproved path")
require(not git("ls-files", "--others", "--exclude-standard").strip(), "unexpected untracked source")

# Only the deleted standalone page may lose an old scope, if one existed.
require(not (root / REMOVED_SCOPE_FILE).exists(), "retired page unexpectedly present")
retained_scopes = [scope for scope in original_review["scopes"] if scope["file"] != REMOVED_SCOPE_FILE]
removed_scopes = [scope for scope in original_review["scopes"] if scope["file"] == REMOVED_SCOPE_FILE]
review["scopes"] = copy.deepcopy(retained_scopes)
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
require(len(review["scopes"]) >= len(retained_scopes), "existing scope removed")
for old, new in zip(retained_scopes, review["scopes"]):
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
# XcodeGen only reads the frozen specification. The semantic guard rejects
# every project change except the exact reviewed source replacement and tool marks.
print(run("xcodegen", "--version").decode())
run("xcodegen", "generate", "--spec", "project.yml")
verify_project(original_project, project_json())
print(git("diff", "--", PROJECT).decode())
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
                  "removed_scopes": removed_scopes,
                  "files": [{k:v for k,v in row.items() if k != "content_base64"} for row in files]},
                 ensure_ascii=False, indent=2))
