#!/usr/bin/env python3
"""Check that the App, Xcode project and Rust workspace share one release version."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def fail(message: str) -> None:
    print(f"version contract failed: {message}", file=sys.stderr)
    raise SystemExit(1)


version = (ROOT / "VERSION").read_text().strip()
build = (ROOT / "BUILD_NUMBER").read_text().strip()
if not re.fullmatch(r"[1-9][0-9]*\.[0-9]{2}", version):
    fail("VERSION must be major.two-digit-minor")
if not re.fullmatch(r"[1-9][0-9]*", build):
    fail("BUILD_NUMBER must be a positive integer without leading zeros")

project = (ROOT / "project.yml").read_text()
if re.findall(r'^\s+MARKETING_VERSION:\s+"([^"]+)"$', project, re.MULTILINE) != [version]:
    fail("project.yml MARKETING_VERSION differs from VERSION")
if re.findall(r'^\s+CURRENT_PROJECT_VERSION:\s+"([^"]+)"$', project, re.MULTILINE) != [build]:
    fail("project.yml CURRENT_PROJECT_VERSION differs from BUILD_NUMBER")

cargo = (ROOT / "rust/Cargo.toml").read_text()
workspace = re.search(r"(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\Z)", cargo)
rust_version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', workspace.group(1), re.MULTILINE) if workspace else None
if rust_version is None or rust_version.group(1) != version + ".0":
    fail("Rust workspace package version differs from VERSION")

print(f"Version contract: PASS (app={version}, build={build}, Rust={rust_version.group(1)})")
