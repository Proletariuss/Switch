#!/usr/bin/env python3
"""Allow source-only gates only inside their audited cold-build source export."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import stat
import sys

REQUIRED_SCRIPTS = (
    "scripts/test-migration-gates.sh",
    "scripts/check-cold-source-mode.py",
)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check(source_root: Path) -> list[str]:
    errors: list[str] = []
    if source_root.is_symlink():
        return ["source root is a symlink"]
    root = source_root.resolve(strict=True)
    capsule = root.parent
    clean_build = capsule.parent
    build = clean_build.parent
    if (root.name != "src" or clean_build.name != "clean-build"
            or build.name != "build"
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,90}", capsule.name)):
        return ["source-only mode requires build/clean-build/<capsule>/src"]
    if any(path.is_symlink() or not path.is_dir()
           for path in (root, capsule, clean_build, build)):
        return ["cold capsule path is missing or symlinked"]
    marker = capsule / "audit.json"
    if marker.is_symlink() or not marker.is_file() or not stat.S_ISREG(marker.lstat().st_mode):
        return ["matching cold capsule audit.json is missing or unsafe"]
    audit = json.loads(marker.read_text())
    expected_origin = build.parent / "src"
    capsule_ref = audit.get("capsule")
    origin_ref = audit.get("source_root")
    if (audit.get("schema") != 1 or not isinstance(capsule_ref, str)
            or not isinstance(origin_ref, str) or not Path(capsule_ref).is_absolute()
            or not Path(origin_ref).is_absolute()
            or Path(capsule_ref).resolve() != capsule
            or Path(origin_ref).resolve() != expected_origin.resolve()):
        errors.append("cold capsule audit identity does not match its source path")
    entries = audit.get("source")
    if not isinstance(entries, list) or not entries:
        return errors + ["cold capsule audit has no source manifest"]
    for relative in REQUIRED_SCRIPTS:
        rows = [row for row in entries if isinstance(row, dict) and row.get("path") == relative]
        path = root / relative
        if len(rows) != 1 or path.is_symlink() or not path.is_file():
            errors.append("cold capsule audit lacks a unique safe source entry: " + relative)
            continue
        row = rows[0]
        if (row.get("sha256") != sha256(path) or row.get("bytes") != path.stat().st_size
                or row.get("mode") != stat.S_IMODE(path.stat().st_mode)):
            errors.append("cold capsule audit source entry differs: " + relative)
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    args = parser.parse_args()
    try:
        errors = check(args.source_root)
    except (OSError, ValueError, TypeError, json.JSONDecodeError) as error:
        errors = [str(error)]
    print(json.dumps({"mode": "SOURCE_ONLY_NO_BUILD_ATTESTATION",
                      "valid": not errors, "errors": errors}, ensure_ascii=False))
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
