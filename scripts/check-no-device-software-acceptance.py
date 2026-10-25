#!/usr/bin/env python3
"""Create or verify a fail-closed no-device software release attestation.

This scope is deliberately narrower than product acceptance.  It can attest a
matching source tree, full cold-capsule software tests/build, Rust-only bundle,
real daemon lifecycle in a hardware-denied sandbox, current metadata, and three
software reviews.  It never converts hardware, carrier, external delivery,
physical audio, long-run, or legacy rollback work into PASS.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import datetime
import hashlib
import importlib.util
import json
import os
import plistlib
import re
import stat
import subprocess
import sys
import tempfile
from pathlib import Path


SOURCE = Path(__file__).resolve().parents[1]
WORKSPACE = SOURCE.parent.resolve()
BUILD = WORKSPACE / "build"
SCHEMA_VERSION = 1
SCOPE = "no-device-software-release"
EXCLUSIONS = [
    "DJI_USB_QMI_AT_hardware",
    "PCSC_BLE_eSIM_hardware",
    "carrier_VoWiFi_SMS_call_interoperability",
    "physical_audio_permissions_quality_and_long_run",
    "external_Telegram_Bark_delivery",
    "legacy_3_42_downgrade_support",
]
CORE_EVIDENCE_NAMES = {
    "clean_build_audit", "clean_build_execution", "clean_build_artifact",
    "clean_build_log", "daemon_lifecycle", "forwarding_handoff_durability",
    "p6_harness", "active_ledger", "defect_closure",
}
REVIEWERS = [
    "independent-release-reviewer-alpha",
    "independent-release-reviewer-beta",
    "independent-release-reviewer-gamma",
]
PRECLOSURE_REVIEWERS = [
    "independent-defect-reviewer-alpha",
    "independent-defect-reviewer-beta",
    "independent-defect-reviewer-gamma",
]
REQUIRED_P6_SOURCES = {
    "src/scripts/check-p6-data-roundtrip.py",
    "src/scripts/check-p6-inplace-bundle-roundtrip.py",
    "src/scripts/check-p6-guarded-legacy-launch.py",
    "src/scripts/tests/test_p6_data_roundtrip.py",
    "src/scripts/tests/test_p6_inplace_bundle_roundtrip_cli.py",
    "src/scripts/tests/test_guarded_legacy_launch.py",
}
MACHO = {
    b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe",
    b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf",
}
EXPECTED_MACHO = {
    "Contents/MacOS/Switch",
    "Contents/Resources/backend/switchd-rs",
    "Contents/Resources/backend/libusb-1.0.0.dylib",
}
DEFECT_EVIDENCE_CATEGORIES = {
    "unit_test", "integration_test", "negative_test", "static_analysis",
    "source_review", "release_workflow",
}
FORWARDING_SCENARIOS = {
    "actual_swift_production_file_survives_process_exit_and_restart",
    "exact_compiled_rust_dto_then_independent_swift_production_file_verify",
    "fresh_real_daemon_swift_file_handoff_restart_replay_conflict_and_real_shutdown",
}
FORWARDING_EXACT_RUST_TEST = "tests::swift_forwarding_handoff_fixture_round_trips"
DAEMON_SCOPE = (
    "participating current daemon data-root and socket lifetime; "
    "no legacy execution or hardware recovery"
)
DAEMON_SCENARIOS = {
    "lifetime_claim_precedes_submitting_and_running_store_recovery",
    "three_uds_modes_and_missing_or_wrong_credentials",
    "same_root_same_socket",
    "same_root_other_socket",
    "same_inode_directory_alias",
    "different_roots_can_run_in_parallel",
    "concurrent_same_root_exactly_one_winner",
    "different_roots_concurrent_stale_reclaim_and_later_takeover",
    "sigterm_exit_then_restart_preserves_lock_inode",
    "foreign_active_public_socket_preserved",
    "foreign_active_callback_socket_preserved",
    "foreign_active_internal_socket_preserved",
    "partial_bind_callback_collision_cleans_only_owned_paths",
    "partial_bind_internal_collision_cleans_only_owned_paths",
    "unsafe_nonempty_lock_rejected_before_stores",
    "unsafe_directory_lock_rejected_before_stores",
    "unsafe_symlink_lock_rejected_before_stores",
    "unsafe_hardlink_lock_rejected_before_stores",
}
DAEMON_REJECTIONS = {
    "claim-before-recovery": "Switch data directory is already owned by another daemon",
    "same_root_same_socket": "Switch data directory is already owned by another daemon",
    "same_root_other_socket": "Switch data directory is already owned by another daemon",
    "same_root_alias": "Switch data directory is already owned by another daemon",
    "foreign-public": "backend_socket_in_use",
    "foreign-callback": "backend_socket_in_use",
    "foreign-internal": "backend_socket_in_use",
    "partial-challenger-callback": "backend_socket_in_use",
    "partial-challenger-internal": "backend_socket_in_use",
    "invalid-lock-nonempty": "Switch daemon lifetime lock is unsafe or changed",
    "invalid-lock-directory": "Switch daemon lifetime lock is unsafe or changed",
    "invalid-lock-symlink": "Switch daemon lifetime lock is unsafe or changed",
    "invalid-lock-hardlink": "Switch daemon lifetime lock is unsafe or changed",
}
DAEMON_SUCCESSES = {
    "first", "different_root", "stale-race-restarted-other-root", "restarted",
    "partial-owner-callback", "partial-owner-internal",
}
DAEMON_SCENARIO_PROCESSES = {
    "lifetime_claim_precedes_submitting_and_running_store_recovery": ("claim-before-recovery",),
    "three_uds_modes_and_missing_or_wrong_credentials": (),
    "same_root_same_socket": ("same_root_same_socket",),
    "same_root_other_socket": ("same_root_other_socket",),
    "same_inode_directory_alias": ("same_root_alias",),
    "different_roots_can_run_in_parallel": ("different_root",),
    "concurrent_same_root_exactly_one_winner": ("race-0", "race-1"),
    "different_roots_concurrent_stale_reclaim_and_later_takeover":
        ("stale-race-0", "stale-race-1", "stale-race-restarted-other-root"),
    "sigterm_exit_then_restart_preserves_lock_inode": ("restarted",),
    "foreign_active_public_socket_preserved": ("foreign-public",),
    "foreign_active_callback_socket_preserved": ("foreign-callback",),
    "foreign_active_internal_socket_preserved": ("foreign-internal",),
    "partial_bind_callback_collision_cleans_only_owned_paths":
        ("partial-owner-callback", "partial-challenger-callback"),
    "partial_bind_internal_collision_cleans_only_owned_paths":
        ("partial-owner-internal", "partial-challenger-internal"),
    "unsafe_nonempty_lock_rejected_before_stores": ("invalid-lock-nonempty",),
    "unsafe_directory_lock_rejected_before_stores": ("invalid-lock-directory",),
    "unsafe_symlink_lock_rejected_before_stores": ("invalid-lock-symlink",),
    "unsafe_hardlink_lock_rejected_before_stores": ("invalid-lock-hardlink",),
}
HANDOFF_PROCESS_LABELS = {
    "swift-produce", "swift-restart", "rust-exact-dto", "swift-verify",
    "handoff-daemon-first", "swift-live", "handoff-daemon-restarted",
    "swift-live-replay",
}
CLOSURE_COLD_TESTS = (
    "ims_sms_transactions::tests::ordinary_reader_reassembles_identical_text_segments_before_acknowledgement",
    "ims_sms_transactions::tests::native_consumer_archive_failure_never_enqueues_success_response",
    "message_archive::tests::multipart_archive_keeps_same_text_parts_distinct_and_commits_one_message",
    "reader_esim_download::tests::journal_fault_integration::composed_acceptance::composed_geteid_journal_fault_cancel_prevents_write_and_drains",
    "reader_esim_download::tests::retained_reader_owner_survives_cancelled_http_waiter_and_shutdown_drains_it",
    "reader_esim_download::tests::journal_fault_integration::composed_acceptance::composed_refresh_cancel_holds_permit_reservation_and_finalizer",
    "reader_esim_download::tests::shutdown_module_finalizer_waits_for_accepted_esim_owner_drain",
    "tests::signed_pcm_conversions_saturate_and_define_non_finite_inputs",
    "tests::streaming_resampler_keeps_long_run_rational_sample_count_and_phase",
    "tests::streaming_resampler_retains_a_bounded_history_at_worst_rate_ratio",
    "tests::downsample_fir_has_the_declared_linear_phase_group_delay",
    "module_devices::tests::same_group_selection_is_independent_of_enumeration_and_related_order",
    "module_devices::tests::two_modules_and_cross_group_pairs_are_unconfirmed",
    "module_devices::tests::defaults_on_any_related_endpoint_cannot_loop_back",
    "provider::tests::telegram_proxy_failures_are_retried_only_before_provider_submission",
    "provider::tests::refused_connection_retries_but_post_submission_server_errors_are_unknown",
    "provider::tests::bark_refused_local_connection_is_a_safe_pre_submit_retry",
    "worker::tests::worker_persists_and_stops_after_five_pre_submit_connect_failures",
    "worker::tests::worker_pauses_ambiguous_provider_result_without_replaying",
)
CLOSURE_SWIFT_MARKERS = (
    "[swift] BLE physical completion and manager regressions",
    "[swift] Module recovery isolation presentation",
    "[swift] Rust-owned mutation, recovery and durable UI intent boundaries",
    "[swift] Backend, BLE deadline and message state-machine regressions",
    "[swift] Rust reader auto-selection proposals and stale UI fences",
    "[swift] Audio playback and call bridge regressions",
)


@dataclass(frozen=True)
class EvidenceBlob:
    path: Path
    data: bytes
    sha256: str
    mode: int


@dataclass(frozen=True)
class FileSnapshot:
    sha256: str
    bytes: int
    mode: int
    device: int
    inode: int
    mtime_ns: int


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def safe_workspace_path(value: str, *, regular: bool = True) -> Path:
    relative = Path(value)
    require(not relative.is_absolute() and relative.parts and ".." not in relative.parts,
            f"unsafe evidence path: {value}")
    path = WORKSPACE / relative
    require(not path.is_symlink(), f"evidence symlink is not allowed: {value}")
    resolved = path.resolve(strict=True)
    require(resolved == WORKSPACE or WORKSPACE in resolved.parents,
            f"evidence escapes workspace: {value}")
    if regular:
        require(resolved.is_file(), f"evidence is not a regular file: {value}")
    else:
        require(resolved.is_dir(), f"evidence is not a directory: {value}")
    return resolved


def read_regular(path: Path, name: str) -> EvidenceBlob:
    """Read one private regular file once, without following a final symlink."""
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1,
            f"{name} must be a single-link regular file")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino)
                and stat.S_ISREG(opened.st_mode) and opened.st_nlink == 1,
                f"{name} changed before it was opened")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            data = stream.read()
        after = os.fstat(descriptor)
        require((after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
                == (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns),
                f"{name} changed while it was read")
    finally:
        os.close(descriptor)
    return EvidenceBlob(path=path, data=data, sha256=sha256_bytes(data),
                        mode=stat.S_IMODE(opened.st_mode))


def snapshot_regular(path: Path, name: str) -> FileSnapshot:
    """Hash a potentially large regular file through one stable descriptor."""
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1,
            f"{name} must be a single-link regular file")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino)
                and stat.S_ISREG(opened.st_mode) and opened.st_nlink == 1,
                f"{name} changed before it was opened")
        digest = hashlib.sha256()
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        after = os.fstat(descriptor)
        current = path.lstat()
        identity = (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns,
                    stat.S_IMODE(opened.st_mode))
        require(identity == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns,
                             stat.S_IMODE(after.st_mode))
                and identity == (current.st_dev, current.st_ino, current.st_size,
                                 current.st_mtime_ns, stat.S_IMODE(current.st_mode))
                and current.st_nlink == 1,
                f"{name} changed while it was hashed")
        return FileSnapshot(
            sha256=digest.hexdigest(), bytes=opened.st_size,
            mode=stat.S_IMODE(opened.st_mode), device=opened.st_dev,
            inode=opened.st_ino, mtime_ns=opened.st_mtime_ns,
        )
    finally:
        os.close(descriptor)


def relative(path: Path) -> str:
    resolved = path.resolve(strict=True)
    require(resolved == WORKSPACE or WORKSPACE in resolved.parents,
            f"evidence is outside workspace: {resolved}")
    require(not path.is_symlink(), f"evidence symlink is not allowed: {path}")
    return resolved.relative_to(WORKSPACE).as_posix()


def record(path: Path) -> dict[str, str]:
    require(path.is_file() and not path.is_symlink(), f"missing or unsafe evidence: {path}")
    blob = read_regular(path, f"evidence {path}")
    return {"path": relative(path), "sha256": blob.sha256}


def verify_record(item: object, name: str) -> EvidenceBlob:
    require(isinstance(item, dict) and set(item) == {"path", "sha256"},
            f"malformed {name} record")
    path = safe_workspace_path(item["path"])
    require(re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is not None,
            f"malformed {name} digest")
    blob = read_regular(path, name)
    require(blob.sha256 == item["sha256"], f"{name} digest changed")
    return blob


def load_json_bytes(data: bytes, name: str) -> dict:
    try:
        value = json.loads(data)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {name}: {error}") from error
    require(isinstance(value, dict), f"{name} must be a JSON object")
    return value


def load_json(path: Path, name: str) -> dict:
    return load_json_bytes(read_regular(path, name).data, name)


def attestation_argument(value: str) -> Path:
    path = Path(value)
    require(not path.is_symlink(), "attestation symlink is not allowed")
    resolved = path.resolve(strict=True)
    require(resolved.is_relative_to(WORKSPACE),
            "attestation must remain inside the workspace")
    return resolved


def source_digest() -> str:
    value = subprocess.check_output(
        [sys.executable, str(SOURCE / "scripts/source-identity.py")], text=True
    ).strip()
    require(re.fullmatch(r"[0-9a-f]{64}", value) is not None,
            "current source digest is malformed")
    return value


def current_active_ledger_report() -> tuple[dict, bytes]:
    """Recompute the active metadata report from the current source tree.

    An earlier green report is not sufficient release evidence: its inputs can
    drift while the summary booleans remain unchanged.  The verifier therefore
    creates a fresh report and requires byte-for-byte equivalent JSON data.
    """
    reports = BUILD / "reports"
    reports.mkdir(parents=True, exist_ok=True)
    descriptor, value = tempfile.mkstemp(
        prefix=".active-ledger-recheck.", suffix=".json", dir=reports
    )
    os.close(descriptor)
    temporary = Path(value)
    temporary.unlink()
    try:
        subprocess.run(
            [sys.executable, str(SOURCE / "scripts/check-acceptance-ledger.py"),
             "--root", str(SOURCE), "--report", str(temporary)],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        blob = read_regular(temporary, "fresh active ledger check")
        return load_json_bytes(blob.data, "fresh active ledger check"), blob.data
    finally:
        temporary.unlink(missing_ok=True)


def verify_active_ledger_snapshot(ledger: dict, fresh: dict, current_digest: str) -> None:
    require(ledger.get("source_digest") == current_digest
            and fresh.get("source_digest") == current_digest,
            "active metadata ledger is for a different source tree")
    require(ledger == fresh,
            "active metadata ledger does not match a fresh current-source check")
    require(ledger.get("metadata_consistent") is True
            and ledger.get("gate_satisfied") is True,
            "active metadata ledger is inconsistent")
    require(ledger.get("overall_denominator_frozen") is False
            and ledger.get("functional_acceptance_awarded") is False
            and ledger.get("overall_product_complete") is False,
            "software attestation must not promote the product ledger")


def verify_active_ledger_bytes(recorded: bytes, fresh: bytes) -> None:
    require(recorded == fresh,
            "active metadata ledger is not byte-equivalent to a fresh check")


def release_identity() -> tuple[str, str, str]:
    version = (SOURCE / "VERSION").read_text().strip()
    build = (SOURCE / "BUILD_NUMBER").read_text().strip()
    cargo = (SOURCE / "rust/Cargo.toml").read_text()
    match = re.search(
        r"(?ms)^\[workspace\.package\]\s*$.*?^version\s*=\s*\"([^\"]+)\"\s*$",
        cargo,
    )
    require(re.fullmatch(r"[1-9][0-9]*\.[0-9]{2}", version) is not None,
            "app version is malformed")
    require(re.fullmatch(r"[1-9][0-9]*", build) is not None,
            "build number is malformed")
    require(match is not None and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", match.group(1)),
            "backend version is malformed")
    return version, build, match.group(1)


def canonical_sha256(value: object) -> str:
    return sha256_bytes(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                   separators=(",", ":")).encode())


def verify_recovery_evidence(log: str, current_digest: str, capsule: Path,
                             execution: dict, artifact: dict) -> None:
    spec = importlib.util.spec_from_file_location(
        "release_recovery_process_checker", SOURCE / "scripts/check-recovery-process-gates.py")
    checker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(checker)
    bound = checker.verify_full_marker(log, current_digest, reports_root=capsule / "build/reports")
    require(execution.get("recovery_process_gate") == bound
            and artifact.get("recovery_process_gate") == bound,
            "cold execution/artifact do not bind the exact full recovery-process evidence")


def bundle_inventory(app: Path, *, workspace_bound: bool = True) -> dict:
    require(app.is_dir() and not app.is_symlink() and app.suffix == ".app",
            "audited App bundle is missing or unsafe")
    directories = [{"path": ".", "mode": stat.S_IMODE(app.lstat().st_mode)}]
    files = []
    for path in sorted(app.rglob("*")):
        entry = path.lstat()
        require(not stat.S_ISLNK(entry.st_mode),
                f"App bundle symlink is not allowed: {path}")
        relative_path = path.relative_to(app).as_posix()
        if stat.S_ISDIR(entry.st_mode):
            directories.append({
                "path": relative_path,
                "mode": stat.S_IMODE(entry.st_mode),
            })
            continue
        blob = read_regular(path, f"App bundle file {path}")
        files.append({
            "path": relative_path,
            "sha256": blob.sha256,
            "bytes": len(blob.data),
            "mode": blob.mode,
        })
    require(files, "audited App bundle is empty")
    tree = {"directories": directories, "files": files}
    return {
        "path": relative(app) if workspace_bound else str(app.resolve()),
        "file_count": len(files),
        "directory_count": len(directories),
        "tree_sha256": canonical_sha256(tree),
        "directories": directories,
        "files": files,
    }


def bundle_inventory_identity(value: object) -> dict:
    require(isinstance(value, dict)
            and set(value) == {"path", "file_count", "directory_count",
                               "tree_sha256", "directories", "files"}
            and isinstance(value.get("path"), str)
            and type(value.get("file_count")) is int
            and type(value.get("directory_count")) is int
            and isinstance(value.get("directories"), list)
            and isinstance(value.get("files"), list)
            and re.fullmatch(r"[0-9a-f]{64}", value.get("tree_sha256", "")) is not None,
            "App bundle inventory schema is malformed")
    return {key: value[key] for key in (
        "file_count", "directory_count", "tree_sha256", "directories", "files")}


def minimum_macos(load_commands: str) -> list[int]:
    versions = []
    for block in re.split(r"Load command \d+", load_commands):
        if "cmd LC_BUILD_VERSION" in block:
            found = re.search(r"^\s*minos ([0-9]+(?:\.[0-9]+){0,2})\s*$", block, re.M)
        elif "cmd LC_VERSION_MIN_MACOSX" in block:
            found = re.search(r"^\s*version ([0-9]+(?:\.[0-9]+){0,2})\s*$", block, re.M)
        else:
            continue
        require(found is not None, "Mach-O deployment version missing")
        parts = [int(value) for value in found.group(1).split(".")]
        versions.append(parts + [0] * (3 - len(parts)))
    require(len(versions) == 1, "expected one macOS deployment version")
    return versions[0]


def macho_dependencies(path: Path) -> list[str]:
    output = subprocess.check_output(["/usr/bin/otool", "-L", str(path)], text=True)
    dependencies = [line.strip().split(" (", 1)[0] for line in output.splitlines()[1:]]
    if path.suffix == ".dylib":
        dependencies = dependencies[1:]
    return dependencies


def forwarding_source_paths() -> set[str]:
    paths = {
        SOURCE / "scripts/check-forwarding-handoff-durability.py",
        SOURCE / "scripts/check-daemon-lifetime.py",
        SOURCE / "scripts/check-p6-data-roundtrip.py",
        SOURCE / "rust/Cargo.toml", SOURCE / "rust/Cargo.lock",
        SOURCE / "rust/crates/proletarius-handoff-core/Cargo.toml",
        SOURCE / "rust/crates/proletarius-audio-core/Cargo.toml",
        SOURCE / "rust/crates/proletarius-audio-core/src/lib.rs",
        SOURCE / "scripts/build-handoff-core.sh",
        SOURCE / "scripts/test-forwarding-authority.sh",
        SOURCE / "scripts/test-sms-forwarding-client.sh",
        SOURCE / "scripts/test-forwarding-handoff-compat.sh",
        SOURCE / "rust/crates/proletarius-api-contract/src/lib.rs",
        SOURCE / "rust/crates/proletarius-store/src/lib.rs",
        SOURCE / "rust/crates/proletariusd/src/main.rs",
        SOURCE / "rust/crates/proletariusd/src/forwarding_handoff_store.rs",
        SOURCE / "rust/crates/proletariusd/src/daemon_lifetime.rs",
        SOURCE / "rust/crates/proletariusd/src/module_recovery.rs",
        SOURCE / "rust/crates/proletariusd/src/shutdown.rs",
    }
    paths.update((SOURCE / "rust/crates/proletarius-handoff-core/src").rglob("*.rs"))
    paths.update((SOURCE / "app/Sources").glob("SMSForwarding*.swift"))
    paths.update(path for path in (SOURCE / "app/Tests/SMSForwardingClient").rglob("*")
                 if path.is_file())
    require(all(path.is_file() and path.is_relative_to(SOURCE) for path in paths),
            "required forwarding production source set is incomplete")
    return {str(path.relative_to(SOURCE)) for path in paths}


def verify_current_bundle(app: Path, artifact: dict, expected_inventory: dict) -> tuple[Path, str]:
    current = bundle_inventory(app)
    require(expected_inventory.get("path") == "dist/Switch.app",
            "artifact App inventory has an unexpected capsule-relative path")
    require(bundle_inventory_identity(current) == bundle_inventory_identity(expected_inventory),
            "cold App bundle tree changed after its audit")
    backend_dir = app / "Contents/Resources/backend"
    require(backend_dir.is_dir() and not backend_dir.is_symlink()
            and {path.name for path in backend_dir.iterdir()}
            == {"switchd-rs", "libusb-1.0.0.dylib"},
            "cold backend bundle is not the exact Rust-only set")
    rows = artifact.get("binaries")
    require(isinstance(rows, list) and all(isinstance(row, dict) for row in rows),
            "cold artifact binary inventory is missing")
    by_path = {row.get("path"): row for row in rows}
    require(set(by_path) == EXPECTED_MACHO and len(rows) == len(EXPECTED_MACHO),
            "cold artifact has an unexpected or duplicate Mach-O set")
    observed_macho = set()
    for path in sorted(app.rglob("*")):
        if not path.is_file() or path.is_symlink():
            continue
        with path.open("rb") as stream:
            if stream.read(4) in MACHO:
                observed_macho.add(path.relative_to(app).as_posix())
    require(observed_macho == EXPECTED_MACHO, "current cold App Mach-O set changed")
    for relative_path in sorted(EXPECTED_MACHO):
        row = by_path[relative_path]
        path = app / relative_path
        require(row.get("sha256") == sha256(path), f"cold Mach-O digest changed: {relative_path}")
        architecture = subprocess.check_output(
            ["/usr/bin/lipo", "-archs", str(path)], text=True).strip()
        require(architecture == row.get("architecture") == "arm64",
                f"cold Mach-O architecture changed: {relative_path}")
        loads = subprocess.check_output(["/usr/bin/otool", "-l", str(path)], text=True)
        require(minimum_macos(loads) == row.get("minimum_macos"),
                f"cold Mach-O deployment changed: {relative_path}")
        require(macho_dependencies(path) == row.get("dependencies"),
                f"cold Mach-O dependencies changed: {relative_path}")
    subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)],
                   check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    require(bundle_inventory_identity(bundle_inventory(app))
            == bundle_inventory_identity(current),
            "cold App bundle changed during final artifact verification")
    backend = backend_dir / "switchd-rs"
    return backend, sha256(backend)


def verify_p6_report(p6: dict) -> None:
    require(p6.get("schema_version") == 1
            and p6.get("syntax_and_whitespace_passed") is True
            and p6.get("full_p6_acceptance") is False
            and p6.get("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER"
            and p6.get("legacy_3_42_executed") is False
            and p6.get("functional_acceptance_awarded") is False,
            "P6 harness status or scope is invalid")
    sources = p6.get("source_sha256")
    require(isinstance(sources, dict) and set(sources) == REQUIRED_P6_SOURCES,
            "P6 harness source set is incomplete or unexpected")
    for value, digest in sources.items():
        verify_record({"path": value, "sha256": digest}, f"P6 source {value}")
    suites = p6.get("targeted_suites")
    require(isinstance(suites, list) and len(suites) >= 2, "P6 targeted suites are incomplete")
    suite_paths = set()
    for index, item in enumerate(suites):
        require(isinstance(item, dict) and set(item) == {"path", "sha256"},
                f"malformed P6 suite {index}")
        blob = verify_record(item, f"P6 suite {index}")
        require(blob.path.is_relative_to((BUILD / "reports").resolve()),
                "P6 suite must stay under build/reports")
        require(blob.path not in suite_paths, "duplicate P6 suite evidence")
        suite_paths.add(blob.path)
    counts = p6.get("suite_counts")
    require(isinstance(counts, dict) and type(counts.get("total")) is int
            and counts["total"] > 0
            and counts["total"] == sum(value for key, value in counts.items()
                                       if key != "total" and type(value) is int),
            "P6 suite counts are inconsistent")


def verify_runtime_artifacts(report: dict, label: str, *,
                             runtime_root: Path | None = None) -> None:
    """Bind the bytes of the sandbox policy, canary and every child log."""
    root = (runtime_root or BUILD / "runtime").resolve()

    def checked_log(row: dict, name: str) -> None:
        value = row.get("log")
        digest = row.get("log_sha256")
        require(isinstance(value, str) and Path(value).is_absolute()
                and re.fullmatch(r"[0-9a-f]{64}", digest or "") is not None,
                f"{name} log path/hash is missing")
        path = Path(value)
        require(path.exists() and not path.is_symlink()
                and path.resolve(strict=True).is_relative_to(root),
                f"{name} log is missing or outside the runtime evidence root")
        require(read_regular(path, name + " log").sha256 == digest,
                f"{name} log bytes changed")

    profile_value = report.get("sandbox_profile")
    profile_sha = report.get("sandbox_profile_sha256")
    require(isinstance(profile_value, str) and Path(profile_value).is_absolute()
            and re.fullmatch(r"[0-9a-f]{64}", profile_sha or "") is not None,
            label + " sandbox profile identity is missing")
    profile = Path(profile_value)
    require(profile.exists() and not profile.is_symlink()
            and profile.resolve(strict=True).is_relative_to(root)
            and read_regular(profile, label + " sandbox profile").sha256 == profile_sha,
            label + " sandbox profile bytes changed")
    canary = report.get("sandbox_canary")
    require(isinstance(canary, dict) and canary.get("inherited_child") is True
            and canary.get("exit_code") == 0 and canary.get("reaped") is True
            and canary.get("owned_process_group_gone") is True
            and canary.get("timeout") is False
            and canary.get("forced_cleanup") is False,
            label + " sandbox canary did not exit cleanly")
    checked_log(canary, label + " sandbox canary")
    children = report.get("processes")
    require(isinstance(children, list) and children
            and all(isinstance(row, dict) for row in children),
            label + " child process list is missing")
    for number, row in enumerate(children):
        require(row.get("sandbox_profile_sha256") == profile_sha,
                label + " child used another sandbox profile")
        checked_log(row, f"{label} child {number}")


def verify_child_runtime(row: dict, current_digest: str, label: str) -> None:
    version, build, backend_version = release_identity()
    runtime = row.get("ready_runtime")
    require(isinstance(runtime, dict)
            and runtime.get("source_digest") == current_digest
            and runtime.get("app_release_version") == version
            and str(runtime.get("build_number")) == build
            and runtime.get("version") == backend_version
            and runtime.get("backend") == "rust"
            and runtime.get("default_runtime") == "rust-native"
            and runtime.get("hardware_owner") == "rust"
            and runtime.get("hardware_ownership") is True
            and runtime.get("go_backend_connected") is False
            and runtime.get("go_backend_phase") == "retired",
            label + " child did not run the current Rust release")


def verify_forwarding_report(forwarding: dict, backend: Path, backend_hash: str,
                             *, runtime_root: Path | None = None,
                             current_digest: str | None = None) -> None:
    require(forwarding.get("schema_version") == 1
            and forwarding.get("passed") is True
            and forwarding.get("validation_completed") is True
            and forwarding.get("rust_dto_requested") is True
            and forwarding.get("full_cross_language_dto_roundtrip_passed") is True
            and forwarding.get("listed_inputs_zero_drift") is True
            and forwarding.get("all_processes_reaped") is True
            and forwarding.get("no_forced_cleanup") is True
            and forwarding.get("cleanup_errors") == [],
            "forwarding handoff durability did not fully pass")
    require(forwarding.get("purpose")
            == "actual production-file Swift restart, exact Rust DTO and real daemon durable handoff"
            and forwarding.get("scope")
            == "synthetic checkpoints and authenticated Unix HTTP only; no provider, SMS, hardware or carrier acceptance",
            "forwarding handoff purpose or non-product scope changed")
    scenarios = forwarding.get("scenarios")
    processes = forwarding.get("processes")
    require(isinstance(scenarios, list) and len(scenarios) == 3
            and all(isinstance(row, dict) and row.get("passed") is True for row in scenarios),
            "forwarding handoff scenario coverage is incomplete")
    by_name = {row.get("name"): row for row in scenarios}
    require(set(by_name) == FORWARDING_SCENARIOS,
            "forwarding handoff scenario set is incomplete or unexpected")
    swift = by_name["actual_swift_production_file_survives_process_exit_and_restart"]
    dto = by_name["exact_compiled_rust_dto_then_independent_swift_production_file_verify"]
    live = by_name["fresh_real_daemon_swift_file_handoff_restart_replay_conflict_and_real_shutdown"]
    require(swift.get("independent_process_reconstruction_equal") is True
            and dto.get("true_swift_rust_swift_roundtrip") is True
            and dto.get("exact_required_rust_test") == FORWARDING_EXACT_RUST_TEST
            and live.get("first_real_sigterm_exit_zero") is True
            and live.get("restarted_real_sigterm_exit_zero") is True
            and live.get("new_daemon_and_swift_process_read_original_file") is True
            and live.get("provider_configured_or_message_submitted") is False
            and live.get("live_checkpoint_directory_is_daemon_root") is True,
            "forwarding handoff lacks exact Swift, Rust DTO, daemon restart, or shutdown proof")
    require(isinstance(processes, list) and processes
            and all(isinstance(row, dict)
                    and row.get("reaped") is True and row.get("forced_cleanup") is False
                    and row.get("timeout") is not True
                    and row.get("owned_process_group_gone") is not False
                    for row in processes),
            "forwarding handoff process cleanup is incomplete")
    compiled = {row.get("program") for row in processes
                if row.get("kind") == "compiled_fixture"}
    rust_rows = [row for row in processes
                 if row.get("kind") == "compiled_fixture"
                 and row.get("arguments")
                 == ["--exact", FORWARDING_EXACT_RUST_TEST, "--ignored", "--nocapture"]]
    require(len(compiled) == 2 and None not in compiled and len(rust_rows) == 1,
            "forwarding handoff lacks the exact compiled Swift and Rust fixture processes")
    inputs = forwarding.get("binary_inputs")
    require(isinstance(inputs, dict) and len(inputs) >= 3
            and inputs.get(str(backend)) == backend_hash,
            "forwarding handoff used a different candidate daemon")
    for value, digest in inputs.items():
        path = Path(value)
        require(path.is_absolute() and path.resolve(strict=True).is_relative_to(BUILD.resolve()),
                "forwarding binary input escaped the build workspace")
        require(read_regular(path.resolve(), f"forwarding binary {value}").sha256 == digest,
                f"forwarding binary input changed: {value}")
    require(compiled.issubset(inputs),
            "forwarding compiled process executable is absent from binary inputs")
    require(all(row.get("program_sha256") == inputs.get(row.get("program"))
                for row in processes if row.get("kind") == "compiled_fixture"),
            "forwarding child executable hash differs from its compiled binary input")
    require(len(processes) == len(HANDOFF_PROCESS_LABELS)
            and {row.get("label") for row in processes} == HANDOFF_PROCESS_LABELS
            and all(row.get("exit_code") == 0
                    and row.get("reaped") is True
                    and row.get("owned_process_group_gone") is True
                    and row.get("timeout") is False
                    and row.get("forced_cleanup") is False
                    for row in processes)
            and all(row.get("sigterm_shutdown_confirmed") is True
                    for row in processes if row.get("label", "").startswith("handoff-daemon-")),
            "forwarding handoff lacks eight successful, fully reaped process outcomes")
    digest = current_digest or source_digest()
    for row in processes:
        if row["label"].startswith("handoff-daemon-"):
            verify_child_runtime(row, digest, "forwarding " + row["label"])
    for value in compiled:
        path = Path(value)
        require(os.access(path, os.X_OK), "forwarding compiled fixture is not executable")
        with path.open("rb") as stream:
            require(stream.read(4) in MACHO,
                    "forwarding compiled fixture is not a Mach-O executable")
    source_inputs = forwarding.get("source_inputs")
    require(isinstance(source_inputs, dict)
            and set(source_inputs) == forwarding_source_paths(),
            "forwarding production source input set is incomplete or unexpected")
    for value, digest in source_inputs.items():
        relative_path = Path(value)
        require(not relative_path.is_absolute() and ".." not in relative_path.parts,
                "unsafe forwarding source input")
        path = (SOURCE / relative_path).resolve(strict=True)
        require(path.is_relative_to(SOURCE) and read_regular(path, f"forwarding source {value}").sha256 == digest,
                f"forwarding source input changed: {value}")
    verify_runtime_artifacts(forwarding, "forwarding handoff", runtime_root=runtime_root)


def verify_daemon_report(daemon: dict, backend_hash: str,
                         *, runtime_root: Path | None = None,
                         current_digest: str | None = None) -> None:
    """Validate the controlled daemon-lifetime matrix, not just its row count.

    A count-only check can be satisfied by duplicating one passing scenario and
    omitting a mandatory failure branch.  The producer has a finite, reviewed
    scenario matrix, so the release verifier pins the exact unique names and
    the mandatory sandbox/process receipts.
    """
    require(daemon.get("passed") is True and daemon.get("validation_completed") is True,
            "daemon lifecycle validation did not pass")
    require(daemon.get("synthetic_only") is True
            and daemon.get("full_p6_acceptance") is False
            and daemon.get("scope") == DAEMON_SCOPE,
            "daemon lifecycle scope must stay controlled, synthetic and non-product")
    require(daemon.get("binary_sha256") == backend_hash,
            "daemon lifecycle used a different Rust binary")
    scenarios = daemon.get("scenarios")
    require(isinstance(scenarios, list)
            and all(isinstance(row, dict) for row in scenarios),
            "daemon lifecycle scenario ledger is malformed")
    names = [row.get("name") for row in scenarios]
    require(len(names) == len(DAEMON_SCENARIOS)
            and len(set(names)) == len(names)
            and set(names) == DAEMON_SCENARIOS
            and all(row.get("passed") is True
                    and isinstance(row.get("processes"), list)
                    for row in scenarios),
            "daemon lifecycle scenario set is incomplete, duplicated, or failed")
    canary = daemon.get("sandbox_canary")
    require(isinstance(canary, dict)
            and canary.get("inherited_child") is True
            and canary.get("exit_code") == 0
            and canary.get("forced_cleanup") is False
            and canary.get("timeout") is False
            and canary.get("surviving_owned_descendants") is not True
            and re.fullmatch(r"[0-9a-f]{64}", canary.get("log_sha256", "")) is not None,
            "daemon lifecycle mandatory sandbox canary is missing or failed")
    processes = daemon.get("processes")
    require(isinstance(processes, list) and len(processes) >= 20
            and all(isinstance(row, dict) for row in processes),
            "daemon lifecycle process ledger is incomplete")
    labels = [row.get("label") for row in processes]
    require(all(isinstance(label, str) and label for label in labels)
            and len(set(labels)) == len(labels),
            "daemon lifecycle process labels are missing or duplicated")
    require(all(type(row.get("exit_code")) is int
                and row.get("forced_cleanup") is False
                and row.get("timeout") is False
                and row.get("surviving_owned_descendants") is not True
                and re.fullmatch(r"[0-9a-f]{64}", row.get("log_sha256", "")) is not None
                for row in processes),
            "daemon lifecycle has an unreaped, timed-out, forced, or malformed process")
    by_label = {row.get("label"): row for row in processes}
    digest = current_digest or source_digest()
    require(len(processes) == 23
            and set(by_label) == DAEMON_SUCCESSES | set(DAEMON_REJECTIONS)
                | {"race-0", "race-1", "stale-race-0", "stale-race-1"},
            "daemon lifecycle lacks the exact 23-child result matrix")
    scenario_labels = []
    for scenario in scenarios:
        embedded = scenario["processes"]
        expected_labels = DAEMON_SCENARIO_PROCESSES[scenario["name"]]
        require([row.get("label") for row in embedded if isinstance(row, dict)]
                == list(expected_labels)
                and len(embedded) == len(expected_labels)
                and all(row == by_label.get(row.get("label")) for row in embedded),
                "daemon scenario does not bind its actual child receipts: "
                + scenario["name"])
        scenario_labels.extend(expected_labels)
    require(len(scenario_labels) == len(set(scenario_labels))
            and set(scenario_labels) == set(by_label) - {"first"},
            "daemon scenario child matrix is incomplete or repeated")
    for pair, expected_rejection in (
        (("race-0", "race-1"), "Switch data directory is already owned by another daemon"),
        (("stale-race-0", "stale-race-1"), "backend_socket_in_use"),
    ):
        require(sorted(by_label[name].get("exit_code") for name in pair) == [0, 1],
                "daemon concurrent pair did not have one winner and one rejection")
        loser = next(by_label[name] for name in pair if by_label[name]["exit_code"] == 1)
        require(loser.get("expected_rejection") == expected_rejection,
                "daemon concurrent loser has the wrong rejection")
    for label, row in by_label.items():
        expected = DAEMON_REJECTIONS.get(label)
        if label.startswith("race-") and row["exit_code"] == 1:
            expected = "Switch data directory is already owned by another daemon"
        if label.startswith("stale-race-") and row["exit_code"] == 1:
            expected = "backend_socket_in_use"
        if expected is None:
            require(row["exit_code"] == 0
                    and row.get("sigterm_shutdown_confirmed") is True,
                    "daemon positive child did not shut down normally: " + label)
            verify_child_runtime(row, digest, "daemon " + label)
        else:
            require(row["exit_code"] == 1
                    and row.get("expected_rejection") == expected,
                    "daemon negative child did not reject as expected: " + label)
            log_value = row.get("log")
            require(isinstance(log_value, str) and Path(log_value).exists()
                    and expected in read_regular(Path(log_value),
                        "daemon rejected child log").data.decode("utf-8", "strict"),
                    "daemon rejection is absent from the child's actual log: " + label)
    require(daemon.get("cleanup_errors") == [] and not daemon.get("fingerprint_error"),
            "daemon lifecycle has a cleanup or source-fingerprint error")
    ownership = daemon.get("interface_ownership", {})
    require(ownership.get("implemented") == ownership.get("total")
            and ownership.get("go") == 0 and ownership.get("total", 0) > 0,
            "daemon lifecycle did not confirm current all-Rust interface ownership")
    verify_runtime_artifacts(daemon, "daemon lifecycle", runtime_root=runtime_root)


def verify_defect_closure_rows(
    closure: dict,
    current_digest: str,
    closure_path: Path,
    cold_test_log: EvidenceBlob,
) -> None:
    rows = closure.get("defects")
    expected_ids = [f"B{index:02d}" for index in range(1, 13)]
    require(closure.get("schema_version") == 1 and closure.get("scope") == SCOPE
            and closure.get("source_digest") == current_digest,
            "B01-B12 closure is for a different schema, scope, or source")
    require(isinstance(rows, list) and all(isinstance(row, dict) for row in rows)
            and [row.get("id") for row in rows] == expected_ids
            and all(row.get("status") == "SOFTWARE_CLOSED" for row in rows),
            "B01-B12 software closure is incomplete or unordered")
    for row in rows:
        evidence = row.get("evidence")
        require(isinstance(evidence, list) and len(evidence) >= 2,
                f"{row.get('id')} needs the current cold gate and separate supporting evidence")
        seen = set()
        has_current_cold_test = False
        has_separate_support = False
        for index, item in enumerate(evidence):
            require(isinstance(item, dict)
                    and set(item) == {"category", "path", "sha256"}
                    and item.get("category") in DEFECT_EVIDENCE_CATEGORIES,
                    f"{row['id']} evidence {index} has an invalid category or schema")
            blob = verify_record({"path": item["path"], "sha256": item["sha256"]},
                                 f"{row['id']} evidence {index}")
            is_current_cold_test = (
                blob.path == cold_test_log.path
                and blob.sha256 == cold_test_log.sha256
                and item["category"] == "integration_test"
            )
            require(blob.path != closure_path,
                    f"{row['id']} evidence must not be the closure report itself")
            require(is_current_cold_test
                    or blob.path.is_relative_to((BUILD / "reports").resolve()),
                    f"{row['id']} evidence must be the exact current cold build.log "
                    "or a separate report under build/reports")
            require(blob.data.strip(), f"{row['id']} evidence {index} is empty")
            require(blob.path not in seen, f"{row['id']} repeats the same evidence")
            seen.add(blob.path)
            if is_current_cold_test:
                has_current_cold_test = True
            if blob.path != cold_test_log.path:
                has_separate_support = True
        require(has_current_cold_test,
                f"{row['id']} is not bound to the current-source cold migration gate")
        require(has_separate_support,
                f"{row['id']} has no separate defect-specific supporting evidence")
    require(closure.get("software_defects_closed") == 12
            and closure.get("product_acceptance_complete") is False
            and closure.get("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER",
            "B01-B12 closure overclaims product/device acceptance")


CLOSURE_RELEASE_FLOW_MARKER = 'release flow tests passed (30 cases; mandatory initialization once, two bundle gates, cp-RX ordering, isolated fake app/text image only)'
CLOSURE_READER_AUDIO_MARKER = 'PASS: 4 production stale-recovery scenarios, 2 live timer controls and 9 production startup/cancellation/failure scenarios (virtual platform; no devices or network)'


def verify_closure_cold_markers(cold_text: str) -> None:
    cold_lines = cold_text.splitlines()
    version, build, backend_version = release_identity()
    required_lines = (
        CLOSURE_READER_AUDIO_MARKER,
        CLOSURE_RELEASE_FLOW_MARKER,
        f"Version contract: PASS (app={version}, build={build}, Rust={backend_version})",
    )
    require(cold_lines.count("migration gates passed") == 1
            and "PASS: ReaderESIM identity/recovery groups" in cold_text
            and all("test " + name + " ... ok" in cold_lines
                    for name in CLOSURE_COLD_TESTS)
            and all(line in cold_lines for line in required_lines)
            and cold_lines.count(CLOSURE_RELEASE_FLOW_MARKER) == 1
            and cold_lines.count(CLOSURE_READER_AUDIO_MARKER) == 1
            and all(cold_lines.count(marker) == 1 for marker in CLOSURE_SWIFT_MARKERS)
            and not any(line.startswith("test result: FAILED") for line in cold_lines)
            and not re.search(r"^test result: .*; [1-9][0-9]* failed;", cold_text, re.M),
            "closure cold log lacks exact B01-B12/eSIM software gate evidence")


def verify_defect_closure_chain(
    closure: dict, current_digest: str, closure_path: Path,
    cold_test_log: EvidenceBlob, *, reports_root: Path | None = None,
    cold_root_override: Path | None = None,
    runtime_root_override: Path | None = None,
) -> None:
    """Independently verify the current closure inputs and ordered reviews.

    The BUILD-side generator is evidence, not an authority for this check.
    In particular, the final attestation must reject a hand-written closure
    that omits the second build or its three pre-closure reviews.
    """
    reports = (reports_root or BUILD / "reports").resolve()
    cold_root = (cold_root_override or BUILD / "clean-build").resolve()
    runtime_root = (runtime_root_override or BUILD / "runtime").resolve()
    require(closure_path.parent.resolve() == reports and closure_path.suffix == ".json",
            "closure is not in the reviewed reports directory")
    capsule_value = closure.get("capsule")
    require(isinstance(capsule_value, str), "closure capsule is missing")
    capsule = safe_workspace_path(capsule_value, regular=False)
    capsule_match = re.fullmatch(r"i([0-9]{3,})-current-([0-9a-f]{8})", capsule.name)
    require(capsule_match is not None and int(capsule_match.group(1)) >= 690
            and closure.get("iteration") == int(capsule_match.group(1))
            and capsule_match.group(2) == current_digest[:8]
            and closure_path.name.startswith(f"iteration-{capsule_match.group(1)}-")
            and capsule.parent == cold_root and cold_test_log.path == capsule / "build.log",
            "closure is not from its exact current-source cold capsule")
    iteration = int(capsule_match.group(1))
    prefix = f"iteration-{iteration}-"

    def pinned(value: object, label: str, expected: Path) -> EvidenceBlob:
        blob = verify_record(value, label)
        require(blob.path == expected, f"{label} has the wrong source path")
        return blob

    cold = closure.get("cold_evidence")
    require(isinstance(cold, dict) and set(cold) == {"audit", "execution", "artifact", "log"},
            "closure cold evidence set is incomplete")
    cold_blobs = {key: pinned(cold[key], f"closure cold {key}", capsule / name)
                  for key, name in (("audit", "audit.json"), ("execution", "execution.json"),
                                    ("artifact", "artifact-audit.json"), ("log", "build.log"))}
    require(cold_blobs["log"].sha256 == cold_test_log.sha256,
            "closure cold log differs from the final attestation cold log")
    audit = load_json_bytes(cold_blobs["audit"].data, "closure cold audit")
    execution = load_json_bytes(cold_blobs["execution"].data, "closure cold execution")
    artifact = load_json_bytes(cold_blobs["artifact"].data, "closure cold artifact")
    require(audit.get("schema") == 1 and audit.get("capsule") == str(capsule)
            and audit.get("source_root") == str(SOURCE)
            and execution.get("status") == "built"
            and execution.get("software_tests") == "passed"
            and execution.get("fresh_output_validation") == "passed"
            and execution.get("go_resolvable") is False
            and execution.get("historical_evidence_scope")
                == "SOURCE_ONLY_NO_BUILD_ATTESTATION"
            and execution.get("audit_sha256") == cold_blobs["audit"].sha256
            and artifact.get("status") == "passed"
            and artifact.get("software_tests") == "passed"
            and artifact.get("source_digest") == current_digest
            and artifact.get("audit_sha256") == cold_blobs["audit"].sha256
            and artifact.get("execution_sha256") == cold_blobs["execution"].sha256,
            "closure cold evidence is not a passed same-source capsule chain")
    commands = execution.get("commands")
    test_command = ["/bin/sh", "scripts/test-migration-gates.sh"]
    build_command = ["/bin/sh", "scripts/build.sh"]
    require(commands == [
        [str(capsule / "tools/python3"), "scripts/check-rust-backend-exit.py",
         "--require-go-free"],
        [str(capsule / "tools/python3"), "scripts/prepare-module-supervisor.py",
         "--fetch"],
        test_command, build_command,
    ], "closure cold build did not run the exact four-step gate before the App build")
    cold_text = cold_blobs["log"].data.decode("utf-8", "strict")
    version, build, backend_version = release_identity()
    verify_closure_cold_markers(cold_text)

    app = capsule / "dist/Switch.app"
    backend = app / "Contents/Resources/backend/switchd-rs"
    require(app.is_dir() and not app.is_symlink()
            and artifact.get("app") == str(app),
            "closure App differs from the audited final capsule App")
    backend_sha = read_regular(backend, "closure final App backend").sha256

    generator = closure.get("generator")
    require(isinstance(generator, dict)
            and set(generator) == {"path", "sha256", "predecessor_sha256",
                                   "prepared_predecessor_sha256"},
            "closure generator identity is incomplete")
    generator_blob = pinned(
        {key: generator[key] for key in ("path", "sha256")},
        "closure generator", reports / f"{prefix}prepare-defect-closure.py")
    prepared_predecessor = ("iteration-684-prepare-defect-closure.py" if iteration == 690
                            else f"iteration-{iteration - 1}-prepare-defect-closure.py")
    for key, name in (("predecessor_sha256", "iteration-677-prepare-defect-closure.py"),
                      ("prepared_predecessor_sha256", prepared_predecessor)):
        require(read_regular(reports / name, "closure predecessor " + name).sha256
                == generator[key], "closure generator predecessor pin changed")

    repeat_blob = pinned(closure.get("offline_repeat"), "closure offline repeat",
                         capsule / f"{prefix}offline-repeat.json")
    repeat_log = read_regular(capsule / f"{prefix}offline-repeat.log",
                              "closure offline repeat log")
    mirror = read_regular(reports / f"{prefix}b11-offline-repeat-evidence.json",
                          "closure B11 offline repeat mirror")
    repeat = load_json_bytes(repeat_blob.data, "closure offline repeat")
    pre_repeat_artifact = pinned(
        repeat.get("pre_repeat_artifact_audit"),
        "closure pre-repeat artifact audit",
        capsule / f"{prefix}pre-repeat-artifact-audit.json")
    pre_artifact = load_json_bytes(pre_repeat_artifact.data,
                                   "closure pre-repeat artifact audit")
    repeat_lines = repeat_log.data.decode("utf-8", "strict").splitlines()
    require(mirror.sha256 == repeat_blob.sha256 and mirror.data == repeat_blob.data
            and repeat.get("source_digest") == current_digest
            and repeat.get("app_version") == version
            and str(repeat.get("app_build")) == build
            and repeat.get("backend_version") == backend_version
            and repeat.get("passed") is True
            and repeat.get("offline") is True
            and repeat.get("rustup_auto_install") is False
            and repeat.get("build_exit_code") == 0
            and repeat.get("command") == build_command
            and repeat.get("execution_sha256") == cold_blobs["execution"].sha256
            and repeat.get("post_repeat_artifact_audit_sha256") == cold_blobs["artifact"].sha256
            and repeat.get("pre_repeat_artifact_audit_sha256") == pre_repeat_artifact.sha256
            and pre_artifact.get("status") == "passed"
            and pre_artifact.get("software_tests") == "passed"
            and pre_artifact.get("source_digest") == current_digest
            and pre_artifact.get("audit_sha256") == cold_blobs["audit"].sha256
            and pre_artifact.get("execution_sha256") == cold_blobs["execution"].sha256
            and pre_artifact.get("app") == str(app)
            and repeat.get("log_sha256") == repeat_log.sha256
            and "** BUILD SUCCEEDED **" in repeat_lines
            and [line for line in repeat_lines if line.strip()][-1]
                == "构建完成：" + str(app)
            and "test result: FAILED" not in repeat_log.data.decode("utf-8", "strict")
            and not re.search(r"^test result: .*; [1-9][0-9]* failed;",
                              repeat_log.data.decode("utf-8", "strict"), re.M),
            "closure B11 is not a complete second offline App build on the same source")

    process_records = {}
    for key in ("current_module_upgrade", "current_daemon_lifecycle",
                "current_forwarding_handoff"):
        blob = verify_record(closure.get(key), "closure " + key)
        require(blob.path.parent == reports and blob.path.name.startswith(prefix)
                and blob.path.suffix == ".json", key + " is not a current report")
        report = load_json_bytes(blob.data, "closure " + key)
        backend_bound = (report.get("binary_inputs", {}).get(str(backend)) == backend_sha
                         if key == "current_forwarding_handoff" else
                         report.get("binary") == str(backend)
                         and report.get("binary_sha256") == backend_sha)
        require(report.get("passed") is True and report.get("validation_completed") is True
                and backend_bound
                and report.get("cleanup_errors") == [],
                key + " does not bind a successful final-App backend run")
        process_records[key] = blob
    upgrade = load_json_bytes(process_records["current_module_upgrade"].data,
                              "closure module upgrade")
    verify_runtime_artifacts(upgrade, "closure module upgrade",
                             runtime_root=runtime_root)
    scenarios = upgrade.get("scenarios")
    require(upgrade.get("schema") == 1 and upgrade.get("synthetic_only") is True
            and upgrade.get("product_acceptance") is False
            and upgrade.get("binary_unchanged") is True
            and isinstance(scenarios, list)
            and all(isinstance(row, dict) for row in scenarios)
            and [row.get("name") for row in scenarios]
            == ["old_empty_lock_pair_upgrade_and_restart", "pending_history_not_reclassified"]
            and all(row.get("passed") is True for row in scenarios)
            and scenarios[0].get("ordinary_admission_and_same_root_restart") is True
            and scenarios[0].get("original_locks_unchanged") is True
            and scenarios[1].get("pending_preserved") is True,
            "closure module upgrade lacks the final two-scenario proof")
    upgrade_processes = upgrade.get("processes")
    require(isinstance(upgrade_processes, list) and len(upgrade_processes) == 3
            and all(isinstance(row, dict) for row in upgrade_processes)
            and [row.get("label") for row in upgrade_processes]
            == ["upgrade-start-0", "upgrade-start-1", "pending-stays-quarantined"],
            "closure module upgrade omitted its three actual process receipts")
    for row in upgrade_processes:
        runtime = row.get("ready_runtime")
        log_path = Path(row.get("log", ""))
        require(row.get("exit_code") == 0
                and row.get("sigterm_shutdown_confirmed") is True
                and row.get("timeout") is False
                and row.get("forced_cleanup") is False
                and not row.get("surviving_owned_descendants")
                and isinstance(runtime, dict)
                and runtime.get("source_digest") == current_digest
                and runtime.get("app_release_version") == version
                and str(runtime.get("build_number")) == build
                and runtime.get("version") == backend_version
                and runtime.get("backend") == "rust"
                and runtime.get("default_runtime") == "rust-native"
                and runtime.get("hardware_owner") == "rust"
                and runtime.get("hardware_ownership") is True
                and runtime.get("go_backend_connected") is False
                and runtime.get("go_backend_phase") == "retired"
                and log_path.is_absolute()
                and log_path.resolve(strict=True).is_relative_to(runtime_root)
                and re.fullmatch(r"[0-9a-f]{64}", row.get("log_sha256", "")) is not None
                and read_regular(log_path, "closure module process log").sha256
                    == row["log_sha256"],
                "closure module upgrade child is not a clean same-source process")
    daemon_report = load_json_bytes(process_records["current_daemon_lifecycle"].data,
                                    "closure daemon lifetime")
    daemon_sources = daemon_report.get("source_sha256")
    require(isinstance(daemon_sources, dict) and daemon_sources
            and all(isinstance(name, str) and isinstance(digest, str)
                    and not Path(name).is_absolute() and ".." not in Path(name).parts
                    and read_regular(SOURCE / name, "closure daemon source " + name).sha256
                        == digest for name, digest in daemon_sources.items()),
            "closure daemon lifetime source pins changed")
    handoff_report = load_json_bytes(process_records["current_forwarding_handoff"].data,
                                     "closure forwarding handoff")
    handoff_sources = handoff_report.get("source_inputs")
    require(isinstance(handoff_sources, dict) and handoff_sources
            and all(isinstance(name, str) and isinstance(digest, str)
                    and not Path(name).is_absolute() and ".." not in Path(name).parts
                    and read_regular(SOURCE / name, "closure handoff source " + name).sha256
                        == digest for name, digest in handoff_sources.items()),
            "closure handoff source pins changed")

    pins = closure.get("reviewed_cli_pins")
    require(isinstance(pins, dict)
            and pins.get("validator_sha256") == read_regular(
                SOURCE / "scripts/check-no-device-software-acceptance.py",
                "current acceptance validator").sha256
            and pins.get("source_identity_sha256") == read_regular(
                SOURCE / "scripts/source-identity.py", "current source identity").sha256
            and all(pins.get(key + "_sha256") == cold_blobs[key].sha256
                    for key in ("audit", "execution", "artifact", "log"))
            and pins.get("offline_repeat_sha256") == repeat_blob.sha256
            and pins.get("offline_repeat_log_sha256") == repeat_log.sha256
            and pins.get("module_upgrade_final_sha256")
                == process_records["current_module_upgrade"].sha256
            and pins.get("daemon_final_sha256")
                == process_records["current_daemon_lifecycle"].sha256
            and pins.get("handoff_final_sha256")
                == process_records["current_forwarding_handoff"].sha256,
            "closure CLI pins do not bind the current validator and exact evidence")

    expected_reviewed = {
        "generator": generator_blob.sha256,
        "cold_audit": cold_blobs["audit"].sha256,
        "cold_execution": cold_blobs["execution"].sha256,
        "cold_artifact": cold_blobs["artifact"].sha256,
        "cold_log": cold_blobs["log"].sha256,
        "offline_repeat": repeat_blob.sha256,
        "offline_repeat_log": repeat_log.sha256,
        "module_upgrade": process_records["current_module_upgrade"].sha256,
        "daemon_lifecycle": process_records["current_daemon_lifecycle"].sha256,
        "forwarding_handoff": process_records["current_forwarding_handoff"].sha256,
    }
    reviews = closure.get("preclosure_reviews")
    require(isinstance(reviews, list) and len(reviews) == 3
            and pins.get("preclosure_review_sha256")
            == [item.get("sha256") for item in reviews if isinstance(item, dict)],
            "closure needs exactly three pinned pre-closure reviews")
    previous_hash = None
    previous_time = None
    reviewers = set()
    for number, item in enumerate(reviews, 1):
        blob = pinned(item, f"closure pre-closure review {number}",
                      reports / f"{prefix}defect-preclosure-review-{number}.json")
        review = load_json_bytes(blob.data, f"closure pre-closure review {number}")
        timestamp = review.get("reviewed_at_utc")
        require(isinstance(timestamp, str) and timestamp.endswith("Z"),
                "pre-closure review needs a UTC timestamp")
        try:
            reviewed_at = datetime.datetime.fromisoformat(timestamp[:-1] + "+00:00")
        except ValueError as error:
            raise ValueError("pre-closure review timestamp is invalid") from error
        reviewer = review.get("reviewer")
        require(review.get("schema_version") == 1
                and review.get("review_id") == f"i{iteration}-b01-b12-preclosure-review-{number}"
                and review.get("round") == number
                and reviewer == PRECLOSURE_REVIEWERS[number - 1]
                and reviewer not in reviewers
                and review.get("scope") == "finite-software-defect-preclosure"
                and review.get("source_digest") == current_digest
                and review.get("previous_review_sha256") == previous_hash
                and review.get("reviewed_evidence_sha256") == expected_reviewed
                and review.get("verdict") == "PASS"
                and review.get("blocking_software_findings") == []
                and review.get("product_complete") is False
                and review.get("functional_acceptance_awarded") is False
                and review.get("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER"
                and isinstance(review.get("review_summary"), str)
                and review["review_summary"].strip()
                and (previous_time is None or reviewed_at > previous_time),
                f"pre-closure review {number} is not an independent ordered approval")
        reviewers.add(reviewer)
        previous_hash, previous_time = blob.sha256, reviewed_at
    created = closure.get("created_at_utc")
    require(isinstance(created, str), "closure creation timestamp is missing")
    try:
        created_at = datetime.datetime.fromisoformat(created.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError("closure creation timestamp is invalid") from error
    require(created_at.tzinfo is not None and created_at > previous_time,
            "closure predates its final pre-closure review")


def verify_defect_closure(
    closure: dict, current_digest: str, closure_path: Path,
    cold_test_log: EvidenceBlob,
) -> None:
    verify_defect_closure_rows(closure, current_digest, closure_path, cold_test_log)
    verify_defect_closure_chain(closure, current_digest, closure_path, cold_test_log)


def self_test_defect_closure_chain(root: Path, capsule: Path, current_digest: str,
                                   rejected) -> None:
    """Synthetic files exercise the independent chain without running a build."""
    iteration = 691
    prefix = f"iteration-{iteration}-"
    def write(path: Path, value: object) -> dict[str, str]:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value, sort_keys=True) + "\n")
        return record(path)

    cold_log = capsule / "build.log"
    version, build, backend_version = release_identity()
    cold_log.write_text("\n".join([
        *("test " + name + " ... ok" for name in CLOSURE_COLD_TESTS),
        *CLOSURE_SWIFT_MARKERS,
        CLOSURE_READER_AUDIO_MARKER,
        CLOSURE_RELEASE_FLOW_MARKER,
        f"Version contract: PASS (app={version}, build={build}, Rust={backend_version})",
        "PASS: ReaderESIM identity/recovery groups", "migration gates passed", "",
    ]))
    cold_text = cold_log.read_text()
    verify_closure_cold_markers(cold_text)
    rejected(lambda: verify_closure_cold_markers(
        cold_text.replace("test " + CLOSURE_COLD_TESTS[0] + " ... ok", "")),
        "missing B01-B12 cold test was accepted")
    rejected(lambda: verify_closure_cold_markers(
        cold_text.replace(CLOSURE_SWIFT_MARKERS[-1], "")),
        "missing cold Swift marker was accepted")
    for marker, legacy in (
        (CLOSURE_RELEASE_FLOW_MARKER, 'release flow tests passed (isolated fake app and text image only)'),
        (CLOSURE_READER_AUDIO_MARKER, 'PASS: 4 production stale-recovery scenarios and 2 live timer controls (virtual platform; no devices or network)'),
    ):
        rejected(lambda: verify_closure_cold_markers(cold_text.replace(marker, legacy)),
                 "legacy producer marker was accepted")
        rejected(lambda: verify_closure_cold_markers(cold_text.replace(marker, "")),
                 "missing producer marker was accepted")
        rejected(lambda: verify_closure_cold_markers(cold_text + marker + "\n"),
                 "duplicate producer marker was accepted")
    audit = write(capsule / "audit.json", {"schema": 1, "capsule": str(capsule),
                                           "source_root": str(SOURCE)})
    execution = write(capsule / "execution.json", {
        "status": "built", "software_tests": "passed", "audit_sha256": audit["sha256"],
        "fresh_output_validation": "passed", "go_resolvable": False,
        "historical_evidence_scope": "SOURCE_ONLY_NO_BUILD_ATTESTATION",
        "commands": [[str(capsule / "tools/python3"),
                      "scripts/check-rust-backend-exit.py", "--require-go-free"],
                     [str(capsule / "tools/python3"),
                      "scripts/prepare-module-supervisor.py", "--fetch"],
                     ["/bin/sh", "scripts/test-migration-gates.sh"],
                     ["/bin/sh", "scripts/build.sh"]],
    })
    app = capsule / "dist/Switch.app"
    backend = app / "Contents/Resources/backend/switchd-rs"
    backend.parent.mkdir(parents=True)
    backend.write_bytes(b"synthetic final backend")
    backend_hash = sha256(backend)
    artifact = write(capsule / "artifact-audit.json", {
        "status": "passed", "software_tests": "passed", "source_digest": current_digest,
        "audit_sha256": audit["sha256"], "execution_sha256": execution["sha256"],
        "app": str(app),
    })
    cold = {"audit": audit, "execution": execution, "artifact": artifact,
            "log": record(cold_log)}
    (root / "iteration-677-prepare-defect-closure.py").write_text("old predecessor\n")
    (root / "iteration-690-prepare-defect-closure.py").write_text("prepared predecessor\n")
    generator = root / f"{prefix}prepare-defect-closure.py"
    generator.write_text("synthetic generator identity\n")
    generator_record = record(generator)
    repeat_log = capsule / f"{prefix}offline-repeat.log"
    repeat_log.write_text("** BUILD SUCCEEDED **\n构建完成：" + str(app) + "\n")
    pre_repeat_artifact = write(capsule / f"{prefix}pre-repeat-artifact-audit.json", {
        "status": "passed", "software_tests": "passed", "source_digest": current_digest,
        "audit_sha256": audit["sha256"], "execution_sha256": execution["sha256"],
        "app": str(app),
    })
    repeat = write(capsule / f"{prefix}offline-repeat.json", {
        "source_digest": current_digest, "app_version": version, "app_build": build,
        "backend_version": backend_version, "passed": True, "offline": True,
        "rustup_auto_install": False, "build_exit_code": 0,
        "command": ["/bin/sh", "scripts/build.sh"],
        "execution_sha256": execution["sha256"],
        "pre_repeat_artifact_audit_sha256": pre_repeat_artifact["sha256"],
        "pre_repeat_artifact_audit": pre_repeat_artifact,
        "post_repeat_artifact_audit_sha256": artifact["sha256"],
        "log_sha256": sha256(repeat_log),
    })
    (root / f"{prefix}b11-offline-repeat-evidence.json").write_bytes(
        (capsule / f"{prefix}offline-repeat.json").read_bytes())
    runtime_root = root / "runtime"
    runtime_root.mkdir()
    profile = runtime_root / "sandbox.sb"
    profile.write_text("(version 1)\n")
    canary_log = runtime_root / "sandbox-canary.log"
    canary_log.write_text("sandbox canary exited\n")
    canary = {"inherited_child": True, "exit_code": 0, "reaped": True,
              "owned_process_group_gone": True, "timeout": False,
              "forced_cleanup": False, "log": str(canary_log),
              "log_sha256": sha256(canary_log)}
    upgrade_processes = []
    for label in ("upgrade-start-0", "upgrade-start-1", "pending-stays-quarantined"):
        process_log = root / "runtime" / f"{label}.log"
        process_log.parent.mkdir(parents=True, exist_ok=True)
        process_log.write_text("synthetic clean child\n")
        upgrade_processes.append({
            "label": label, "exit_code": 0, "sigterm_shutdown_confirmed": True,
            "timeout": False, "forced_cleanup": False,
            "sandbox_profile_sha256": sha256(profile),
            "log": str(process_log), "log_sha256": sha256(process_log),
            "ready_runtime": {"source_digest": current_digest,
                              "app_release_version": version, "build_number": build,
                              "version": backend_version, "backend": "rust",
                              "default_runtime": "rust-native",
                              "hardware_owner": "rust", "hardware_ownership": True,
                              "go_backend_connected": False,
                              "go_backend_phase": "retired"},
        })
    upgrade = write(root / f"{prefix}module-upgrade.json", {
        "schema": 1, "passed": True, "validation_completed": True,
        "synthetic_only": True, "product_acceptance": False,
        "binary": str(backend), "binary_sha256": backend_hash,
        "binary_unchanged": True, "cleanup_errors": [],
        "sandbox_profile": str(profile), "sandbox_profile_sha256": sha256(profile),
        "sandbox_canary": canary,
        "scenarios": [
            {"name": "old_empty_lock_pair_upgrade_and_restart", "passed": True,
             "ordinary_admission_and_same_root_restart": True,
             "original_locks_unchanged": True},
            {"name": "pending_history_not_reclassified", "passed": True,
             "pending_preserved": True}],
        "processes": upgrade_processes,
    })
    daemon = write(root / f"{prefix}daemon-lifetime.json", {
        "passed": True, "validation_completed": True, "binary": str(backend),
        "binary_sha256": backend_hash, "cleanup_errors": [],
        "source_sha256": {"scripts/source-identity.py": sha256(
            SOURCE / "scripts/source-identity.py")},
    })
    handoff = write(root / f"{prefix}forwarding-handoff.json", {
        "passed": True, "validation_completed": True,
        "binary_inputs": {str(backend): backend_hash}, "cleanup_errors": [],
        "source_inputs": {"scripts/source-identity.py": sha256(
            SOURCE / "scripts/source-identity.py")},
    })
    reviewed = {
        "generator": generator_record["sha256"], "cold_audit": audit["sha256"],
        "cold_execution": execution["sha256"], "cold_artifact": artifact["sha256"],
        "cold_log": cold["log"]["sha256"], "offline_repeat": repeat["sha256"],
        "offline_repeat_log": sha256(repeat_log), "module_upgrade": upgrade["sha256"],
        "daemon_lifecycle": daemon["sha256"], "forwarding_handoff": handoff["sha256"],
    }
    reviews = []
    previous_hash = None
    for number in range(1, 4):
        review = write(root / f"{prefix}defect-preclosure-review-{number}.json", {
            "schema_version": 1, "review_id": f"i{iteration}-b01-b12-preclosure-review-{number}",
            "round": number, "reviewer": PRECLOSURE_REVIEWERS[number - 1],
            "scope": "finite-software-defect-preclosure", "source_digest": current_digest,
            "previous_review_sha256": previous_hash,
            "reviewed_evidence_sha256": reviewed, "verdict": "PASS",
            "blocking_software_findings": [], "product_complete": False,
            "functional_acceptance_awarded": False,
            "device_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
            "review_summary": "synthetic independent review",
            "reviewed_at_utc": f"2026-10-22T00:00:0{number}Z",
        })
        reviews.append(review)
        previous_hash = review["sha256"]
    closure = {
        "iteration": iteration, "capsule": relative(capsule),
        "created_at_utc": "2026-10-22T00:00:04+00:00", "cold_evidence": cold,
        "generator": {**generator_record,
            "predecessor_sha256": sha256(root / "iteration-677-prepare-defect-closure.py"),
            "prepared_predecessor_sha256": sha256(root / "iteration-690-prepare-defect-closure.py")},
        "offline_repeat": repeat, "current_module_upgrade": upgrade,
        "current_daemon_lifecycle": daemon, "current_forwarding_handoff": handoff,
        "preclosure_reviews": reviews,
        "reviewed_cli_pins": {
            "validator_sha256": sha256(SOURCE / "scripts/check-no-device-software-acceptance.py"),
            "source_identity_sha256": sha256(SOURCE / "scripts/source-identity.py"),
            **{key + "_sha256": value["sha256"] for key, value in cold.items()},
            "offline_repeat_sha256": repeat["sha256"],
            "offline_repeat_log_sha256": sha256(repeat_log),
            "module_upgrade_final_sha256": upgrade["sha256"],
            "daemon_final_sha256": daemon["sha256"],
            "handoff_final_sha256": handoff["sha256"],
            "preclosure_review_sha256": [item["sha256"] for item in reviews],
        },
    }
    path = root / f"{prefix}defect-closure.json"
    cold_blob = read_regular(cold_log, "synthetic current cold log")
    def check(value):
        verify_defect_closure_chain(value, current_digest, path, cold_blob,
                                    reports_root=root, cold_root_override=capsule.parent,
                                    runtime_root_override=root / "runtime")
    check(closure)
    for name, mutation in (
        ("missing three reviews", lambda row: row.pop("preclosure_reviews")),
        ("missing offline repeat", lambda row: row.pop("offline_repeat")),
        ("wrong final App process", lambda row: row["current_module_upgrade"].update(
            {"sha256": "0" * 64})),
        ("wrong predecessor", lambda row: row["generator"].update(
            {"predecessor_sha256": "0" * 64})),
        ("wrong review order", lambda row: row["preclosure_reviews"].reverse()),
        ("wrong reviewed evidence", lambda row: row["reviewed_cli_pins"].update(
            {"offline_repeat_log_sha256": "0" * 64})),
    ):
        changed = json.loads(json.dumps(closure))
        mutation(changed)
        rejected(lambda: check(changed), name + " was accepted")
    process_log = Path(upgrade_processes[0]["log"])
    original_log = process_log.read_bytes()
    process_log.write_text("changed module child bytes\n")
    rejected(lambda: check(closure), "tampered module child log was accepted")
    process_log.write_bytes(original_log)
    repeat_log_bytes = repeat_log.read_bytes()
    repeat_log.write_text("** BUILD SUCCEEDED **\nwrong App path\n")
    rejected(lambda: check(closure), "tampered repeat log was accepted")
    repeat_log.write_bytes(repeat_log_bytes)
    pre_artifact_path = capsule / f"{prefix}pre-repeat-artifact-audit.json"
    pre_artifact_bytes = pre_artifact_path.read_bytes()
    pre_artifact_path.write_text("changed pre-repeat audit bytes\n")
    rejected(lambda: check(closure), "tampered pre-repeat artifact was accepted")
    pre_artifact_path.write_bytes(pre_artifact_bytes)
    review_path = root / f"{prefix}defect-preclosure-review-1.json"
    review_bytes = review_path.read_bytes()
    wrong_source_review = json.loads(review_bytes)
    wrong_source_review["source_digest"] = "0" * 64
    review_path.write_text(json.dumps(wrong_source_review, sort_keys=True) + "\n")
    changed = json.loads(json.dumps(closure))
    changed["preclosure_reviews"][0]["sha256"] = sha256(review_path)
    rejected(lambda: check(changed), "foreign-source preclosure review was accepted")
    review_path.write_bytes(review_bytes)




def core_records(
    capsule: Path, daemon: Path, forwarding: Path, p6: Path, ledger: Path, defects: Path
) -> dict[str, dict[str, str]]:
    return {
        "clean_build_audit": record(capsule / "audit.json"),
        "clean_build_execution": record(capsule / "execution.json"),
        "clean_build_artifact": record(capsule / "artifact-audit.json"),
        "clean_build_log": record(capsule / "build.log"),
        "daemon_lifecycle": record(daemon),
        "forwarding_handoff_durability": record(forwarding),
        "p6_harness": record(p6),
        "active_ledger": record(ledger),
        "defect_closure": record(defects),
    }


def verify_core(report: dict) -> tuple[dict[str, str], Path, str, datetime.datetime]:
    evidence = report.get("evidence")
    require(isinstance(evidence, dict), "missing evidence records")
    require(set(evidence) == CORE_EVIDENCE_NAMES, "missing or unexpected core evidence")
    blobs = {name: verify_record(evidence[name], name) for name in CORE_EVIDENCE_NAMES}
    hashes = {name: evidence[name]["sha256"] for name in sorted(CORE_EVIDENCE_NAMES)}
    capsule = safe_workspace_path(report.get("capsule", ""), regular=False)
    capsule_names = {
        "clean_build_audit": "audit.json",
        "clean_build_execution": "execution.json",
        "clean_build_artifact": "artifact-audit.json",
        "clean_build_log": "build.log",
    }
    require(all(blobs[name].path == capsule / filename
                for name, filename in capsule_names.items()),
            "clean-build evidence is not the exact named evidence from its capsule")
    reports_root = (BUILD / "reports").resolve()
    require(all(blobs[name].path.is_relative_to(reports_root)
                for name in CORE_EVIDENCE_NAMES - set(capsule_names)),
            "non-capsule software evidence must remain under build/reports")

    audit = load_json_bytes(blobs["clean_build_audit"].data, "clean-build audit")
    execution = load_json_bytes(blobs["clean_build_execution"].data, "clean-build execution")
    artifact = load_json_bytes(blobs["clean_build_artifact"].data, "clean-build artifact audit")
    require(execution.get("status") == "built" and execution.get("software_tests") == "passed",
            "cold capsule did not finish the complete software test/build sequence")
    require(execution.get("audit_sha256") == hashes["clean_build_audit"],
            "cold execution is not bound to its audit")
    commands = execution.get("commands")
    require(isinstance(commands, list), "cold execution command list is missing")
    test_command = ["/bin/sh", "scripts/test-migration-gates.sh"]
    build_command = ["/bin/sh", "scripts/build.sh"]
    require(test_command in commands and build_command in commands
            and commands.index(test_command) < commands.index(build_command),
            "full software gate was not completed before the app build")
    require(artifact.get("status") == "passed" and artifact.get("software_tests") == "passed",
            "cold artifact audit did not require the matching software test run")
    require(artifact.get("audit_sha256") == hashes["clean_build_audit"]
            and artifact.get("execution_sha256") == hashes["clean_build_execution"],
            "cold artifact audit is not bound to its audit/execution")
    require(audit.get("schema") == 1 and isinstance(audit.get("boundaries"), list),
            "cold audit boundary declaration is missing")
    log = blobs["clean_build_log"].data.decode(errors="replace")
    require("migration gates passed" in log, "cold build log has no complete migration-gate success")

    current_digest = source_digest()
    require(report.get("source_digest") == current_digest
            and artifact.get("source_digest") == current_digest,
            "attestation, current source, and cold artifact source identity differ")
    verify_recovery_evidence(log, current_digest, capsule, execution, artifact)
    require(capsule == blobs["clean_build_audit"].path.parent,
            "capsule path does not own the audited evidence")
    app = capsule / "dist/Switch.app"
    require(app.is_dir() and not app.is_symlink(), "cold Switch.app is missing or unsafe")
    require(Path(artifact.get("app", "")).resolve() == app.resolve(),
            "artifact audit references a different app")
    version, build, backend_version = release_identity()
    require(report.get("app_version") == version and report.get("build_number") == build
            and report.get("backend_version") == backend_version,
            "release identity changed after the attestation")
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    require(info.get("CFBundleIdentifier") == "com.proletarius.switch"
            and info.get("CFBundleShortVersionString") == version
            and info.get("CFBundleVersion") == build,
            "cold app bundle identity differs")
    expected_inventory = report.get("app_bundle")
    require(isinstance(expected_inventory, dict), "attestation App bundle inventory is missing")
    require(artifact.get("app_bundle") == expected_inventory,
            "attestation App tree was not bound by the reviewed artifact audit")
    backend, backend_hash = verify_current_bundle(app, artifact, expected_inventory)

    daemon = load_json_bytes(blobs["daemon_lifecycle"].data, "daemon lifecycle")
    verify_daemon_report(daemon, backend_hash, current_digest=current_digest)

    forwarding = load_json_bytes(blobs["forwarding_handoff_durability"].data,
                                 "forwarding handoff durability")
    verify_forwarding_report(forwarding, backend, backend_hash,
                             current_digest=current_digest)
    p6 = load_json_bytes(blobs["p6_harness"].data, "P6 harness")
    verify_p6_report(p6)
    ledger = load_json_bytes(blobs["active_ledger"].data, "active ledger check")
    fresh_ledger, fresh_ledger_bytes = current_active_ledger_report()
    verify_active_ledger_snapshot(ledger, fresh_ledger, current_digest)
    verify_active_ledger_bytes(blobs["active_ledger"].data, fresh_ledger_bytes)
    closure = load_json_bytes(blobs["defect_closure"].data, "B01-B12 software closure")
    require(closure.get("current_daemon_lifecycle") == report["evidence"]["daemon_lifecycle"]
            and closure.get("current_forwarding_handoff")
                == report["evidence"]["forwarding_handoff_durability"],
            "closure process proofs differ from final attestation process proofs")
    verify_defect_closure(
        closure,
        current_digest,
        blobs["defect_closure"].path,
        blobs["clean_build_log"],
    )
    created = datetime.datetime.fromisoformat(
        closure["created_at_utc"].replace("Z", "+00:00"))
    return hashes, app, backend_hash, created


def verify_reviews(report: dict, core_hashes: dict[str, str],
                   *, closure_created_at: datetime.datetime | None = None) -> None:
    items = report.get("reviews")
    require(isinstance(items, list) and len(items) == 3, "exactly three review records are required")
    reviewers: set[str] = set()
    review_hashes: set[str] = set()
    previous_hash = None
    previous_time = None
    for expected_round, item in enumerate(items, 1):
        blob = verify_record(item, f"review {expected_round}")
        require(blob.path.is_relative_to((BUILD / "reports").resolve()),
                f"review {expected_round} must remain under build/reports")
        require(blob.sha256 not in review_hashes, "review files must have unique hashes")
        review_hashes.add(blob.sha256)
        review = load_json_bytes(blob.data, f"review {expected_round}")
        reviewer = review.get("reviewer")
        require(review.get("schema_version") == 1 and review.get("round") == expected_round,
                f"review {expected_round} has the wrong schema or order")
        require(review.get("scope") == SCOPE and review.get("source_digest") == report["source_digest"],
                f"review {expected_round} is for a different scope or source")
        require(review.get("review_id") == f"no-device-software-release-round-{expected_round}",
                f"review {expected_round} has the wrong controlled review ID")
        require(reviewer == REVIEWERS[expected_round - 1] and reviewer not in reviewers,
                f"review {expected_round} has the wrong controlled reviewer ID")
        reviewers.add(reviewer)
        require(review.get("previous_review_sha256") == previous_hash,
                f"review {expected_round} does not extend the exact prior review hash")
        timestamp = review.get("reviewed_at_utc")
        require(isinstance(timestamp, str) and timestamp.endswith("Z"),
                f"review {expected_round} lacks a UTC review timestamp")
        try:
            reviewed_at = datetime.datetime.fromisoformat(timestamp[:-1] + "+00:00")
        except ValueError as error:
            raise ValueError(f"review {expected_round} has an invalid UTC timestamp") from error
        require((previous_time is None or reviewed_at > previous_time)
                and (closure_created_at is None or reviewed_at > closure_created_at),
                f"review {expected_round} is not later than the prior review")
        previous_time = reviewed_at
        require(isinstance(review.get("review_summary"), str)
                and review["review_summary"].strip(),
                f"review {expected_round} has no independent review summary")
        require(review.get("verdict") == "PASS"
                and review.get("software_release_recommendation") == "PASS"
                and review.get("blocking_software_findings") == [],
                f"review {expected_round} did not pass")
        require(review.get("product_complete") is False
                and review.get("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER",
                f"review {expected_round} overclaims product/device acceptance")
        require(review.get("reviewed_evidence_sha256") == core_hashes,
                f"review {expected_round} did not review the exact core evidence")
        previous_hash = blob.sha256


def verify_report(path: Path) -> dict:
    attestation = read_regular(path, "software acceptance attestation")
    report = load_json_bytes(attestation.data, "software acceptance attestation")
    require(report.get("schema_version") == SCHEMA_VERSION and report.get("scope") == SCOPE,
            "wrong software acceptance schema or scope")
    require(report.get("software_acceptance_complete") is True
            and report.get("product_acceptance_complete") is False
            and report.get("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER",
            "software acceptance status is missing or overclaims product acceptance")
    require(report.get("exclusions") == EXCLUSIONS, "software acceptance exclusions changed")
    core_hashes, app, _, closure_created_at = verify_core(report)
    verify_reviews(report, core_hashes, closure_created_at=closure_created_at)
    evidence = report["evidence"]
    for name in CORE_EVIDENCE_NAMES:
        verify_record(evidence[name], f"final {name}")
    for index, item in enumerate(report["reviews"], 1):
        verify_record(item, f"final review {index}")
    require(bundle_inventory_identity(bundle_inventory(app))
            == bundle_inventory_identity(report["app_bundle"]),
            "cold App bundle changed before attestation verification completed")
    require(read_regular(path, "final software acceptance attestation").sha256
            == attestation.sha256,
            "software acceptance attestation changed during verification")
    return report


def verified_app(report_path: Path) -> Path:
    report = verify_report(report_path)
    return safe_workspace_path(report["capsule"], regular=False) / "dist/Switch.app"


def verified_bindings(report_path: Path) -> tuple[str, str, str]:
    """Return exact App-tree, main-executable and Rust-daemon digests."""
    report = verify_report(report_path)
    inventory = report["app_bundle"]
    hashes_by_path = {
        row.get("path"): row.get("sha256") for row in inventory["files"]
        if isinstance(row, dict)
    }
    tree_hash = inventory.get("tree_sha256")
    main_hash = hashes_by_path.get("Contents/MacOS/Switch")
    backend_hash = hashes_by_path.get("Contents/Resources/backend/switchd-rs")
    require(re.fullmatch(r"[0-9a-f]{64}", tree_hash or "") is not None
            and re.fullmatch(r"[0-9a-f]{64}", main_hash or "") is not None
            and re.fullmatch(r"[0-9a-f]{64}", backend_hash or "") is not None,
            "attestation App, main executable, or backend binding is malformed")
    return tree_hash, main_hash, backend_hash


def verify_app_copy(report_path: Path, candidate: Path) -> None:
    report = verify_report(report_path)
    verify_app_tree(report, candidate)


def verify_app_tree(report: dict, candidate: Path) -> None:
    """Verify one App tree against an already verified attestation report."""
    expected = report["app_bundle"]
    require(not candidate.is_symlink(), "release App copy must not be a symlink")
    actual = bundle_inventory(candidate.resolve(strict=True), workspace_bound=False)
    require(bundle_inventory_identity(actual) == bundle_inventory_identity(expected),
            "release App copy differs from the attested cold App")
    subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(candidate)],
                   check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    require(bundle_inventory(candidate.resolve(strict=True), workspace_bound=False) == actual,
            "release App copy changed during signature verification")


def dmg_argument(value: str) -> Path:
    path = Path(value)
    require(not path.is_symlink(), "release DMG must not be a symlink")
    resolved = path.resolve(strict=True)
    require(resolved.is_relative_to(WORKSPACE) and resolved.suffix.lower() == ".dmg",
            "release DMG must remain inside the workspace and use the .dmg suffix")
    snapshot_regular(resolved, "release DMG")
    return resolved


def mounted_entity(attach_plist: bytes, expected_mountpoint: Path) -> str:
    """Validate hdiutil's exact private mount and return its device node."""
    value = plistlib.loads(attach_plist)
    require(isinstance(value, dict), "hdiutil attach plist is not a dictionary")
    entities = value.get("system-entities")
    require(isinstance(entities, list), "hdiutil attach plist has no system entities")
    mounted = [item for item in entities
               if isinstance(item, dict) and isinstance(item.get("mount-point"), str)]
    require(len(mounted) == 1, "release DMG must expose exactly one mounted volume")
    declared = Path(mounted[0]["mount-point"])
    require(declared.resolve(strict=True) == expected_mountpoint.resolve(strict=True),
            "hdiutil mounted the release DMG at an unexpected location")
    device = mounted[0].get("dev-entry")
    require(isinstance(device, str)
            and re.fullmatch(r"/dev/disk[0-9]+(?:s[0-9]+)?", device),
            "mounted release DMG has no unambiguous device node")
    return device


def mounted_switch_app(mountpoint: Path) -> Path:
    require(mountpoint.is_dir() and not mountpoint.is_symlink(),
            "release DMG mountpoint is missing or unsafe")
    require({item.name for item in mountpoint.iterdir()} == {"Switch.app", "Applications"},
            "release DMG must contain only Switch.app and the Applications link")
    link = mountpoint / "Applications"
    require(link.is_symlink() and os.readlink(link) == "/Applications",
            "release DMG Applications link is missing or unsafe")
    app = mountpoint / "Switch.app"
    require(app.is_dir() and not app.is_symlink(),
            "release DMG Switch.app is missing or unsafe")
    return app


def verify_dmg_app(report_path: Path, dmg: Path) -> None:
    """Mount a DMG read-only and bind its complete Switch.app tree to evidence."""
    report = verify_report(report_path)
    before = snapshot_regular(dmg, "release DMG")
    BUILD.mkdir(parents=True, exist_ok=True)
    primary_error = None
    cleanup_error = None
    with tempfile.TemporaryDirectory(prefix=".dmg-app-verification-", dir=BUILD) as value:
        mountpoint = Path(value)
        attached = False
        device = None
        try:
            process = subprocess.run(
                ["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse",
                 "-noautoopen", "-mountpoint", str(mountpoint), "-plist", str(dmg)],
                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            attached = True
            device = mounted_entity(process.stdout, mountpoint)
            verify_app_tree(report, mounted_switch_app(mountpoint))
            # Mounted payload must also pass current metadata/layout/resource
            # policy; the tree attestation alone does not cover xattrs/forks.
            subprocess.run([sys.executable, str(SOURCE / "scripts/audit-release-bundle.py"),
                            str(mountpoint / "Switch.app"), "--stage-root", str(mountpoint)],
                           check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           timeout=30)
        except BaseException as error:  # preserve verification failure until cleanup completes
            primary_error = error
        finally:
            if attached or os.path.ismount(mountpoint):
                detach = subprocess.run(
                    ["/usr/bin/hdiutil", "detach", str(mountpoint)],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                )
                if detach.returncode != 0 and device is not None:
                    detach = subprocess.run(
                        ["/usr/bin/hdiutil", "detach", device],
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    )
                if detach.returncode != 0:
                    cleanup_error = ValueError(
                        "release DMG verification could not confirm read-only mount cleanup")
    after = snapshot_regular(dmg, "release DMG after mounted verification")
    if cleanup_error is not None:
        raise cleanup_error
    if primary_error is not None:
        raise primary_error
    require(after == before, "release DMG changed during mounted App verification")


def create_report(args: argparse.Namespace) -> Path:
    capsule = Path(args.capsule).resolve(strict=True)
    require(capsule.is_dir() and not capsule.is_symlink() and BUILD in capsule.parents,
            "capsule must be a real directory under the workspace build tree")
    daemon = Path(args.daemon_lifecycle).resolve(strict=True)
    forwarding = Path(args.forwarding_handoff_durability).resolve(strict=True)
    p6 = Path(args.p6_harness).resolve(strict=True)
    ledger = Path(args.active_ledger).resolve(strict=True)
    defects = Path(args.defect_closure).resolve(strict=True)
    reviews = [Path(value).resolve(strict=True) for value in args.review]
    require(len(reviews) == 3, "exactly three --review files are required")
    version, build, backend = release_identity()
    artifact = load_json(capsule / "artifact-audit.json", "clean-build artifact audit")
    audited_bundle = artifact.get("app_bundle")
    require(isinstance(audited_bundle, dict),
            "clean-build artifact audit has no complete App bundle inventory")
    report = {
        "schema_version": SCHEMA_VERSION,
        "scope": SCOPE,
        "source_digest": source_digest(),
        "app_version": version,
        "build_number": build,
        "backend_version": backend,
        "software_acceptance_complete": True,
        "product_acceptance_complete": False,
        "device_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
        "exclusions": EXCLUSIONS,
        "capsule": relative(capsule),
        "app_bundle": audited_bundle,
        "evidence": core_records(capsule, daemon, forwarding, p6, ledger, defects),
        "reviews": [record(path) for path in reviews],
    }
    output = Path(args.output).resolve()
    require(output == WORKSPACE or WORKSPACE in output.parents,
            "attestation output must remain inside the workspace")
    require(not output.exists() and not output.is_symlink(),
            "refusing to overwrite an existing attestation")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", dir=output.parent, delete=False,
                                     prefix=".software-acceptance.", suffix=".json") as stream:
        temporary = Path(stream.name)
        json.dump(report, stream, ensure_ascii=False, indent=2, sort_keys=True)
        stream.write("\n")
    try:
        verify_report(temporary)
        os.replace(temporary, output)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    return output


def self_test() -> None:
    require(len(EXCLUSIONS) == 6 and len(set(EXCLUSIONS)) == len(EXCLUSIONS),
            "software acceptance exclusions must remain explicit and unique")
    reports = BUILD / "reports"
    reports.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="software-acceptance-self-test-", dir=reports) as value:
        root = Path(value)
        def rejected(action, message):
            try:
                action()
            except ValueError:
                return
            raise AssertionError(message)

        sample = root / "sample.json"
        sample.write_text("{}\n")
        item = record(sample)
        require(verify_record(item, "sample").path == sample.resolve(), "valid record rejected")
        item["sha256"] = "0" * 64
        try:
            verify_record(item, "sample")
        except ValueError:
            pass
        else:
            raise AssertionError("changed digest accepted")
        link = root / "link"
        link.symlink_to(sample)
        try:
            safe_workspace_path(relative(link))
        except ValueError:
            pass
        else:
            raise AssertionError("evidence symlink accepted")
        rejected(lambda: attestation_argument(str(link)),
                 "attestation symlink accepted")
        hardlink = root / "hardlink"
        os.link(sample, hardlink)
        try:
            record(sample)
        except ValueError:
            pass
        else:
            raise AssertionError("multi-link evidence accepted")
        hardlink.unlink()
        item = record(sample)
        require(verify_record(item, "sample").sha256 == item["sha256"],
                "single-open evidence verification failed")
        image = root / "sample.dmg"
        image.write_bytes(b"fixture image")
        image_snapshot = snapshot_regular(image, "fixture DMG")
        require(image_snapshot.sha256 == sha256_bytes(b"fixture image"),
                "streamed DMG snapshot has the wrong digest")
        image.write_bytes(b"changed fixture image")
        require(snapshot_regular(image, "changed fixture DMG") != image_snapshot,
                "DMG replacement was not detected")

        mount_fixture = root / "mount"
        mount_fixture.mkdir()
        plist = plistlib.dumps({"system-entities": [
            {"dev-entry": "/dev/disk99"},
            {"dev-entry": "/dev/disk99s1", "mount-point": str(mount_fixture)},
        ]})
        require(mounted_entity(plist, mount_fixture) == "/dev/disk99s1",
                "valid hdiutil attach plist was rejected")
        ambiguous = plistlib.dumps({"system-entities": [
            {"dev-entry": "/dev/disk99s1", "mount-point": str(mount_fixture)},
            {"dev-entry": "/dev/disk99s2", "mount-point": str(mount_fixture)},
        ]})
        rejected(lambda: mounted_entity(ambiguous, mount_fixture),
                 "ambiguous hdiutil mount plist was accepted")
        mounted_app = mount_fixture / "Switch.app"
        mounted_app.mkdir()
        (mount_fixture / "Applications").symlink_to("/Applications")
        require(mounted_switch_app(mount_fixture) == mounted_app,
                "valid mounted Switch.app was rejected")
        other_app = mount_fixture / "Other.app"
        other_app.mkdir()
        rejected(lambda: mounted_switch_app(mount_fixture),
                 "multiple top-level Apps were accepted")

        app_fixture = root / "Inventory.app"
        resource = app_fixture / "Contents/Resources/example.json"
        resource.parent.mkdir(parents=True)
        resource.write_text('{"value":1}\n')
        inventory = bundle_inventory(app_fixture)
        require(inventory["file_count"] == 1,
                "complete App inventory omitted a resource")
        require(inventory["directory_count"] == 3,
                "complete App inventory omitted a directory")
        resource.write_text('{"value":2}\n')
        require(bundle_inventory(app_fixture) != inventory,
                "App resource mutation did not change the tree inventory")
        resource.write_text('{"value":1}\n')
        resource.chmod(0o700)
        require(bundle_inventory(app_fixture) != inventory,
                "App resource mode mutation did not change the tree inventory")
        resource.chmod(0o644)
        empty = app_fixture / "Contents/Empty"
        empty.mkdir()
        require(bundle_inventory(app_fixture) != inventory,
                "App empty directory did not change the tree inventory")

        suite_a = root / "p6-a.log"
        suite_b = root / "p6-b.log"
        suite_a.write_text("PASS A\n")
        suite_b.write_text("PASS B\n")
        p6 = {
            "schema_version": 1,
            "syntax_and_whitespace_passed": True,
            "full_p6_acceptance": False,
            "device_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
            "legacy_3_42_executed": False,
            "functional_acceptance_awarded": False,
            "source_sha256": {
                name: sha256(WORKSPACE / name) for name in REQUIRED_P6_SOURCES
            },
            "targeted_suites": [record(suite_a), record(suite_b)],
            "suite_counts": {"a": 1, "b": 1, "total": 2},
        }
        verify_p6_report(p6)
        bad_p6 = json.loads(json.dumps(p6))
        bad_p6["targeted_suites"][0]["sha256"] = "0" * 64
        rejected(lambda: verify_p6_report(bad_p6), "changed nested P6 suite hash accepted")

        current_digest = source_digest()
        active_ledger = {
            "source_digest": current_digest,
            "document_sha256": {"metadata.json": "1" * 64},
            "metadata_consistent": True,
            "gate_satisfied": True,
            "overall_denominator_frozen": False,
            "functional_acceptance_awarded": False,
            "overall_product_complete": False,
        }
        verify_active_ledger_snapshot(active_ledger, dict(active_ledger), current_digest)
        ledger_bytes = json.dumps(active_ledger, ensure_ascii=False, indent=2).encode() + b"\n"
        verify_active_ledger_bytes(ledger_bytes, ledger_bytes)
        rejected(lambda: verify_active_ledger_bytes(
            ledger_bytes, json.dumps(active_ledger, sort_keys=True).encode() + b"\n"),
            "non-byte-equivalent active ledger was accepted")
        stale_ledger = dict(active_ledger)
        stale_ledger["source_digest"] = "0" * 64
        rejected(lambda: verify_active_ledger_snapshot(
            stale_ledger, active_ledger, current_digest),
            "stale active-ledger source digest accepted")
        changed_ledger = json.loads(json.dumps(active_ledger))
        changed_ledger["document_sha256"]["metadata.json"] = "2" * 64
        rejected(lambda: verify_active_ledger_snapshot(
            active_ledger, changed_ledger, current_digest),
            "active-ledger input hash drift accepted")
        capsules = BUILD / "clean-build"
        capsules.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="software-acceptance-cold-self-test-", dir=capsules
        ) as cold_value:
            cold_root = Path(cold_value)
            cold_path = cold_root / "build.log"
            cold_path.write_text("PASS A\n")
            cold_test_log = read_regular(cold_path, "cold-gate fixture")
            closure = {
                "schema_version": 1,
                "scope": SCOPE,
                "source_digest": current_digest,
                "defects": [
                    {"id": f"B{index:02d}", "status": "SOFTWARE_CLOSED",
                     "evidence": [
                         {"category": "integration_test", **record(cold_path)},
                         {"category": "source_review", **record(sample)},
                     ]}
                    for index in range(1, 13)
                ],
                "software_defects_closed": 12,
                "product_acceptance_complete": False,
                "device_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
            }
            verify_defect_closure_rows(
                closure, current_digest, root / "closure.json", cold_test_log)

            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][6]["evidence"] = []
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "evidence-free defect closure accepted")

            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][0]["category"] = "uncontrolled_claim"
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "uncontrolled defect evidence category accepted")

            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][0]["category"] = "source_review"
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "current cold build.log with the wrong category was accepted")

            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][0]["sha256"] = "0" * 64
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "changed per-defect evidence hash accepted")

            spoofed_cold_path = cold_root / "spoofed-build.log"
            spoofed_cold_path.write_bytes(cold_test_log.data)
            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][0] = {
                "category": "integration_test", **record(spoofed_cold_path),
            }
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "same-hash clean-build path spoof was accepted")

            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][0] = {
                "category": "integration_test", **record(suite_a),
            }
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "report-local same-hash cold-gate path spoof was accepted")

            cold_path.write_text("CHANGED AFTER CORE VERIFICATION\n")
            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][0] = {
                "category": "integration_test", **record(cold_path),
            }
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "changed current cold build.log was accepted")
            cold_path.write_bytes(cold_test_log.data)

            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"] = [
                {"category": "integration_test", **record(cold_path)}
            ]
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "defect closure without separate support was accepted")

            external_support = cold_root / "support.log"
            external_support.write_text("external support\n")
            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][1] = {
                "category": "source_review", **record(external_support),
            }
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "support outside build/reports was accepted")

            empty_support = root / "empty-support.log"
            empty_support.write_bytes(b"")
            bad_closure = json.loads(json.dumps(closure))
            bad_closure["defects"][0]["evidence"][1] = {
                "category": "source_review", **record(empty_support),
            }
            rejected(lambda: verify_defect_closure_rows(
                bad_closure, current_digest, root / "closure.json", cold_test_log),
                "empty support report was accepted")

            chain_capsule = cold_root / f"i691-current-{current_digest[:8]}"
            chain_capsule.mkdir()
            self_test_defect_closure_chain(root, chain_capsule, current_digest, rejected)

        backend = root / "switchd-rs"
        swift_binary = root / "swift-forwarding-fixture"
        rust_binary = root / "rust-forwarding-dto-fixture"
        backend.write_bytes(b"synthetic executable identity")
        swift_binary.write_bytes(b"\xcf\xfa\xed\xfe synthetic Swift executable identity")
        rust_binary.write_bytes(b"\xcf\xfa\xed\xfe synthetic Rust test executable identity")
        swift_binary.chmod(0o700)
        rust_binary.chmod(0o700)
        runtime_root = root / "process-evidence"
        runtime_root.mkdir()
        profile = runtime_root / "hardware-denied.sb"
        profile.write_text("(version 1)\n")
        profile_hash = sha256(profile)
        canary_log = runtime_root / "canary.log"
        canary_log.write_text("canary exited cleanly\n")
        canary = {"inherited_child": True, "exit_code": 0, "reaped": True,
                  "owned_process_group_gone": True, "forced_cleanup": False,
                  "timeout": False, "log": str(canary_log),
                  "log_sha256": sha256(canary_log)}
        fixture_version, fixture_build, fixture_backend_version = release_identity()
        ready_runtime = {
            "source_digest": current_digest,
            "app_release_version": fixture_version,
            "build_number": fixture_build,
            "version": fixture_backend_version,
            "backend": "rust", "default_runtime": "rust-native",
            "hardware_owner": "rust", "hardware_ownership": True,
            "go_backend_connected": False, "go_backend_phase": "retired",
        }
        daemon_processes = []
        all_labels = (DAEMON_SUCCESSES | set(DAEMON_REJECTIONS) |
                      {"race-0", "race-1", "stale-race-0", "stale-race-1"})
        for label in sorted(all_labels):
            rejection = DAEMON_REJECTIONS.get(label)
            if label == "race-1":
                rejection = "Switch data directory is already owned by another daemon"
            if label == "stale-race-1":
                rejection = "backend_socket_in_use"
            process_log = runtime_root / f"daemon-{label}.log"
            process_log.write_text((rejection or "normal SIGTERM exit") + "\n")
            daemon_processes.append({
                "label": label, "exit_code": 1 if rejection else 0,
                "expected_rejection": rejection,
                "sigterm_shutdown_confirmed": rejection is None,
                "ready_runtime": ready_runtime if rejection is None else None,
                "forced_cleanup": False, "timeout": False,
                "sandbox_profile_sha256": profile_hash,
                "log": str(process_log), "log_sha256": sha256(process_log),
            })
        daemon = {
            "passed": True,
            "validation_completed": True,
            "synthetic_only": True,
            "full_p6_acceptance": False,
            "scope": DAEMON_SCOPE,
            "binary_sha256": sha256(backend),
            "scenarios": [
                {"name": name, "passed": True,
                 "processes": [next(row for row in daemon_processes
                                    if row["label"] == label)
                               for label in DAEMON_SCENARIO_PROCESSES[name]]}
                for name in sorted(DAEMON_SCENARIOS)
            ],
            "sandbox_profile": str(profile),
            "sandbox_profile_sha256": profile_hash,
            "sandbox_canary": canary,
            "processes": daemon_processes,
            "cleanup_errors": [],
            "interface_ownership": {"implemented": 300, "total": 300, "go": 0},
        }
        verify_daemon_report(daemon, sha256(backend), runtime_root=runtime_root)
        bad_daemon = json.loads(json.dumps(daemon))
        bad_daemon["scenarios"][1] = dict(bad_daemon["scenarios"][0])
        rejected(lambda: verify_daemon_report(bad_daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "duplicated daemon scenario accepted")
        bad_daemon = json.loads(json.dumps(daemon))
        next(row for row in bad_daemon["scenarios"] if row["processes"])[
            "processes"] = []
        rejected(lambda: verify_daemon_report(bad_daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "daemon scenario with missing child receipts accepted")
        bad_daemon = json.loads(json.dumps(daemon))
        bad_daemon["sandbox_canary"]["exit_code"] = 1
        rejected(lambda: verify_daemon_report(bad_daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "failed daemon sandbox canary accepted")
        bad_daemon = json.loads(json.dumps(daemon))
        bad_daemon["processes"][0]["exit_code"] = None
        rejected(lambda: verify_daemon_report(bad_daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "unreaped daemon process accepted")
        bad_daemon = json.loads(json.dumps(daemon))
        for row in bad_daemon["processes"]:
            row["exit_code"] = 0
        rejected(lambda: verify_daemon_report(bad_daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "daemon expected refusals were accepted as successful exits")
        bad_daemon = json.loads(json.dumps(daemon))
        next(row for row in bad_daemon["processes"] if row["exit_code"] == 0)[
            "ready_runtime"]["source_digest"] = "0" * 64
        rejected(lambda: verify_daemon_report(bad_daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "daemon child from another source was accepted")
        bad_daemon = json.loads(json.dumps(daemon))
        bad_daemon["processes"][0]["log"] = str(runtime_root / "missing.log")
        rejected(lambda: verify_daemon_report(bad_daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "missing daemon child log was accepted")
        original = canary_log.read_bytes()
        canary_log.write_text("tampered canary\n")
        rejected(lambda: verify_daemon_report(daemon, sha256(backend),
                                              runtime_root=runtime_root),
                 "tampered daemon canary log was accepted")
        canary_log.write_bytes(original)
        handoff_processes = []
        for label in sorted(HANDOFF_PROCESS_LABELS):
            is_daemon = label.startswith("handoff-daemon-")
            program = rust_binary if label == "rust-exact-dto" else swift_binary
            process_log = runtime_root / f"handoff-{label}.log"
            process_log.write_text("successful handoff child\n")
            handoff_processes.append({
                "label": label, "kind": None if is_daemon else "compiled_fixture",
                "program": None if is_daemon else str(program),
                "program_sha256": None if is_daemon else sha256(program),
                "arguments": (["--exact", FORWARDING_EXACT_RUST_TEST,
                               "--ignored", "--nocapture"] if label == "rust-exact-dto" else []),
                "exit_code": 0, "reaped": True, "owned_process_group_gone": True,
                "sigterm_shutdown_confirmed": is_daemon,
                "ready_runtime": ready_runtime if is_daemon else None,
                "forced_cleanup": False, "timeout": False,
                "sandbox_profile_sha256": profile_hash,
                "log": str(process_log), "log_sha256": sha256(process_log),
            })
        forwarding = {
            "schema_version": 1,
            "passed": True,
            "validation_completed": True,
            "purpose": "actual production-file Swift restart, exact Rust DTO and real daemon durable handoff",
            "scope": "synthetic checkpoints and authenticated Unix HTTP only; no provider, SMS, hardware or carrier acceptance",
            "rust_dto_requested": True,
            "full_cross_language_dto_roundtrip_passed": True,
            "listed_inputs_zero_drift": True,
            "all_processes_reaped": True,
            "no_forced_cleanup": True,
            "cleanup_errors": [],
            "scenarios": [
                {"name": "actual_swift_production_file_survives_process_exit_and_restart",
                 "passed": True, "independent_process_reconstruction_equal": True},
                {"name": "exact_compiled_rust_dto_then_independent_swift_production_file_verify",
                 "passed": True, "true_swift_rust_swift_roundtrip": True,
                 "exact_required_rust_test": FORWARDING_EXACT_RUST_TEST},
                {"name": "fresh_real_daemon_swift_file_handoff_restart_replay_conflict_and_real_shutdown",
                 "passed": True, "first_real_sigterm_exit_zero": True,
                 "restarted_real_sigterm_exit_zero": True,
                 "new_daemon_and_swift_process_read_original_file": True,
                 "provider_configured_or_message_submitted": False,
                 "live_checkpoint_directory_is_daemon_root": True},
            ],
            "processes": handoff_processes,
            "sandbox_profile": str(profile),
            "sandbox_profile_sha256": profile_hash,
            "sandbox_canary": canary,
            "binary_inputs": {
                str(backend): sha256(backend),
                str(swift_binary): sha256(swift_binary),
                str(rust_binary): sha256(rust_binary),
            },
            "source_inputs": {
                name: sha256(SOURCE / name) for name in forwarding_source_paths()
            },
        }
        verify_forwarding_report(forwarding, backend, sha256(backend),
                                 runtime_root=runtime_root)
        bad_forwarding = json.loads(json.dumps(forwarding))
        bad_forwarding["full_cross_language_dto_roundtrip_passed"] = False
        rejected(lambda: verify_forwarding_report(
            bad_forwarding, backend, sha256(backend), runtime_root=runtime_root),
            "forwarding report without full DTO roundtrip accepted")
        bad_forwarding = json.loads(json.dumps(forwarding))
        bad_forwarding["processes"].append("not a process receipt")
        rejected(lambda: verify_forwarding_report(
            bad_forwarding, backend, sha256(backend), runtime_root=runtime_root),
            "malformed forwarding process receipt accepted")
        bad_forwarding = json.loads(json.dumps(forwarding))
        for row in bad_forwarding["processes"]:
            row["exit_code"] = 1
        rejected(lambda: verify_forwarding_report(
            bad_forwarding, backend, sha256(backend), runtime_root=runtime_root),
            "failed forwarding child exits were accepted")
        bad_forwarding = json.loads(json.dumps(forwarding))
        next(row for row in bad_forwarding["processes"]
             if row["label"] == "swift-produce")["program_sha256"] = "0" * 64
        rejected(lambda: verify_forwarding_report(
            bad_forwarding, backend, sha256(backend), runtime_root=runtime_root),
            "forwarding child with a changed executable hash was accepted")
        bad_forwarding = json.loads(json.dumps(forwarding))
        next(row for row in bad_forwarding["processes"]
             if row["label"] == "handoff-daemon-first")["ready_runtime"]["backend"] = "go"
        rejected(lambda: verify_forwarding_report(
            bad_forwarding, backend, sha256(backend), runtime_root=runtime_root),
            "forwarding daemon child with Go runtime was accepted")
        bad_forwarding = json.loads(json.dumps(forwarding))
        bad_forwarding["processes"][0]["log"] = str(runtime_root / "missing.log")
        rejected(lambda: verify_forwarding_report(
            bad_forwarding, backend, sha256(backend), runtime_root=runtime_root),
            "missing forwarding child log was accepted")
        original = canary_log.read_bytes()
        canary_log.write_text("tampered forwarding canary\n")
        rejected(lambda: verify_forwarding_report(
            forwarding, backend, sha256(backend), runtime_root=runtime_root),
            "tampered forwarding canary log was accepted")
        canary_log.write_bytes(original)

        core_hashes = {name: hashlib.sha256(name.encode()).hexdigest()
                       for name in sorted(CORE_EVIDENCE_NAMES)}
        review_records = []
        previous_hash = None
        for round_number, reviewer in enumerate(REVIEWERS, 1):
            path = root / f"review-{round_number}.json"
            path.write_text(json.dumps({
                "schema_version": 1,
                "round": round_number,
                "review_id": f"no-device-software-release-round-{round_number}",
                "reviewer": reviewer,
                "previous_review_sha256": previous_hash,
                "reviewed_at_utc": f"2026-10-22T00:00:0{round_number}Z",
                "review_summary": f"independent review {round_number}",
                "scope": SCOPE,
                "source_digest": current_digest,
                "verdict": "PASS",
                "software_release_recommendation": "PASS",
                "blocking_software_findings": [],
                "product_complete": False,
                "device_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
                "reviewed_evidence_sha256": core_hashes,
            }, sort_keys=True) + "\n")
            item = record(path)
            review_records.append(item)
            previous_hash = item["sha256"]
        review_report = {"source_digest": current_digest, "reviews": review_records}
        verify_reviews(review_report, core_hashes)
        rejected(lambda: verify_reviews(
            review_report, core_hashes,
            closure_created_at=datetime.datetime.fromisoformat(
                "2026-10-22T00:00:01+00:00")),
            "release review before closure was accepted")
        review_2_path = root / "review-2.json"
        review_2_bytes = review_2_path.read_bytes()
        changed = load_json(root / "review-2.json", "review fixture")
        changed["reviewer"] = "free-form-reviewer"
        review_2_path.write_text(json.dumps(changed, sort_keys=True) + "\n")
        changed_record = record(review_2_path)
        original_record = review_records[1]
        review_records[1] = changed_record
        rejected(lambda: verify_reviews(review_report, core_hashes),
                 "uncontrolled reviewer identity accepted")
        review_2_path.write_bytes(review_2_bytes)
        review_records[1] = original_record
        changed = load_json(root / "review-3.json", "review fixture")
        changed["previous_review_sha256"] = "0" * 64
        (root / "review-3.json").write_text(json.dumps(changed, sort_keys=True) + "\n")
        review_records[2] = record(root / "review-3.json")
        rejected(lambda: verify_reviews(review_report, core_hashes),
                 "review chain discontinuity accepted")
    print("no-device software acceptance self-test passed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    create = sub.add_parser("create")
    create.add_argument("--capsule", required=True)
    create.add_argument("--daemon-lifecycle", required=True)
    create.add_argument("--forwarding-handoff-durability", required=True)
    create.add_argument("--p6-harness", required=True)
    create.add_argument("--active-ledger", required=True)
    create.add_argument("--defect-closure", required=True)
    create.add_argument("--review", action="append", default=[])
    create.add_argument("--output", required=True)
    verify = sub.add_parser("verify")
    verify.add_argument("report")
    app_path = sub.add_parser("verified-app")
    app_path.add_argument("report")
    bindings = sub.add_parser("verified-bindings")
    bindings.add_argument("report")
    app_copy = sub.add_parser("verify-app-copy")
    app_copy.add_argument("report")
    app_copy.add_argument("app")
    dmg_app = sub.add_parser("verify-dmg-app")
    dmg_app.add_argument("report")
    dmg_app.add_argument("dmg")
    sub.add_parser("self-test")
    args = parser.parse_args()
    if args.action == "create":
        print(create_report(args))
    elif args.action == "verify":
        verify_report(attestation_argument(args.report))
        print("no-device software acceptance: PASS")
    elif args.action == "verified-app":
        print(verified_app(attestation_argument(args.report)))
    elif args.action == "verified-bindings":
        print(*verified_bindings(attestation_argument(args.report)))
    elif args.action == "verify-app-copy":
        verify_app_copy(attestation_argument(args.report), Path(args.app))
        print("attested App copy: PASS")
    elif args.action == "verify-dmg-app":
        verify_dmg_app(attestation_argument(args.report), dmg_argument(args.dmg))
        print("attested DMG App: PASS")
    else:
        self_test()
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, subprocess.CalledProcessError, plistlib.InvalidFileException) as error:
        print(f"no-device software acceptance failed: {error}", file=sys.stderr)
        raise SystemExit(1)
