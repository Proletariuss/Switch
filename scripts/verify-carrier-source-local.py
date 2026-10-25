#!/usr/bin/env python3
"""Verify installed MIT carrier source bytes against the checked-in lock.

This is a local build gate. The separate audit-carrier-freshness.py command
compares an explicit upstream Git commit and is not part of this preflight.
"""

import importlib.util
import json
from pathlib import Path
import re
import stat


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "carrier_freshness", Path(__file__).with_name("audit-carrier-freshness.py"))
FRESHNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FRESHNESS)

COMMIT = re.compile(r"[0-9a-f]{40}\Z")
HASH = re.compile(r"[0-9a-f]{64}\Z")
PROFILE = re.compile(r"profiles/[A-Za-z0-9_-][A-Za-z0-9_.-]*\.yaml\Z")
SELECTOR = re.compile(r"selectors/[0-9]{5,6}\.yaml\Z")
METADATA = {"LICENSE", "README.md", "index.yaml", "inventory.yaml"}
LICENSE_FIELDS = {"key", "name", "spdx_id", "url", "node_id"}
CARRIER_ROOT = {"README.md", "SOURCES.md", "ios-carrier-profiles-MIT.txt",
                "profiles", "selectors", "compatibility", "user_templates"}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key: " + key)
        result[key] = value
    return result


def regular_bytes(path):
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode):
        raise ValueError("expected regular carrier source file: " + str(path))
    return path.read_bytes()


def read_json(path):
    return json.loads(regular_bytes(path), object_pairs_hook=unique_object)


def check_carrier_tree(data):
    if not stat.S_ISDIR(data.lstat().st_mode):
        raise ValueError("carrier source root is not a regular directory")
    entries = {entry.name: entry for entry in data.iterdir()}
    if set(entries) != CARRIER_ROOT:
        raise ValueError("unexpected or missing carrier root entry: "
                         + str(sorted(set(entries) ^ CARRIER_ROOT)))
    for name in ("profiles", "selectors", "compatibility", "user_templates"):
        if not stat.S_ISDIR(entries[name].lstat().st_mode):
            raise ValueError("carrier source directory is a symlink or non-directory: " + name)
    for name in ("README.md", "SOURCES.md", "ios-carrier-profiles-MIT.txt"):
        regular_bytes(entries[name])
    for name, allowed in (("compatibility", {"registry-overlay.json"}),
                          ("user_templates", {"3_hk.json"})):
        children = {entry.name: entry for entry in entries[name].iterdir()}
        if set(children) != allowed:
            raise ValueError("unexpected or missing carrier support file: " + name)
        for child in children.values():
            regular_bytes(child)


def manifest_files(lock, revision):
    if not isinstance(lock, dict) or set(lock) != {"repo", "sha", "license", "files"}:
        raise ValueError("invalid carrier lock schema")
    if lock["repo"] != "iniwex5/ios-carrier-profiles" or lock["sha"] != revision:
        raise ValueError("carrier lock repository or revision differs from overlay")
    license_record = lock["license"]
    if not isinstance(license_record, dict) or set(license_record) != LICENSE_FIELDS:
        raise ValueError("invalid carrier license schema")
    if (license_record["key"] != "mit" or license_record["name"] != "MIT License"
            or license_record["spdx_id"] != "MIT"
            or license_record["url"] != "https://api.github.com/licenses/mit"
            or not isinstance(license_record["node_id"], str)
            or not license_record["node_id"]):
        raise ValueError("carrier lock lacks reviewed MIT SPDX provenance")
    rows = lock["files"]
    if not isinstance(rows, list):
        raise ValueError("invalid carrier lock file rows")
    files = {}
    for row in rows:
        if not isinstance(row, dict) or set(row) != {"path", "sha256"}:
            raise ValueError("invalid carrier lock file row")
        path, sha256 = row["path"], row["sha256"]
        if not isinstance(path, str) or not (
                path in METADATA or PROFILE.fullmatch(path) or SELECTOR.fullmatch(path)):
            raise ValueError("invalid carrier lock path: " + str(path))
        if not isinstance(sha256, str) or not HASH.fullmatch(sha256):
            raise ValueError("invalid carrier lock digest: " + path)
        if path in files:
            raise ValueError("duplicate carrier lock path: " + path)
        files[path] = sha256
    if not METADATA.issubset(files) or not any(PROFILE.fullmatch(p) for p in files) or not any(
            SELECTOR.fullmatch(p) for p in files):
        raise ValueError("incomplete carrier lock inventory")
    return files


def verify(root=ROOT):
    root = Path(root)
    data = root / "data/carriers"
    check_carrier_tree(data)
    overlay = read_json(data / "compatibility/registry-overlay.json")
    if not isinstance(overlay, dict) or overlay.get("schema_version") != 1:
        raise ValueError("invalid carrier compatibility overlay schema")
    revision = overlay.get("upstream_revision")
    if not isinstance(revision, str) or not COMMIT.fullmatch(revision):
        raise ValueError("invalid carrier compatibility upstream revision")
    manifest_dir = root / "docs/optimization/all-rust/carrier-reference-locks"
    if manifest_dir.is_symlink():
        raise ValueError("carrier lock directory is a symlink")
    lock_path = manifest_dir / f"ios-carrier-profiles-{revision}-manifest.json"
    expected = manifest_files(read_json(lock_path), revision)
    installed_expected = {path: sha for path, sha in expected.items()
                          if path == "LICENSE" or path.startswith(("profiles/", "selectors/"))}
    actual = FRESHNESS.local_files(data)
    changes = FRESHNESS.compare(actual, installed_expected)
    if any(changes.values()):
        raise ValueError("installed carrier bytes differ from source lock: "
                         + json.dumps(changes, sort_keys=True))
    return {"revision": revision, "profiles": sum(p.startswith("profiles/") for p in actual),
            "selectors": sum(p.startswith("selectors/") for p in actual),
            "files": len(actual), "exact_match": True}


if __name__ == "__main__":
    try:
        print(json.dumps(verify(), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError, json.JSONDecodeError) as error:
        raise SystemExit("carrier source preflight failed: " + str(error)) from error
