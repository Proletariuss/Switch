#!/usr/bin/env python3
"""Print a stable SHA-256 for the current source tree, excluding generated output."""

from __future__ import annotations

import hashlib
import os
import stat
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EXCLUDED = {".git", "build", "dist", "target", "DerivedData", ".build", "__pycache__"}
GENERATED_SUFFIXES = (".xcodeproj", ".xcworkspace", ".app", ".dSYM")


def generated_part(name: str) -> bool:
    return name in EXCLUDED or name.endswith(GENERATED_SUFFIXES)


def source_paths() -> list[str]:
    try:
        result = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=ROOT,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        paths = result.stdout.decode("utf-8", "surrogateescape").split("\0")
        return sorted(path for path in paths if path)
    except (OSError, subprocess.CalledProcessError):
        paths: list[str] = []
        for directory, names, filenames in os.walk(ROOT, followlinks=False):
            names[:] = sorted(
                name
                for name in names
                if not generated_part(name) and not (Path(directory) / name).is_symlink()
            )
            for filename in filenames:
                paths.append((Path(directory) / filename).relative_to(ROOT).as_posix())
        return sorted(paths)


def excluded(relative: str) -> bool:
    return any(generated_part(part) for part in Path(relative).parts)


def digest_tree() -> str:
    digest = hashlib.sha256()
    for relative in source_paths():
        if excluded(relative):
            continue
        path = ROOT / relative
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            # A deleted tracked path is not part of the source being built.
            continue
        if stat.S_ISLNK(metadata.st_mode):
            payload = os.readlink(path).encode("utf-8", "surrogateescape")
            kind = b"L"
        elif stat.S_ISREG(metadata.st_mode):
            payload = None
            kind = b"F"
        else:
            continue

        encoded_path = relative.encode("utf-8", "surrogateescape")
        digest.update(len(encoded_path).to_bytes(8, "big"))
        digest.update(encoded_path)
        digest.update(kind)
        digest.update(stat.S_IMODE(metadata.st_mode).to_bytes(4, "big"))
        if payload is not None:
            digest.update(len(payload).to_bytes(8, "big"))
            digest.update(payload)
            continue
        digest.update(metadata.st_size.to_bytes(8, "big"))
        with path.open("rb") as source:
            for block in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(block)
    return digest.hexdigest()


if __name__ == "__main__":
    try:
        print(digest_tree())
    except OSError as error:
        print(f"source identity failed: {error}", file=sys.stderr)
        raise SystemExit(1)
