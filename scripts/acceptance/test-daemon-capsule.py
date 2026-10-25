#!/usr/bin/env python3
"""Offline G8 validation evidence. Never launches Switch.app or switchd-rs."""
import hashlib
import json
import os
from pathlib import Path
import sys
import re
import stat

from evidence import clean_owned_caches, create_report, finish_inputs, offline_environment, run_command, tree_snapshot, write_json

SOURCE = Path(__file__).resolve().parents[2]
WORKSPACE = SOURCE.parent


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def actual_generated_snapshot(output, boundary, baseline_established=True):
    """Authoritative inventory of the actual cache, never the retained copy.

    Enumerate without following directory symlinks. Those are retained in the
    receipt and rejected because their consumed input closure is unbounded.
    File symlinks are accepted only when their target remains inside this cache.
    """
    cache = output / "compiler-cache"
    archive = output / ("actual-generated-" + boundary + "-files")
    archive.mkdir(mode=0o700)
    rows, errors = {}, []

    def visit(path):
        relative = str(path.relative_to(cache))
        try:
            mode = path.lstat().st_mode
            row = {"mode": stat.S_IMODE(mode)}
            rows[relative] = row
            if stat.S_ISLNK(mode):
                row.update(kind="symlink", target=os.readlink(path))
                target = path.resolve(strict=True)
                row["resolved_target"] = str(target)
                if not target.is_relative_to(cache.resolve()) or not target.is_file():
                    raise RuntimeError("generated symlink target is not an in-cache regular file")
                content = path.read_bytes()
                row["sha256"] = hashlib.sha256(content).hexdigest()
            elif stat.S_ISREG(mode):
                row["kind"] = "file"
                content = path.read_bytes()
                row["sha256"] = hashlib.sha256(content).hexdigest()
            elif stat.S_ISDIR(mode):
                row["kind"] = "directory"
                children = sorted(path.iterdir())
                row["children"] = [child.name for child in children]
                for child in children:
                    visit(child)
                row["sha256"] = hashlib.sha256(json.dumps({key: rows[key] for key in sorted(rows)
                                                           if key.startswith(relative + "/")}, sort_keys=True).encode()).hexdigest()
                return
            else:
                row["kind"] = "unsupported-special-file"
                raise RuntimeError("unsupported generated input kind")
            destination = archive / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            with destination.open("xb") as stream:
                stream.write(content)
            if sha(destination) != row["sha256"]:
                raise RuntimeError("generated input preservation mismatch")
        except Exception as exc:
            errors.append({"path": relative, "failure": repr(exc)})

    try:
        build = cache / "debug/build"
        if build.is_symlink():
            raise RuntimeError("build directory symlink")
        for package in sorted(build.iterdir()) if build.exists() else []:
            if package.is_symlink():
                raise RuntimeError("build package directory symlink")
            out = package / "out"
            if out.exists() or out.is_symlink():
                visit(out)
    except Exception as exc:
        errors.append({"path": "debug/build", "failure": repr(exc)})
    result = {"schema": 1, "authority": "actual compiler-cache/debug/build/*/out",
              "boundary": boundary, "baseline_established": baseline_established,
              "cache_exists": cache.exists(), "entries": rows, "errors": errors}
    write_json(output / ("actual-generated-" + boundary + ".json"), result)
    return result


def run_batch(output, environment, cargo_path, swift_path, checks, generated):
    cargo = [str(cargo_path)]
    manifest = SOURCE / "scripts/acceptance/daemon-capsule-fixture/Cargo.toml"
    target = output / "compiler-cache"
    common = ["--offline", "--locked", "--manifest-path", str(manifest), "--target-dir", str(target)]

    def run(label, command):
        checks.append(run_command(output, label, [os.fspath(v) for v in command], SOURCE, environment))

    # Compile every tested Rust target first. Establish the actual OUT_DIR
    # baseline before copying generated inputs or executing any test binary.
    run("rust-fixture-build", cargo + ["build"] + common)
    run("rust-tests-build", cargo + ["test", "--no-run"] + common)
    actual_before = actual_generated_snapshot(output, "before")
    if actual_before["errors"]:
        raise RuntimeError("actual generated input baseline is incomplete")
    generated_tree = output / "generated-rust-inputs"
    generated_tree.mkdir(mode=0o700)
    for path in sorted((target / "debug/build").glob("*/out/**/*")):
        if path.is_file():
            destination = generated_tree / path.relative_to(target / "debug/build")
            destination.parent.mkdir(parents=True, exist_ok=True)
            with destination.open("xb") as stream:
                stream.write(path.read_bytes())
            generated[str(destination)] = sha(destination)
    rust_fixture = output / "bin/daemon-capsule-fixture"
    with rust_fixture.open("xb") as stream:
        stream.write((target / "debug/daemon-capsule-fixture").read_bytes())
    rust_fixture.chmod(0o700)
    generated[str(rust_fixture)] = sha(rust_fixture)
    run("rust-tests", cargo + ["test"] + common + ["--", "--test-threads=1"])
    run("swift-fixture-build", [swift_path, "-parse-as-library", "-module-cache-path", output / "swift-cache",
        "app/Sources/AcceptanceContext.swift", "scripts/acceptance/GuardianFixture.swift",
        "scripts/acceptance/daemon-capsule-fixture/SwiftDelegation.swift", "-o", target / "swift-delegation"])
    swift_fixture = output / "bin/swift-delegation"
    with swift_fixture.open("xb") as stream:
        stream.write((target / "swift-delegation").read_bytes())
    swift_fixture.chmod(0o700)
    generated[str(swift_fixture)] = sha(swift_fixture)
    write_json(output / "generated-before.json", generated)
    chain = output / "swift-live-kernel-chain"
    if chain.exists():
        raise SystemExit("kernel-chain root already exists; use a new report directory")
    run("swift-live-kernel-chain", [output / "bin/swift-delegation", chain, rust_fixture])
    if "SWIFT_DAEMON_SCHEMA_KERNEL_PARENT_VALIDATED_REPLAY_REFUSED_NO_GO" not in (output / "swift-live-kernel-chain.log").read_text():
        raise SystemExit("kernel parent validation marker absent")
    for mode, marker in [("alternate-parent", "SWIFT_RENAMED_PARENT_SAME_HASH_INODE_KERNEL_REFUSED_NO_GO"),
                         ("expired-child", "SWIFT_EXPIRED_CHILD_KERNEL_REFUSED_NO_GO")]:
        run("swift-kernel-" + mode, [output / "bin/swift-delegation", output / ("swift-kernel-" + mode), rust_fixture, mode])
        if marker not in (output / ("swift-kernel-" + mode + ".log")).read_text():
            raise SystemExit("adversarial kernel rejection marker absent: " + mode)
    rows = []
    cases = [("production", [], 0), ("empty", ["--switch-acceptance-capsule="], 78),
             ("separate", ["--switch-acceptance-capsule", "/missing"], 78),
             ("unknown", ["--switch-acceptance-unsafe"], 78),
             ("relative", ["--switch-acceptance-capsule=relative"], 78),
             ("duplicate", ["--switch-acceptance-capsule=/a"] * 2, 78),
             ("missing-file", ["--switch-acceptance-capsule=/missing"], 78),
             ("hidden-positional", ["value", "--switch-acceptance-capsule="], 78),
             ("hidden-terminator", ["--", "--switch-acceptance-capsule="], 78),
             ("single-dash", ["-switch-acceptance-capsule=/missing"], 78),
             ("invalid-utf8", [b"--switch-acceptance-capsule=/bad\xff"], 78)]
    for label, args, expected in cases:
        result = run_command(output, "bootstrap-" + label, [os.fsencode(rust_fixture)] + [os.fsencode(v) for v in args],
                             SOURCE, environment, timeout=10, expected=expected)
        sentinel = b"SERVICE_INITIALIZATION_SENTINEL" in (output / ("bootstrap-" + label + ".log")).read_bytes()
        if sentinel != (expected == 0):
            raise SystemExit("dependency sentinel fence failed: " + label)
        rows.append({"case": label, "exit_code": result["exit_code"], "dependency_sentinel_reached": sentinel})
    write_json(output / "bootstrap-sentinels.json", rows)
    main_source = (SOURCE / "rust/crates/proletariusd/src/main.rs").read_text()
    for function, start in [("main", "async fn main()"), ("run", "async fn run(arguments:")]:
        body = main_source[main_source.index(start):]
        gate = body.index("acceptance_capsule::pre_entry(")
        if any(body.index(dependency) < gate for dependency in ["required_app_token()", "data_root::resolve()"]):
            raise SystemExit(function + " moved pre-entry behind dependencies")
    paths = [SOURCE / p for p in ["rust/crates/proletariusd/src/acceptance_capsule.rs",
        "rust/crates/proletariusd/src/acceptance_capsule/tests.rs", "rust/crates/proletariusd/src/daemon_args.rs",
        "rust/crates/proletariusd/src/main.rs", "rust/crates/proletarius-platform-fs/src/process_identity.rs",
        "rust/crates/proletarius-platform-fs/src/lib.rs", "app/Sources/AcceptanceContext.swift", "app/Tests/AcceptanceContext/main.swift",
        "scripts/acceptance/GuardianFixture.swift", "scripts/acceptance/test-daemon-capsule.py",
        "scripts/acceptance/daemon-capsule-fixture/Cargo.toml", "scripts/acceptance/daemon-capsule-fixture/Cargo.lock",
        "scripts/acceptance/daemon-capsule-fixture/main.rs", "scripts/acceptance/daemon-capsule-fixture/SwiftDelegation.swift"]]
    write_json(output / "source-sha256.json", {str(p.relative_to(SOURCE)):sha(p) for p in paths})
    report = {"scope":"G8-daemon-capsule-preentry-validator-offline-only",
        "rust_tests":13,"renamed_parent_same_hash_inode_kernel_refused":True,"expired_child_kernel_refused":True,"bootstrap_process_cases":len(rows),"swift_createDaemonCapsule_exact_schema":True,
        "kernel_direct_parent_chain_verified":True,"shipping_main_run_source_order_verified":True,
        "actual_app_launched":False,"actual_daemon_launched":False,"runtime_admission":"NO-GO",
        "G8":"NO-GO pending independent review and actual acceptance composition",
        "G9":"NO-GO", "formal_duration_credit_seconds":0,
        "limits":["Descriptors pin observations, not same-UID immutable exec custody.",
          "Foreign filesystem owner refusal is covered by metadata predicate; no privileged chown was performed.",
          "Shipping main/run were source-bound; only standalone exact-module sentinel fixture executed.",
          "No Keychain, real Application Support, hardware, external network, LaunchAgent or DMG access.",
          "P0-P6 overall progress follows parent ledger; this slice grants no product completion percentage."]}
    return report


def input_roots(environment):
    # Inventory whole source trees rather than a hand-picked 14-file list.
    roots = [SOURCE / "rust", SOURCE / "scripts/acceptance", SOURCE / "app/Sources/AcceptanceContext.swift",
             SOURCE / "app/Tests/AcceptanceContext", Path(sys.executable), Path("/usr/bin/xcrun"),
             Path("/usr/bin/sandbox-exec")]
    for parent in [SOURCE, *SOURCE.parents]:
        roots.extend([parent / ".cargo/config", parent / ".cargo/config.toml",
                      parent / "rust-toolchain", parent / "rust-toolchain.toml"])
    cargo_home = Path(environment["CARGO_HOME"])
    roots.extend([cargo_home / "config", cargo_home / "config.toml", Path(environment["RUSTUP_HOME"]) / "settings.toml"])
    lock = (SOURCE / "scripts/acceptance/daemon-capsule-fixture/Cargo.lock").read_text()
    for block in lock.split("[[package]]")[1:]:
        package = {key: json.loads(value) for key, value in re.findall(r'^(name|version|source) = ("[^"\n]+")$', block, re.MULTILINE)}
        if "name" not in package or "version" not in package:
            raise RuntimeError("unrecognized lock package; fail closed")
        if package.get("source", "").startswith("registry+"):
            candidates = sorted((cargo_home / "registry/src").glob("*/" + package["name"] + "-" + package["version"]))
            if len(candidates) != 1:
                raise RuntimeError("offline dependency missing or ambiguous: " + package["name"])
            roots.extend(candidates)
    return roots


def tool_snapshot(output, environment, label):
    rustup = str(Path(environment["CARGO_HOME"]) / "bin/rustup")
    paths, facts = {}, {}
    for tool in ("cargo", "rustc", "swiftc"):
        command = [rustup, "which", tool] if tool != "swiftc" else ["/usr/bin/xcrun", "--find", tool]
        run_command(output, label + "-" + tool + "-path", command, SOURCE, environment)
        paths[tool] = (output / (label + "-" + tool + "-path.log")).read_text().strip()
        run_command(output, label + "-" + tool + "-version", [paths[tool], "--version"], SOURCE, environment)
        facts[tool] = {"path": paths[tool], "version": (output / (label + "-" + tool + "-version.log")).read_text(),
                       "sha256": sha(Path(paths[tool]))}
    for option in ("--show-sdk-path", "--show-sdk-version"):
        run_command(output, label + option, ["/usr/bin/xcrun", option], SOURCE, environment)
        facts[option] = (output / (label + option + ".log")).read_text().strip()
    sdk = Path(facts["--show-sdk-path"])
    facts["sdk_settings"] = tree_snapshot([sdk / "SDKSettings.json", sdk / "SDKSettings.plist"])
    facts["rustup"] = {"path": rustup, "sha256": sha(Path(rustup))}
    run_command(output, label + "-cargo-metadata", [paths["cargo"], "metadata", "--offline", "--locked",
                "--format-version", "1", "--manifest-path",
                str(SOURCE / "scripts/acceptance/daemon-capsule-fixture/Cargo.toml")], SOURCE,
                dict(environment, RUSTC=paths["rustc"]))
    metadata = json.loads((output / (label + "-cargo-metadata.log")).read_text())
    facts["dependency_roots"] = sorted({str(Path(package["manifest_path"]).parent) for package in metadata["packages"]})
    allowed = [SOURCE.resolve(), (Path(environment["CARGO_HOME"]) / "registry/src").resolve()]
    if any(not any(Path(root).resolve().is_relative_to(base) for base in allowed) for root in facts["dependency_roots"]):
        raise RuntimeError("dependency root outside reviewed offline source roots")
    facts["dependency_graph"] = metadata
    return paths, facts


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: test-daemon-capsule.py NEW_ABSOLUTE_REPORT")
    output = create_report(sys.argv[1], WORKSPACE)
    environment = offline_environment(output)
    environment["G8_FIXTURE_ROOT"] = str(output / "fixtures")
    for name in ("fixtures", "bin"):
        (output / name).mkdir(mode=0o700)
    checks, generated, tools = [], {}, {}
    snapshot_count = 0
    def snapshot():
        nonlocal snapshot_count
        label = "inputs-" + str(snapshot_count)
        snapshot_count += 1
        paths, facts = tool_snapshot(output, environment, label)
        tools.update(paths)
        return {"files": tree_snapshot(input_roots(environment) + [Path(p) for p in facts.get("dependency_roots", [])]),
                "tools": facts, "environment": dict(environment)}
    before, error = {}, None
    report = {"scope": "offline-fixture-incomplete", "checks": checks, "formal_duration_credit_seconds": 0,
              "G8": "NO-GO", "G9": "NO-GO"}
    try:
        before = snapshot()
        environment["RUSTC"] = tools["rustc"]
        environment["SDKROOT"] = before["tools"]["--show-sdk-path"]
        before["environment"] = dict(environment)
        write_json(output / "inputs-before.json", before)
        report = run_batch(output, environment, tools["cargo"], tools["swiftc"], checks, generated)
    except (Exception, SystemExit) as exc:
        error = exc
    if not (output / "inputs-before.json").exists():
        write_json(output / "inputs-before.json", {"snapshot_failed": repr(error), "partial": before})
    if not (output / "generated-before.json").exists():
        write_json(output / "generated-before.json", generated)
    actual_before_path = output / "actual-generated-before.json"
    if not actual_before_path.exists():
        actual_generated_snapshot(output, "before", baseline_established=False)
    actual_before = json.loads(actual_before_path.read_text())
    actual_after = actual_generated_snapshot(output, "after")
    actual_changed = sorted(key for key in actual_before["entries"].keys() | actual_after["entries"].keys()
                            if actual_before["entries"].get(key) != actual_after["entries"].get(key))
    actual_valid = actual_before["baseline_established"] and not actual_before["errors"] and not actual_after["errors"]
    actual_stable = actual_valid and not actual_changed and actual_before["cache_exists"] == actual_after["cache_exists"]
    report["actual_generated_inputs"] = {"before": "actual-generated-before.json", "after": "actual-generated-after.json",
                                          "stable_at_boundaries": actual_stable, "changed_paths": actual_changed}
    if not actual_stable:
        report["actual_generated_failure"] = "actual OUT_DIR changed or could not be fully inventoried; no passing credit"
        error = RuntimeError(report["actual_generated_failure"] + ("; earlier failure: " + repr(error) if error else ""))
    generated_after = {str(p): sha(p) for root in (output / "bin", output / "generated-rust-inputs")
                       for p in root.rglob("*") if p.is_file()}
    write_json(output / "generated-after.json", generated_after)
    if generated != generated_after:
        error = RuntimeError("generated binary drift")
    try:
        # Actual before/after receipts and raw bytes are durable before cleanup.
        # Incomplete snapshots retain the actual cache as unique failure evidence.
        names = ["compiler-cache", "swift-cache"] if actual_valid else ["swift-cache"]
        if not actual_valid:
            write_json(output / "retained-cache.json", {"path": str(output / "compiler-cache"),
                                                       "reason": "incomplete actual generated-input evidence"})
        clean_owned_caches(output, names)
    except Exception as exc:
        error = exc
    status = finish_inputs(output, before, snapshot, report, error)
    write_json(output / "commands.json", checks)
    write_json(output / "artifact-sha256.json", {str(p.relative_to(output)): sha(p) for p in output.rglob("*")
                                                if p.is_file() and p.name != "runner.log"})
    return status


if __name__ == "__main__":
    raise SystemExit(main())
