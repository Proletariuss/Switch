#!/usr/bin/env python3
"""Offline exact-commit carrier audit; never fetches or changes carrier data.

Read Git objects at the explicit revision, never the upstream working tree.
JSON output is deterministic and includes additions/removals and every hash.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def upstream_files(repo, revision):
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("revision must be a full lowercase Git commit SHA")
    commit = subprocess.check_output(
        ["git", "-C", str(repo), "rev-parse", revision + "^{commit}"], text=True).strip()
    if commit != revision:
        raise ValueError("revision did not resolve to the requested commit")
    tree = subprocess.check_output(
        ["git", "-C", str(repo), "ls-tree", "-r", "-z", revision])
    result = {}
    for entry in tree.split(b"\0"):
        if not entry:
            continue
        metadata, raw_path = entry.split(b"\t", 1)
        path = raw_path.decode("utf-8")
        if path not in ("LICENSE", "README.md", "index.yaml", "inventory.yaml") and not (
                path.startswith(("profiles/", "selectors/")) and path.endswith(".yaml")):
            continue
        mode, kind, oid = metadata.split()
        if mode != b"100644" or kind != b"blob":
            raise ValueError("non-regular upstream source: " + path)
        result[path] = digest(subprocess.check_output(
            ["git", "-C", str(repo), "cat-file", "blob", oid.decode("ascii")]))
    if "LICENSE" not in result or not any(p.startswith("profiles/") for p in result):
        raise ValueError("incomplete upstream source")
    return dict(sorted(result.items()))


def local_files(data):
    result = {}
    for kind in ("profiles", "selectors"):
        directory = data / kind
        if directory.is_symlink():
            raise ValueError("carrier directory is a symlink")
        for path in sorted(directory.iterdir()):
            if path.suffix != ".yaml" or not path.is_file() or path.is_symlink():
                raise ValueError("unexpected carrier source: " + str(path))
            result[kind + "/" + path.name] = digest(path.read_bytes())
    license_path = data / "ios-carrier-profiles-MIT.txt"
    if license_path.is_symlink():
        raise ValueError("carrier license is a symlink")
    result["LICENSE"] = digest(license_path.read_bytes())
    return dict(sorted(result.items()))


def compare(actual, expected):
    return {
        "added": sorted(expected.keys() - actual.keys()),
        "removed": sorted(actual.keys() - expected.keys()),
        "changed": sorted(key for key in actual.keys() & expected.keys()
                          if actual[key] != expected[key]),
    }


def validate_lock(lock, revision, expected):
    if lock.get("repo") != "iniwex5/ios-carrier-profiles" or lock.get("sha") != revision:
        raise ValueError("source lock repository or revision differs")
    rows = lock.get("files", [])
    files = {row["path"]: row["sha256"] for row in rows}
    if len(files) != len(rows) or files != expected:
        raise ValueError("source lock differs from exact upstream Git tree")
    if lock.get("license", {}).get("spdx_id") != "MIT":
        raise ValueError("source lock lacks reviewed MIT provenance")


def validate_compatibility(overlay, original_raw, source_lock):
    original = json.loads(original_raw)
    if (overlay.get("source_revision") != original["revision"] or
            overlay.get("source_registry_sha256") != digest(original_raw) or
            source_lock.get("sha") != original["revision"]):
        raise ValueError("compatibility source provenance differs")
    hashes = {row["path"]: row["sha256"] for row in source_lock["files"]}
    retained_paths = set()
    for alias, profile in overlay["profiles"].items():
        if not alias.startswith("legacy_") or profile != original["profiles"].get(alias[7:]):
            raise ValueError("compatibility profile differs from frozen historical source")
        retained_paths.add("profiles/" + alias[7:] + ".yaml")
    for plmn, manifest in overlay["selectors"].items():
        retained_paths.add("selectors/" + plmn + ".yaml")
        for candidate in manifest["profiles"]:
            path = candidate["path"].replace("profiles/legacy_", "profiles/", 1)
            matches = [row for row in original["selectors"][plmn]["profiles"]
                       if row["id"] == candidate["id"] and row["path"] == path]
            if len(matches) != 1:
                raise ValueError("compatibility selector source is absent or ambiguous")
            prior = matches[0]
            for rule in candidate["selectors"]:
                if rule not in prior["selectors"]:
                    raise ValueError("compatibility selector differs from historical source")
            expected = {key: value for key, value in prior.items() if key not in ("path", "selectors")}
            actual = {key: value for key, value in candidate.items() if key not in ("path", "selectors")}
            if expected != actual:
                raise ValueError("compatibility candidate metadata differs from source")
    expected_sources = [{"path": path, "sha256": hashes[path]} for path in sorted(retained_paths)]
    if overlay["source_files"] != expected_sources:
        raise ValueError("compatibility source-file hash attribution differs")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream-repo", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--data-dir", type=Path, default=ROOT / "data/carriers")
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    expected = upstream_files(args.upstream_repo, args.revision)
    validate_lock(json.loads(args.lock.read_text()), args.revision, expected)
    installed_expected = {key: value for key, value in expected.items()
                          if key == "LICENSE" or key.startswith(("profiles/", "selectors/"))}
    changes = compare(local_files(args.data_dir), installed_expected)
    compatibility = args.data_dir / "compatibility/registry-overlay.json"
    compatibility_report = None
    if compatibility.exists():
        raw = compatibility.read_bytes()
        overlay = json.loads(raw)
        if overlay["upstream_revision"] != args.revision:
            raise ValueError("compatibility overlay requires review for this upstream revision")
        historical = (ROOT / "rust/testdata/carrier-history/carrier-registry.json").read_bytes()
        prior_revision = json.loads(historical)["revision"]
        prior_lock = json.loads((ROOT / "docs/optimization/all-rust/carrier-reference-locks" /
                                 ("ios-carrier-profiles-" + prior_revision + "-manifest.json")).read_text())
        validate_compatibility(overlay, historical, prior_lock)
        compatibility_report = {"sha256": digest(raw), "source_revision": prior_revision,
                                "profiles": len(overlay["profiles"]),
                                "selectors": len(overlay["selectors"]),
                                "retained_rules": sum(len(c["selectors"]) for m in overlay["selectors"].values()
                                                      for c in m["profiles"])}
    report = {"schema_version": 1, "repo": "iniwex5/ios-carrier-profiles",
              "revision": args.revision, "source_file_count": len(expected),
              "profiles": sum(p.startswith("profiles/") for p in expected),
              "selectors": sum(p.startswith("selectors/") for p in expected),
              "exact_match": not any(changes.values()), "changes": changes,
              "compatibility": compatibility_report,
              "files": [{"path": key, "sha256": value} for key, value in expected.items()]}
    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(encoded)
        print(json.dumps({key: value for key, value in report.items() if key != "files"}, sort_keys=True))
    else:
        print(encoded, end="")
    return 0 if report["exact_match"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
