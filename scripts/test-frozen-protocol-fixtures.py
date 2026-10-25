#!/usr/bin/env python3
"""Replay reviewed, frozen independent protocol oracles without a Go toolchain.

Expected bytes were captured from the retired implementation, never generated
by the Rust examples being tested. SDP admission and rejection wording are
explicitly reviewed literals, not independent negotiation implementations.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "rust/testdata/retired-go-oracles"
DIRECT = {
    "effective-proposals.json": 2730,
    "usb-status.json": 85,
    "radio.json": 86,
    "http-methods.json": 839,
    "carrier-parameters.json": 2726,
}
PROTOCOL = {
    "pcma.json": ("proletarius-call", "pcma_fixture", 4),
    "amr-nb.json": ("proletarius-call", "amr_fixture", 64),
    "amr-wb.json": ("proletarius-call", "amr_wb_fixture", 24),
    "message-routing.json": ("proletarius-vowifi", "message_routing_fixture", 66),
    "ussd-decode.json": ("proletarius-ims", "ussd_decode_fixture", 10),
    "sip-wire.json": ("proletarius-ims", "ussd_request_fixture", 4),
    "call-target.json": ("proletarius-ims", "call_target_fixture", 10),
    "call-rejection.json": ("proletarius-ims", "call_rejection_fixture", 8),
    "sdp-admission.json": ("proletarius-call", "sdp_fixture", 57),
    "sms-submit.json": ("proletarius-sms", "submit_fixture", 50),
}


def validate_manifest(directory, name, expected_counts):
    manifest = json.loads((directory / name).read_text())
    if manifest.get("schema_version") != 1 or manifest.get("capture_iteration") != 511:
        raise ValueError(f"unsupported provenance schema: {name}")
    entries = manifest.get("fixtures", [])
    names = [entry.get("file") for entry in entries]
    if len(names) != len(set(names)) or set(names) != set(expected_counts):
        raise ValueError(f"missing, duplicate or unreviewed fixture in {name}")
    result = {}
    for entry in entries:
        filename = entry["file"]
        if Path(filename).name != filename:
            raise ValueError("fixture path must be a direct filename")
        path = directory / filename
        if path.is_symlink():
            raise ValueError("fixture symlinks are not permitted")
        raw = path.read_bytes()
        if len(raw) != entry["bytes"] or hashlib.sha256(raw).hexdigest() != entry["sha256"]:
            raise ValueError(f"frozen oracle hash/length changed: {filename}")
        if not entry.get("provenance") or not entry.get("source_files_sha256"):
            raise ValueError(f"missing independent-source provenance: {filename}")
        value = json.loads(raw)
        rows = value if isinstance(value, list) else value.get("cases", value.get("rows"))
        if not isinstance(rows, list) or len(rows) != entry["case_count"] or len(rows) != expected_counts[filename]:
            raise ValueError(f"oracle coverage changed: {filename}")
        if "pattern_count" in entry and len(value.get("patterns", [])) != entry["pattern_count"]:
            raise ValueError(f"HTTP pattern coverage changed: {filename}")
        if "fixture_schema_version" in entry and value.get("schema_version") != entry["fixture_schema_version"]:
            raise ValueError(f"fixture schema changed: {filename}")
        result[filename] = value
    return result


def validate_protocol(filename, value):
    crate, example, count = PROTOCOL[filename]
    if value.get("schema") != 1 or value.get("crate") != crate or value.get("example") != example:
        raise ValueError(f"unreviewed execution target/schema: {filename}")
    rows = value.get("rows", [])
    names = [row.get("name") for row in rows]
    if len(rows) != count or len(set(names)) != len(rows) or any(not isinstance(n, str) or not n for n in names):
        raise ValueError(f"missing/duplicate protocol cases: {filename}")
    for row in rows:
        if not isinstance(row.get("args"), list) or any(not isinstance(a, str) for a in row["args"]):
            raise ValueError(f"invalid arguments: {filename}/{row['name']}")
        alternatives = [k for k in ("expected_stdout", "expected_json", "expected_lines", "expected_prefix") if k in row]
        if len(alternatives) != 1:
            raise ValueError(f"missing/ambiguous result: {filename}/{row['name']}")
        expected_key = {"message-routing.json": "expected_json", "sms-submit.json": "expected_lines", "sdp-admission.json": "expected_prefix"}.get(filename, "expected_stdout")
        if alternatives[0] != expected_key:
            raise ValueError(f"weakened result comparison: {filename}/{row['name']}")
        if filename == "sdp-admission.json":
            if row["expected_prefix"] not in ("ok|", "error|") or row["args"][-1:] != ["--fixture-wideband-capabilities"]:
                raise ValueError("SDP admission requires explicit reviewed capabilities")
    return rows


def load_bundle(directory=FIXTURES):
    validate_manifest(directory, "direct-manifest.json", DIRECT)
    provenance = json.loads((directory / "protocol-manifest.json").read_text())
    if provenance.get("source_manifest_file") != "legacy-source-hashes.json":
        raise ValueError("missing legacy source provenance")
    source_hashes = (directory / "legacy-source-hashes.json").read_bytes()
    if hashlib.sha256(source_hashes).hexdigest() != provenance.get("source_manifest_sha256"):
        raise ValueError("legacy source provenance hash changed")
    source_hashes = json.loads(source_hashes)
    for entry in provenance["fixtures"]:
        for source, digest in entry["source_files_sha256"].items():
            if source_hashes.get(source) != digest:
                raise ValueError("exporter provenance disagrees with source snapshot")
    data = validate_manifest(directory, "protocol-manifest.json", {k: v[2] for k, v in PROTOCOL.items()})
    for filename, value in data.items():
        validate_protocol(filename, value)
    return data


def compare_result(filename, row, output):
    # Only the example's terminal newline is removed; wire bytes remain base64
    # or hex, so SIP whitespace, header order and PCM bytes are compared exactly.
    actual = output.rstrip("\r\n")
    if "expected_json" in row:
        matched = json.loads(actual) == row["expected_json"]
    elif "expected_lines" in row:
        matched = actual.splitlines() == row["expected_lines"]
    elif "expected_prefix" in row:
        matched = actual.startswith(row["expected_prefix"])
    else:
        matched = actual == row["expected_stdout"]
    if not matched:
        # Do not dump SMS text, headers or large PCM payloads on failure.
        raise AssertionError(f"frozen oracle mismatch: {filename}/{row['name']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-only", action="store_true", help="validate complete frozen data and provenance without running binaries")
    parser.add_argument("--skip-build", action="store_true", help="use already rebuilt Rust examples; normal gate always builds")
    args = parser.parse_args()
    data = load_bundle()
    print(f"Frozen oracle integrity: {sum(DIRECT.values())} direct cases and {sum(v[2] for v in PROTOCOL.values())} protocol invocations", flush=True)
    if args.check_only:
        return
    target = os.environ.get("CARGO_TARGET_DIR")
    if not target:
        raise ValueError("source scripts/workspace-env.sh first to contain build outputs")
    target_path = Path(target).resolve()
    if ROOT.parent / "build" not in target_path.parents:
        raise ValueError("CARGO_TARGET_DIR must remain inside this workspace build directory")
    if not args.skip_build:
        packages = {}
        for crate, example, _ in PROTOCOL.values():
            packages.setdefault(crate, set()).add(example)
        for crate, examples in packages.items():
            command = [os.environ.get("SWITCH_CARGO_BIN") or "cargo", "build", "--locked", "-p", crate]
            for example in sorted(examples):
                command += ["--example", example]
            subprocess.run(command, cwd=ROOT / "rust", check=True, timeout=900)
    consumed = 0
    for filename in sorted(data):
        value = data[filename]
        binary = target_path / "debug/examples" / PROTOCOL[filename][1]
        for row in value["rows"]:
            stdin = json.dumps(row["stdin"]) if "stdin" in row else None
            process = subprocess.run([str(binary), *row["args"]], input=stdin, text=True, capture_output=True, check=True, timeout=15)
            compare_result(filename, row, process.stdout)
            consumed += 1
        print(f"PASS {filename}: {len(value['rows'])} cases", flush=True)
    assert consumed == sum(v[2] for v in PROTOCOL.values())
    print(f"Frozen protocol replay passed: {consumed} invocations, no Go execution")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
