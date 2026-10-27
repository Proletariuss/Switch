#!/usr/bin/env python3
"""Regenerate only the reviewed Xcode project on GitHub runners.

The exact source replacement and numeric Xcode tool markers are the only
permitted semantic changes. No Rust, review metadata or production source is
modified, and this generation does not establish software/device acceptance.
"""
import base64
import copy
import hashlib
import json
import os
import re
from pathlib import Path
import stat
import subprocess
import sys

sys.dont_write_bytecode = True
SOURCE_SHA = "87683879ca3d78810baa919a7daa848d67dd5ba9"
SOURCE_TREE = "bbcd5644d9e2993a825d4289b9fb7ffe10526019"
PROJECT = "Switch.xcodeproj/project.pbxproj"
OUTPUT_PATHS = {PROJECT}
LIMITS = {PROJECT: 524288}

def require(condition, message):
    if not condition:
        raise RuntimeError(message)

def run(*args):
    return subprocess.check_output(args, cwd=root)

def git(*args):
    return run("git", "-c", "core.hooksPath=/dev/null", *args)

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
original_project = project_json()

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
require(files, "project generation produced no changes")
require(sum(row["size"] for row in files) <= 524288, "aggregate size exceeded")
bundle = {"schema": 1, "source_sha": SOURCE_SHA, "source_tree": SOURCE_TREE,
          "workflow_sha": os.environ["GITHUB_SHA"], "run_id": os.environ["GITHUB_RUN_ID"],
          "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"], "files": files}
out.mkdir(parents=True, exist_ok=False)
payload = (json.dumps(bundle, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
require(len(payload) <= 1048576, "artifact JSON too large")
(out / "changes.json").write_bytes(payload)
with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
    print("bundle_sha256=" + digest(payload), file=output)
print(json.dumps({"source_sha": SOURCE_SHA, "files": [{k:v for k,v in row.items() if k != "content_base64"} for row in files]}, indent=2))
