#!/usr/bin/env python3
"""Verify a built Switch.app's release identity and embedded Rust source digest."""

from __future__ import annotations

import plistlib
import re
import sys
from pathlib import Path


def fail(message: str) -> None:
    print(f"app identity failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> int:
    if len(sys.argv) != 6:
        fail("usage: verify-app-identity.py APP VERSION BUILD BACKEND_VERSION SOURCE_DIGEST")
    app, version, build, backend_version, source_digest = (
        Path(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]
    )
    if not re.fullmatch(r"[1-9][0-9]*\.[0-9]{2}", version):
        fail("expected App version is malformed")
    if not re.fullmatch(r"[1-9][0-9]*", build):
        fail("expected build number is malformed")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", backend_version):
        fail("expected backend version is malformed")
    if not re.fullmatch(r"[0-9a-f]{64}", source_digest):
        fail("expected source digest is malformed")

    info = app / "Contents/Info.plist"
    backend = app / "Contents/Resources/backend/switchd-rs"
    if app.suffix != ".app" or app.is_symlink() or not app.is_dir():
        fail("built app bundle is missing or unsafe")
    if info.is_symlink() or not info.is_file():
        fail("built app Info.plist is missing or unsafe")
    if backend.is_symlink() or not backend.is_file():
        fail("Rust backend executable is missing or unsafe")

    try:
        with info.open("rb") as stream:
            identity = plistlib.load(stream)
    except (OSError, plistlib.InvalidFileException, ValueError) as error:
        fail(f"cannot read app Info.plist: {error}")
    expected = {
        "CFBundleShortVersionString": version,
        "CFBundleVersion": build,
        "CFBundleIdentifier": "com.proletarius.switch",
    }
    for key, value in expected.items():
        if identity.get(key) != value:
            fail(f"{key} expected {value!r}, got {identity.get(key)!r}")

    try:
        backend_bytes = backend.read_bytes()
    except OSError as error:
        fail(f"cannot read Rust backend: {error}")
    embedded_identity = {
        "backend version": backend_version,
        "app release version": version,
        "app build number": build,
        "source digest": source_digest,
    }
    for name, value in embedded_identity.items():
        if value.encode("ascii") not in backend_bytes:
            fail(f"backend embedded {name} differs from release identity")

    print(
        f"App identity: PASS (app={version}, build={build}, backend={backend_version}, "
        f"Rust source={source_digest[:12]})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
