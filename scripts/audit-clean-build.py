#!/usr/bin/env python3
"""Prepare/validate a cold macOS build capsule; never build without `execute`.

This proves a fresh source export and fresh compiler outputs, not a hermetic OS
or a source rebuild of Xcode, Rust, xcodegen, pkg-config, or installed libusb.
Downloaded Cargo .crate archives and index metadata may be copied; expanded
registry sources, Cargo configuration, wrappers and compiled outputs are not.
"""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import uuid
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT.parent / "build"
CAPSULES = BUILD / "clean-build"
ARM_STD_ARCHIVE_REL = Path("build/toolchains/downloads/rust-std-1.98.0-armv7-unknown-linux-musleabi.tar.xz")
ARM_STD_ARCHIVE_SHA256 = "01503f49f85d80cc2d852e6278f0bb226e86bc20e94ef40c39bca4566b60c5a6"
SYSTEM_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"
GENERATED = {".git", "target", "DerivedData", ".build", "__pycache__", "build", "dist"}
COMPILED = {".o", ".obj", ".a", ".lib", ".dylib", ".dll", ".exe", ".so", ".rlib", ".rmeta", ".swiftmodule", ".pyc"}
MACHO = {b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf"}
GO_MARKERS = {"buildinfo": b"\xff Go buildinf:", "build-id": b"Go build ID:", "pclntab-section": b"__gopclntab", "buildinfo-section": b"__go_buildinfo"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def recovery_checker():
    spec = importlib.util.spec_from_file_location(
        "cold_recovery_process_checker", ROOT / "scripts/check-recovery-process-gates.py")
    checker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(checker)
    return checker


def verify_recovery_evidence(capsule, source_digest, execution):
    bound = recovery_checker().verify_full_marker(
        (capsule / "build.log").read_text(), source_digest,
        reports_root=capsule / "build/reports")
    require(execution.get("recovery_process_gate") == bound,
            "cold execution does not bind its complete recovery-process evidence")
    return bound


def app_bundle_inventory(app, *, inventory_root=None):
    """Return a stable inventory of every directory and regular file.

    The artifact audit, rather than the later attestation generator, owns this
    inventory.  Reviewers can therefore bind the complete bundle tree by
    hashing artifact-audit.json before any release attestation is created.
    Directory modes and empty directories are included; regular files are read
    through no-follow descriptors and must remain single-link and unchanged.
    The inventory path is relative to the trusted root, which defaults to the
    workspace for existing callers. Resolved bundles must remain inside it.
    """
    require(app.is_dir() and not app.is_symlink() and app.suffix == ".app",
            "audited App bundle is missing or unsafe")
    if inventory_root is None:
        inventory_root = ROOT.parent
    try:
        inventory_path = app.resolve().relative_to(inventory_root.resolve()).as_posix()
    except ValueError as error:
        raise ValueError("audited App bundle is outside its trusted inventory root") from error
    directories = [{"path": ".", "mode": stat.S_IMODE(app.lstat().st_mode)}]
    files = []
    for path in sorted(app.rglob("*")):
        before = path.lstat()
        require(not stat.S_ISLNK(before.st_mode),
                f"App bundle symlink is not allowed: {path}")
        relative = path.relative_to(app).as_posix()
        if stat.S_ISDIR(before.st_mode):
            directories.append({"path": relative, "mode": stat.S_IMODE(before.st_mode)})
            continue
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1,
                f"App bundle entry is not a single-link regular file: {path}")
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        try:
            opened = os.fstat(descriptor)
            require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino)
                    and stat.S_ISREG(opened.st_mode) and opened.st_nlink == 1,
                    f"App bundle entry changed before it was opened: {path}")
            with os.fdopen(descriptor, "rb", closefd=False) as stream:
                data = stream.read()
            after = os.fstat(descriptor)
            require((opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns)
                    == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns),
                    f"App bundle entry changed while it was inventoried: {path}")
        finally:
            os.close(descriptor)
        files.append({
            "path": relative,
            "sha256": hashlib.sha256(data).hexdigest(),
            "bytes": len(data),
            "mode": stat.S_IMODE(opened.st_mode),
        })
    require(files, "audited App bundle is empty")
    tree = {"directories": directories, "files": files}
    encoded = json.dumps(tree, ensure_ascii=False, sort_keys=True,
                         separators=(",", ":")).encode()
    return {
        "path": inventory_path,
        "file_count": len(files),
        "directory_count": len(directories),
        "tree_sha256": hashlib.sha256(encoded).hexdigest(),
        "directories": directories,
        "files": files,
    }


def generated(relative):
    return any(part in GENERATED or part.endswith((".xcodeproj", ".xcworkspace", ".app", ".dSYM")) for part in relative.parts)


def source_entry(root, relative):
    require(not relative.is_absolute() and ".." not in relative.parts, "unsafe source path")
    path = root / relative
    require(not path.is_symlink(), f"source symlink requires explicit audit: {relative}")
    require(path.is_file(), f"source is not a regular file: {relative}")
    require(relative.suffix not in COMPILED, f"compiled source input: {relative}")
    require(relative.suffix != ".go" and relative.name not in {"go.mod", "go.sum", "go.work", "go.work.sum"}, f"Go source input: {relative}")
    with path.open("rb") as stream:
        prefix = stream.read(8)
    require(prefix[:4] not in MACHO and not prefix.startswith((b"\x7fELF", b"!<arch>\n")), f"native binary source input: {relative}")
    return {"path": relative.as_posix(), "sha256": sha(path), "bytes": path.stat().st_size, "mode": stat.S_IMODE(path.stat().st_mode)}


def source_inventory(root):
    raw = subprocess.check_output(["/usr/bin/git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard"])
    result = []
    for name in sorted(set(os.fsdecode(raw).split("\0")) - {""}):
        relative = Path(name)
        if generated(relative):
            continue
        # Deleted tracked files are intentionally absent in this working-tree export.
        if not (root / relative).exists() and not (root / relative).is_symlink():
            continue
        result.append(source_entry(root, relative))
    require(result, "empty source inventory")
    return result


def tool_path(name):
    value = shutil.which(name)
    require(value, f"required installed tool missing: {name}")
    return str(Path(value).resolve())


def toolchain_identity(configured):
    """Describe observed compiler inputs; installer metadata is optional.

    Rustup places manifests under lib/rustlib, but linked/custom sysroots may
    have none. File identity is therefore established independently of those
    manifests. Documentation under share is excluded from compilation coverage.
    """
    configured = Path(configured)
    root = configured.resolve()
    require(root.is_dir(), "toolchain sysroot missing")
    for name in ("cargo", "rustc", "rustdoc"):
        require((root / "bin" / name).is_file(), f"toolchain binary missing: {name}")
    stdlib = root / "lib/rustlib/aarch64-apple-darwin/lib"
    require(any(stdlib.glob("libstd-*.rlib")), "arm64 Rust standard library missing")
    files = []
    for directory in ("bin", "lib", "libexec", "etc"):
        require(not (root / directory).is_symlink(), f"unreviewed sysroot directory symlink: {directory}")
        for path in sorted((root / directory).rglob("*")):
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                require(path.is_file() and root in path.resolve().parents, f"unreviewed sysroot symlink: {relative}")
            elif path.is_dir():
                continue
            require(path.is_file(), f"non-regular sysroot input: {relative}")
            files.append({"path": relative, "sha256": sha(path), "bytes": path.stat().st_size,
                          "mode": stat.S_IMODE(path.stat().st_mode),
                          "symlink": os.readlink(path) if path.is_symlink() else None})
    metadata = [item["path"] for item in files if Path(item["path"]).name in
                {"multirust-channel-manifest.toml", "multirust-config.toml", "components", "rust-installer-version"}
                or Path(item["path"]).name.startswith("manifest-")]
    return {"configured_path": str(configured), "resolved_path": str(root),
            "root_is_symlink": configured.is_symlink(), "metadata_files": metadata,
            "layout": "symlinked-sysroot" if configured.is_symlink() else "directory-sysroot",
            "coverage": ["bin", "lib", "libexec", "etc"], "excluded": ["share documentation"], "files": files}


def source_libusb_notice():
    # No argument/environment can supply the path, size or expected digest.
    # ROOT is the canonical source/export root derived from this script.
    notice = ROOT / "licenses/libusb-COPYING"
    require(notice.is_file() and not notice.is_symlink()
            and notice.stat().st_size == 26436
            and sha(notice) == "5df07007198989c622f5d41de8d703e7bef3d0e79d62e24332ee739a452af62a",
            "source-pinned libusb notice missing or changed")
    return notice


def dependencies(args):
    require(sys.platform == "darwin" and platform.machine() == "arm64", "production clean build requires Apple Silicon macOS")
    developer = Path(args.developer_dir).resolve()
    sdk = developer / "Platforms/MacOSX.platform/Developer/SDKs/MacOSX27.sdk"
    require(sdk.is_dir(), "Xcode macOS 27 SDK missing")
    rustup_home = Path(args.rustup_home).expanduser().resolve()
    toolchain = rustup_home / "toolchains/1.98.0-aarch64-apple-darwin"
    require((toolchain / "lib/rustlib/aarch64-apple-darwin/lib").is_dir(), "installed Rust 1.98.0 arm64 toolchain missing")
    for name in ("cargo-fmt", "rustfmt"):
        require((toolchain / "bin" / name).is_file(), f"Rust formatting component missing: {name}")
    tools = {name: tool_path(name) for name in ("rustup", "xcodegen", "pkg-config")}
    tools["python3"] = "/usr/bin/python3"
    for name in ("cargo", "rustc", "rustdoc", "cargo-fmt", "rustfmt"):
        # Rustup dispatches by argv[0]. Keep every command required by the
        # migration gate available inside the isolated capsule PATH.
        tools[name] = tools["rustup"]
    source_libusb_notice()
    libusb = Path(args.libusb).resolve()
    pkgconfig = Path(args.libusb_pkgconfig).resolve()
    arm_std_archive = BUILD / ARM_STD_ARCHIVE_REL.relative_to("build")
    require(arm_std_archive.is_file() and not arm_std_archive.is_symlink(), "pinned ARM Rust std archive missing")
    require(sha(arm_std_archive) == ARM_STD_ARCHIVE_SHA256, "pinned ARM Rust std archive checksum mismatch")
    inputs = [libusb, pkgconfig / "libusb-1.0.pc", toolchain / "bin/cargo", toolchain / "bin/rustc", toolchain / "bin/rustdoc", arm_std_archive]
    inputs += [Path(value) for value in tools.values()]
    inputs += [developer.parent / "Info.plist"]
    identity = toolchain_identity(toolchain)
    for path in inputs:
        require(path.is_file(), f"installed dependency missing: {path}")
    return {"developer_dir": str(developer), "sdk": str(sdk.resolve()), "rustup_home": str(rustup_home), "toolchain": str(toolchain), "toolchain_identity": identity, "tools": tools, "libusb": str(libusb), "libusb_pkgconfig": str(pkgconfig), "input_hashes": {str(path): sha(path) for path in sorted(set(inputs))}}


def clean_environment(capsule, dep):
    # Construct from scratch: no inherited flags, wrappers, SDKROOT, DYLD_*,
    # cache servers, Cargo config, proxy credentials or compiler overrides.
    # XcodeGen requires the login name; derive it from the actual process UID,
    # never from caller-controlled USER/LOGNAME environment values.
    username = pwd.getpwuid(os.getuid()).pw_name
    return {
        "PATH": str(capsule / "tools") + ":" + SYSTEM_PATH,
        "HOME": str(Path.home()), "USER": username, "LOGNAME": username,
        "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8",
        "DEVELOPER_DIR": dep["developer_dir"],
        "RUSTUP_HOME": dep["rustup_home"], "RUSTUP_TOOLCHAIN": "1.98.0", "RUSTUP_AUTO_INSTALL": "0",
        "SWITCH_CARGO_BIN": str(capsule / "tools/cargo"),
        # The source export deliberately omits historical build/dist evidence.
        # Gates may check its committed scope, but cannot attest those artifacts here.
        "SWITCH_COLD_SOURCE_ONLY": "1",
        "CARGO_HOME": str(capsule / "build/cargo-home"), "CARGO_NET_OFFLINE": "true", "CARGO_INCREMENTAL": "0",
        "CARGO_TARGET_DIR": str(capsule / "build/rust-target"),
        # Omit debug symbols without changing optimization or runtime checks.
        "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_TEST_DEBUG": "0",
        # macOS AF_UNIX sun_path is limited to 104 bytes. Test fixtures that
        # derive socket paths from the Cargo manifest can already run near
        # that limit inside a clean-build capsule, so keep OS temp fixtures
        # on the short, system-private temp root. Cargo and build outputs stay
        # isolated under the capsule as before.
        "CFFIXED_USER_HOME": str(capsule / "build/test-home"),
        "SWITCH_TEST_TMPDIR": "/private/tmp/", "TMPDIR": "/private/tmp/",
        "XDG_CACHE_HOME": str(capsule / "build/cache"),
        "LIBUSB_DYLIB_ARM64": dep["libusb"],
        "PKG_CONFIG_LIBDIR": dep["libusb_pkgconfig"], "PKG_CONFIG_PATH": dep["libusb_pkgconfig"],
        "MACOSX_DEPLOYMENT_TARGET": "27.0",
    }


def assert_no_go(path):
    require(shutil.which("go", path=path) is None, "Go executable is available on audited PATH")
    require(shutil.which("gofmt", path=path) is None, "gofmt executable is available on audited PATH")


def cache_copy(source, destination):
    copied = []
    # No registry/src: extract pristine downloaded crates in the new Cargo home.
    for name in ("cache", "index"):
        folder = source / "registry" / name
        require(folder.is_dir(), f"downloaded Cargo registry {name} is missing")
        for path in sorted(folder.rglob("*")):
            require(not path.is_symlink(), "registry cache contains a symlink")
            if not path.is_file():
                continue
            relative = path.relative_to(source)
            if name == "cache":
                require(path.suffix == ".crate", "unexpected non-crate registry cache entry")
            output = destination / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, output)
            copied.append({"path": relative.as_posix(), "sha256": sha(output), "bytes": output.stat().st_size})
    require(copied, "empty downloaded registry cache")
    return copied


def capsule_path(value):
    path = Path(value).resolve()
    require(path.parent == CAPSULES.resolve(), "capsule must be an immediate child of Switch_MAC/build/clean-build")
    require(path.is_dir() and not Path(value).is_symlink(), "capsule directory missing or symlinked")
    return path


def validate(capsule, fresh=False):
    manifest = json.loads((capsule / "audit.json").read_text())
    require(manifest["capsule"] == str(capsule), "capsule identity mismatch")
    source = capsule / "src"
    for directory in (source, capsule / "build", capsule / "tools"):
        require(directory.is_dir() and not directory.is_symlink(), "capsule directory replaced by symlink")
    for path in source.rglob("*"):
        require(not path.is_symlink(), "symlink injected into exported source")
        if fresh:
            require(not generated(path.relative_to(source)), "generated output injected into fresh source export")
    # Cargo also reads configuration from cwd ancestors, independently of
    # CARGO_HOME. A clean cache alone does not neutralize those configurations.
    for ancestor in (source / "rust", source, *source.parents):
        for name in ("config", "config.toml"):
            require(not (ancestor / ".cargo" / name).exists(), "ancestor Cargo configuration requires explicit audit")
    expected = {item["path"]: item for item in manifest["source"]}
    for relative, item in expected.items():
        require(source_entry(source, Path(relative)) == item, f"export changed: {relative}")
    actual = {p.relative_to(source).as_posix() for p in source.rglob("*") if p.is_file() and not generated(p.relative_to(source))}
    require(actual == set(expected), "unexpected file in exported source")
    require(toolchain_identity(manifest["dependencies"]["toolchain"]) == manifest["dependencies"]["toolchain_identity"], "installed compiler sysroot identity changed")
    for path, digest in manifest["dependencies"]["input_hashes"].items():
        require(sha(Path(path)) == digest, f"installed dependency changed: {path}")
    env = clean_environment(capsule, manifest["dependencies"])
    for name, target in manifest["dependencies"]["tools"].items():
        require((capsule / "tools" / name).is_symlink() and (capsule / "tools" / name).resolve() == Path(target), f"tool link changed: {name}")
    require(set(p.name for p in (capsule / "tools").iterdir()) == set(manifest["dependencies"]["tools"]), "unexpected tool injected")
    assert_no_go(env["PATH"])
    cargo_home = capsule / "build/cargo-home"
    require(cargo_home.is_dir() and not cargo_home.is_symlink(), "Cargo home root replaced by symlink")
    require(not (capsule / "dist").is_symlink(), "distribution root replaced by symlink")
    require(not (cargo_home / "config").exists() and not (cargo_home / "config.toml").exists(), "Cargo configuration injected")
    require(not (source / ".cargo").exists() and not (source / "rust/.cargo").exists(), "source Cargo config needs explicit audit")
    for path in cargo_home.rglob("*"):
        require(not path.is_symlink(), "symlink injected into Cargo home")
    for item in manifest["cache"]:
        require(sha(cargo_home / item["path"]) == item["sha256"], "downloaded registry cache changed")
    arm_archive = manifest.get("arm_std_archive", {})
    archive_relative = Path(arm_archive.get("path", ""))
    require(archive_relative == ARM_STD_ARCHIVE_REL, "pinned ARM std archive path changed")
    require(arm_archive.get("sha256") == ARM_STD_ARCHIVE_SHA256, "pinned ARM std archive digest changed")
    capsule_archive = capsule / archive_relative
    require(capsule_archive.is_file() and not capsule_archive.is_symlink(), "cold-build ARM std archive missing")
    require(sha(capsule_archive) == ARM_STD_ARCHIVE_SHA256, "cold-build ARM std archive checksum mismatch")
    if fresh:
        for path in (capsule / "build").rglob("*"):
            require(not path.is_symlink(), "symlink injected into fresh build outputs")
        require(not (capsule / "execution.json").exists(), "capsule already executed; prepare a new one")
        require({p.relative_to(cargo_home).as_posix() for p in cargo_home.rglob("*") if p.is_file()} == {i["path"] for i in manifest["cache"]}, "unexpected preexisting Cargo state")
        expected_files = {("cargo-home" / Path(item["path"])).as_posix() for item in manifest["cache"]}
        expected_files.add(archive_relative.relative_to("build").as_posix())
        actual_files = {path.relative_to(capsule / "build").as_posix()
                        for path in (capsule / "build").rglob("*") if path.is_file()}
        require(actual_files == expected_files, "unexpected preexisting clean-build input or compiler output")
        allowed_directories = {Path("cargo-home"), Path("tmp"), Path("test-home")}
        for relative in expected_files:
            parent = Path(relative).parent
            while parent != Path("."):
                allowed_directories.add(parent)
                parent = parent.parent
        actual_directories = {path.relative_to(capsule / "build")
                              for path in (capsule / "build").rglob("*") if path.is_dir()}
        require(actual_directories <= allowed_directories, "unexpected preexisting clean-build directory")
        require(not (capsule / "dist").exists() or not any((capsule / "dist").iterdir()), "preexisting distribution output")
    return manifest, env


def prepare(args):
    entries = source_inventory(ROOT)
    dep = dependencies(args)
    require(not re.search(r'^source = "git\+', (ROOT / "rust/Cargo.lock").read_text(), re.M), "git Cargo dependencies need a separately audited source cache")
    CAPSULES.mkdir(parents=True, exist_ok=True)
    name = args.run_id or datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ-") + uuid.uuid4().hex[:8]
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,90}", name), "invalid run id")
    capsule = CAPSULES / name
    capsule.mkdir()  # never overwrite or delete a previous capsule
    (capsule / "src").mkdir()
    (capsule / "tools").mkdir()
    for item in entries:
        relative = Path(item["path"])
        output = capsule / "src" / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / relative, output)
        output.chmod(item["mode"])
        require(source_entry(capsule / "src", relative) == item, "source changed during export")
    for name, target in dep["tools"].items():
        (capsule / "tools" / name).symlink_to(target)
    cache = cache_copy(Path(args.cargo_cache).resolve(), capsule / "build/cargo-home")
    arm_archive_source = BUILD / ARM_STD_ARCHIVE_REL.relative_to("build")
    arm_archive_destination = capsule / ARM_STD_ARCHIVE_REL
    arm_archive_destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(arm_archive_source, arm_archive_destination)
    arm_archive_destination.chmod(0o644)
    require(sha(arm_archive_destination) == ARM_STD_ARCHIVE_SHA256,
            "ARM std archive changed during clean-build capsule preparation")
    for name in ("tmp", "test-home"):
        (capsule / "build" / name).mkdir()
    manifest = {"schema": 1, "capsule": str(capsule), "source_root": str(ROOT), "source": entries, "cache": cache, "dependencies": dep,
                "arm_std_archive": {"path": ARM_STD_ARCHIVE_REL.as_posix(), "sha256": ARM_STD_ARCHIVE_SHA256},
                "min_free_gib": args.min_free_gib,
                "boundaries": ["Current working-tree export, including tracked edits and nonignored new files; no Git history or generated project", "Only downloaded Cargo archives/index and one checksum-pinned ARM std archive reused; expanded sources and compiler outputs are fresh", "Historical build/dist receipts are excluded; their version-scoped source declaration is checked here and their artifacts require separate full verification", "Installed Xcode/Rust/xcodegen/pkg-config and arm64 libusb are external trusted inputs", "PATH and environment isolation are not an OS sandbox or a proof against malicious absolute-path execution", "Mach-O marker checks are evidence combined with source/build provenance, not a universal language detector", "Offline build needs all locked Cargo archives/index and the pinned ARM std archive present; no download or toolchain install is attempted"]}
    write_json(capsule / "audit.json", manifest)
    validate(capsule, fresh=True)
    print(json.dumps({"capsule": str(capsule), "source_files": len(entries), "source_bytes": sum(e["bytes"] for e in entries), "cache_bytes": sum(e["bytes"] for e in cache), "free_gib": round(shutil.disk_usage(capsule).free / 2**30, 2), "next": "execute " + str(capsule)}, indent=2))


def command(command, env, cwd, log):
    with log.open("ab") as output:
        output.write(("\n$ " + json.dumps(command) + "\n").encode())
        output.flush()
        subprocess.run(command, env=env, cwd=cwd, stdout=output, stderr=subprocess.STDOUT, check=True)


def execution_commands(capsule, with_tests):
    commands = [[str(capsule / "tools/python3"), "scripts/check-rust-backend-exit.py", "--require-go-free"],
                [str(capsule / "tools/python3"), "scripts/prepare-module-supervisor.py", "--fetch"]]
    if with_tests:
        commands.append(["/bin/sh", "scripts/test-migration-gates.sh"])
    commands.append(["/bin/sh", "scripts/build.sh"])
    return commands


def execute(capsule, with_tests=False):
    manifest, env = validate(capsule, fresh=True)
    require(shutil.disk_usage(capsule).free >= manifest["min_free_gib"] * 2**30, f"at least {manifest['min_free_gib']} GiB free required; no existing output will be removed")
    receipt = {"started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(), "commands": [], "status": "running", "path": env["PATH"], "environment_keys": sorted(env), "audit_sha256": sha(capsule / "audit.json"), "fresh_output_validation": "passed", "go_resolvable": False,
               "historical_evidence_scope": ("SOURCE_ONLY_NO_BUILD_ATTESTATION" if with_tests else "NOT_RUN"),
               "software_tests": "requested" if with_tests else "not_requested"}
    write_json(capsule / "execution.json", receipt)
    commands = execution_commands(capsule, with_tests)
    try:
        receipt["tool_versions"] = {
            "xcode": captured(["/usr/bin/xcodebuild", "-version"], env),
            "sdk": captured(["/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-version"], env),
            "rustc": captured([str(capsule / "tools/rustc"), "--version", "--verbose"], env),
            "cargo": captured([str(capsule / "tools/cargo"), "+1.98.0", "--version"], env),
            "xcodegen": captured([str(capsule / "tools/xcodegen"), "--version"], env),
        }
        require(re.search(r"^release: 1\.98\.0$", receipt["tool_versions"]["rustc"], re.M), "selected rustc is not release 1.98.0")
        require(re.search(r"^host: aarch64-apple-darwin$", receipt["tool_versions"]["rustc"], re.M), "selected rustc host is not Apple Silicon macOS")
        require(re.match(r"cargo 1\.98\.0(?: |$)", receipt["tool_versions"]["cargo"]), "selected cargo is not release 1.98.0")
        for invocation in commands:
            receipt["commands"].append(invocation)
            write_json(capsule / "execution.json", receipt)
            command(invocation, env, capsule / "src", capsule / "build.log")
            if invocation == ["/bin/sh", "scripts/test-migration-gates.sh"]:
                digest = captured([str(capsule / "tools/python3"), "scripts/source-identity.py"],
                                  env, cwd=capsule / "src").strip()
                receipt["recovery_process_gate"] = recovery_checker().verify_full_marker(
                    (capsule / "build.log").read_text(), digest,
                    reports_root=capsule / "build/reports")
                write_json(capsule / "execution.json", receipt)
        receipt["status"] = "built"
        if with_tests:
            receipt["software_tests"] = "passed"
    except BaseException:
        receipt["status"] = "failed"
        raise
    finally:
        receipt["finished_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(capsule / "execution.json", receipt)
    verify(capsule, require_tests=with_tests)


def captured(args, env, cwd=None):
    return subprocess.check_output(args, env=env, cwd=cwd, stderr=subprocess.STDOUT, text=True)


def minimum_macos(load_commands):
    versions = []
    for block in re.split(r"Load command \d+", load_commands):
        if "cmd LC_BUILD_VERSION" in block:
            found = re.search(r"^\s*minos ([0-9]+(?:\.[0-9]+){0,2})\s*$", block, re.M)
        elif "cmd LC_VERSION_MIN_MACOSX" in block:
            found = re.search(r"^\s*version ([0-9]+(?:\.[0-9]+){0,2})\s*$", block, re.M)
        else:
            continue
        require(found is not None, "Mach-O deployment version missing")
        parts = tuple(int(value) for value in found.group(1).split("."))
        versions.append(parts + (0,) * (3 - len(parts)))
    require(len(versions) == 1, "expected one macOS deployment version")
    return versions[0]


def verify(capsule, require_tests=False):
    manifest, env = validate(capsule)
    execution = json.loads((capsule / "execution.json").read_text())
    require(execution["status"] == "built" and execution["audit_sha256"] == sha(capsule / "audit.json"), "no matching successful cold-build receipt")
    recovery_evidence = None
    if require_tests:
        require(execution.get("software_tests") == "passed"
                and ["/bin/sh", "scripts/test-migration-gates.sh"] in execution.get("commands", [])
                and execution.get("historical_evidence_scope") == "SOURCE_ONLY_NO_BUILD_ATTESTATION",
                "cold-build receipt has no matching complete software-test run")
        digest = captured([str(capsule / "tools/python3"), "scripts/source-identity.py"],
                          env, cwd=capsule / "src").strip()
        recovery_evidence = verify_recovery_evidence(capsule, digest, execution)
    app = capsule / "dist/Switch.app"
    backend = app / "Contents/Resources/backend"
    require(backend.is_dir() and {p.name for p in backend.iterdir()} == {"switchd-rs", "libusb-1.0.0.dylib"}, "backend bundle must contain only Rust daemon and libusb")
    expected = {"Contents/MacOS/Switch", "Contents/Resources/backend/switchd-rs", "Contents/Resources/backend/libusb-1.0.0.dylib"}
    binaries = []
    for path in sorted(app.rglob("*")):
        require(not path.is_symlink(), "bundle symlink requires explicit audit")
        if not path.is_file():
            continue
        with path.open("rb") as stream:
            magic = stream.read(4)
        if magic not in MACHO:
            continue
        data = path.read_bytes()
        require(not any(marker in data for marker in GO_MARKERS.values()), f"Go binary marker: {path.name}")
        architecture = captured(["/usr/bin/lipo", "-archs", str(path)], env).strip()
        require(architecture == "arm64", f"unexpected architectures: {architecture}")
        loads = captured(["/usr/bin/otool", "-l", str(path)], env)
        libraries = captured(["/usr/bin/otool", "-L", str(path)], env)
        relative = path.relative_to(app).as_posix()
        # Skip dylib's first entry (LC_ID_DYLIB), which is identity, not a dependency.
        deps = [line.strip().split(" (", 1)[0] for line in libraries.splitlines()[1:]]
        if path.suffix == ".dylib":
            deps = deps[1:]
        for dependency in deps:
            allowed = dependency.startswith(("/usr/lib/", "/System/Library/"))
            if dependency == "@loader_path/libusb-1.0.0.dylib":
                allowed = (path.parent / "libusb-1.0.0.dylib").is_file()
            require(allowed, f"non-system or unresolved dependency needs audit: {relative}: {dependency}")
        minimum = minimum_macos(loads)
        if relative == "Contents/Resources/backend/libusb-1.0.0.dylib":
            require(minimum <= (27, 0, 0), "libusb deployment exceeds macOS 27.0")
        else:
            require(minimum == (27, 0, 0), f"expected macOS 27.0 deployment command: {relative}")
        binaries.append({"path": relative, "sha256": sha(path), "architecture": architecture, "minimum_macos": list(minimum), "dependencies": deps, "load_commands": loads})
    require({item["path"] for item in binaries} == expected, "unexpected or missing Mach-O executable in bundle")
    source_digest = captured([str(capsule / "tools/python3"), "scripts/source-identity.py"], env,
                             cwd=capsule / "src").strip()
    require(re.fullmatch(r"[0-9a-f]{64}", source_digest) is not None, "cold-build source identity is malformed")
    backend_strings = captured(["/usr/bin/strings", str(backend / "switchd-rs")], env)
    require(source_digest in backend_strings, "embedded Rust backend source digest differs from cold source export")
    prepared = subprocess.run([str(capsule / "tools/python3"), "scripts/prepare-module-supervisor.py"],
                              cwd=capsule / "src", env=env, capture_output=True, text=True, check=True)
    supervisor_output = capsule / "build/module-supervisor"
    supervisor_manifest_path = supervisor_output / "manifest.json"
    supervisor_artifact = supervisor_output / "switch-voice-supervisor.armv7"
    require(supervisor_output.is_dir() and not supervisor_output.is_symlink(), "cold-build supervisor output missing")
    require(supervisor_manifest_path.is_file() and not supervisor_manifest_path.is_symlink(), "cold-build supervisor manifest missing")
    require(supervisor_artifact.is_file() and not supervisor_artifact.is_symlink(), "cold-build supervisor artifact missing")
    supervisor_manifest = json.loads(supervisor_manifest_path.read_text())
    supervisor_sources = {}
    fixed = ["rust/Cargo.toml", "rust/Cargo.lock", "scripts/build-module-supervisor.py", "scripts/prepare-module-supervisor.py"]
    crate = capsule / "src/rust/crates/switch-voice-supervisor"
    source_paths = [capsule / "src" / name for name in fixed] + [path for path in crate.rglob("*") if path.is_file()]
    for path in sorted(set(source_paths)):
        require(not path.is_symlink(), "linked module-supervisor source")
        supervisor_sources[path.relative_to(capsule / "src").as_posix()] = sha(path)
    require(supervisor_manifest.get("source_files") == supervisor_sources, "cold-build supervisor source identity mismatch")
    require(supervisor_manifest.get("sha256") == sha(supervisor_artifact), "cold-build supervisor artifact digest mismatch")
    sysroot_manifest = capsule / "build/toolchains/rust-1.98.0-armv7-musleabi/source-manifest.json"
    require(supervisor_manifest.get("sysroot_manifest_sha256") == sha(sysroot_manifest), "cold-build supervisor ARM std provenance mismatch")
    supervisor = {"path": str(supervisor_artifact), "sha256": sha(supervisor_artifact),
                  "source_files": len(supervisor_sources), "arm_std_archive_sha256": ARM_STD_ARCHIVE_SHA256,
                  "arm_std_validation": json.loads(prepared.stdout)}
    signature = captured(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], env)
    output = {"status": "passed", "audit_sha256": sha(capsule / "audit.json"), "execution_sha256": sha(capsule / "execution.json"), "app": str(app), "app_bundle": app_bundle_inventory(app, inventory_root=capsule), "source_digest": source_digest, "binaries": binaries, "module_supervisor": supervisor, "signature": signature, "go_markers_checked": list(GO_MARKERS), "software_tests": execution.get("software_tests", "not_recorded"), "recovery_process_gate": recovery_evidence, "boundaries": manifest["boundaries"]}
    write_json(capsule / "artifact-audit.json", output)
    print(str(capsule / "artifact-audit.json"))


def dependency_self_test(private):
    """Run the real prepare/dependencies path against inert synthetic inputs.

    Only the existing ARM compiler-archive pin is substituted with a tiny test
    archive; the public libusb notice pin is never substituted. No fixture
    compiler/tool can execute silently: each writes a sentinel then fails.
    """
    public_notice = source_libusb_notice().read_bytes()
    source = private / "dependency-source"
    build = private / "dependency-build"
    capsules = build / "clean-build"
    tools = private / "dependency-tools"
    tools.mkdir()
    sentinel = private / "dependency-tool-was-executed"
    inert = '#!/bin/sh\n: > "' + str(sentinel) + '"\nexit 91\n'
    for name in ("rustup", "xcodegen", "pkg-config"):
        path = tools / name
        path.write_text(inert)
        path.chmod(0o700)
    developer = private / "Xcode.app/Contents/Developer"
    (developer / "Platforms/MacOSX.platform/Developer/SDKs/MacOSX27.sdk").mkdir(parents=True)
    (developer.parent / "Info.plist").write_text("synthetic Xcode identity; never executed\n")
    rustup = private / "synthetic-rustup"
    toolchain = rustup / "toolchains/1.98.0-aarch64-apple-darwin"
    (toolchain / "bin").mkdir(parents=True)
    for name in ("cargo", "rustc", "rustdoc", "cargo-fmt", "rustfmt"):
        (toolchain / "bin" / name).write_text(inert)
        (toolchain / "bin" / name).chmod(0o700)
    stdlib = toolchain / "lib/rustlib/aarch64-apple-darwin/lib/libstd-synthetic.rlib"
    stdlib.parent.mkdir(parents=True)
    stdlib.write_bytes(b"synthetic stdlib; not linked")
    libusb = private / "synthetic-libusb/libusb.dylib"
    libusb.parent.mkdir()
    libusb.write_bytes(b"synthetic libusb; not linked")
    pkgconfig = libusb.parent / "pkgconfig"
    pkgconfig.mkdir()
    (pkgconfig / "libusb-1.0.pc").write_text("synthetic pkgconfig; not executed\n")
    archive = build / ARM_STD_ARCHIVE_REL.relative_to("build")
    archive.parent.mkdir(parents=True)
    archive.write_bytes(b"synthetic ARM std archive; not unpacked")
    notice = source / "licenses/libusb-COPYING"
    notice.parent.mkdir(parents=True)
    notice.write_bytes(public_notice)
    (source / "rust").mkdir()
    (source / "rust/Cargo.lock").write_text("version = 4\n")
    # The actual prepare source inventory uses git; this is only an empty
    # private index, with all synthetic source files enumerated as untracked.
    git_template = private / "empty-git-template"
    git_template.mkdir()
    git_env = {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
               "GIT_TEMPLATE_DIR": str(git_template)}
    subprocess.run(["/usr/bin/git", "init", "--quiet", str(source)],
                   env=dict(os.environ, **git_env), check=True)
    cache = private / "synthetic-cargo-cache"
    for name in ("cache", "index"):
        folder = cache / "registry" / name / "synthetic"
        folder.mkdir(parents=True)
        (folder / ("tiny.crate" if name == "cache" else "metadata")).write_bytes(b"synthetic downloaded input")
    foreign = private / "foreign-license.json"
    foreign.write_text('{"enabled":true,"telegram":{"bot_token":"synthetic-bare-token"},"bark":{"device_key":"synthetic-bare-key"},"message":"synthetic SMS"}')
    args = argparse.Namespace(developer_dir=str(developer), rustup_home=str(rustup),
                              libusb=str(libusb), libusb_pkgconfig=str(pkgconfig),
                              cargo_cache=str(cache), min_free_gib=20, run_id="prepare-positive")
    with mock.patch.multiple(sys.modules[__name__], ROOT=source, BUILD=build,
                             CAPSULES=capsules, ARM_STD_ARCHIVE_SHA256=sha(archive)), \
            mock.patch.dict(os.environ, dict(git_env, PATH=str(tools), LIBUSB_LICENSE_FILE=str(foreign))):
        dep = dependencies(args)  # Same function prepare calls; no SOURCE injection.
        require(dep["toolchain_identity"] == toolchain_identity(toolchain), "dependencies lost real sysroot inventory")
        require(str(foreign) not in dep["input_hashes"] and "libusb_license" not in dep,
                "external license became a dependency")
        env = clean_environment(private / "synthetic-capsule", dep)
        require("LIBUSB_LICENSE_FILE" not in env, "external license entered clean environment")
        require(not sentinel.exists(), "dependencies executed a synthetic compiler/tool")
        print("PASS clean-dependencies positive and hostile environment ignored (real dependencies function)")
        prepare(args)  # Real source export/dependencies/cache/validate, no execute.
        capsule = capsules / args.run_id
        require((capsule / "audit.json").is_file() and not (capsule / "execution.json").exists(),
                "prepare did not produce an unexecuted capsule")
        require("LIBUSB_LICENSE_FILE" not in clean_environment(capsule, dep), "prepare reintroduced license environment")
        require(not sentinel.exists(), "prepare executed a synthetic compiler/tool")
        print("PASS clean-dependencies prepare-positive (real prepare path, no compiler/tool execution)")
        for name in ("missing", "tampered", "symlink"):
            notice.unlink()
            if name == "tampered":
                notice.write_bytes(bytes([public_notice[0] ^ 1]) + public_notice[1:])
            elif name == "symlink":
                notice.symlink_to(foreign)
            args.run_id = "blocked-" + name
            try:
                prepare(args)
            except ValueError as error:
                # source_inventory itself rejects the symlink before dependencies.
                expected = "source symlink requires explicit audit" if name == "symlink" else "source-pinned libusb notice missing or changed"
                require(expected in str(error), "prepare pin refusal occurred for an unrelated reason")
            else:
                raise AssertionError("prepare accepted " + name + " source notice")
            require(not (capsules / args.run_id).exists() and not sentinel.exists(),
                    "rejected prepare created a capsule or executed a build tool")
            # Direct dependencies must independently refuse, including symlink.
            try:
                dependencies(args)
            except ValueError as error:
                require("source-pinned libusb notice missing or changed" in str(error),
                        "dependencies pin refusal occurred for an unrelated reason")
            else:
                raise AssertionError("dependencies accepted " + name + " source notice")
            print("PASS clean-dependencies " + name + " refused before capsule/build (prepare and dependencies)")
            if notice.is_symlink() or notice.exists():
                notice.unlink()
            notice.write_bytes(public_notice)
        require(not sentinel.exists(), "a synthetic compiler/tool was executed")


def self_test():
    # Negative checks exercise the audit itself, with no Cargo/Xcode invocation.
    require(minimum_macos("Load command 0\n cmd LC_BUILD_VERSION\n minos 27.1\n") == (27, 1, 0), "deployment version prefix ambiguity")
    require(minimum_macos("Load command 0\n cmd LC_VERSION_MIN_MACOSX\n version 11.0\n") == (11, 0, 0), "legacy deployment command parsing failed")
    BUILD.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="clean-build-audit-test-", dir=BUILD) as temporary:
        root = Path(temporary)
        dependency_self_test(root)
        observed_cwd = captured(["/bin/pwd"], os.environ.copy(), cwd=root).strip()
        require(Path(observed_cwd).resolve() == root.resolve(), "captured command ignored its requested source root")
        sample = root / "source.rs"
        sample.write_text("fn main() {}\n")
        initial = source_entry(root, Path("source.rs"))
        sample.write_text("fn changed() {}\n")
        require(source_entry(root, Path("source.rs"))["sha256"] != initial["sha256"], "mutation not detected")
        app_fixture = root / "Inventory.app"
        resource = app_fixture / "Contents/Resources/example.json"
        resource.parent.mkdir(parents=True)
        resource.write_text('{"value":1}\n')
        inventory = app_bundle_inventory(app_fixture)
        require(inventory["path"] == app_fixture.relative_to(ROOT.parent).as_posix(),
                "default artifact App inventory path changed")
        capsule_fixture = root / "inventory-capsule"
        capsule_app = capsule_fixture / "dist/Switch.app"
        capsule_app.parent.mkdir(parents=True)
        shutil.copytree(app_fixture, capsule_app)
        require(app_bundle_inventory(capsule_app, inventory_root=capsule_fixture)["path"]
                == "dist/Switch.app", "capsule-relative artifact App inventory path is incorrect")
        (capsule_fixture / "escape").symlink_to(app_fixture.parent, target_is_directory=True)
        for escaped_app in (app_fixture, capsule_fixture / "escape/Inventory.app"):
            try:
                app_bundle_inventory(escaped_app, inventory_root=capsule_fixture)
            except ValueError as error:
                require("outside its trusted inventory root" in str(error),
                        "artifact App path confinement failed for an unrelated reason")
            else:
                raise AssertionError("artifact App outside its trusted inventory root accepted")
        require(inventory["file_count"] == 1,
                "artifact App inventory omitted a resource")
        require(inventory["directory_count"] == 3,
                "artifact App inventory omitted a directory")
        resource.write_text('{"value":2}\n')
        require(app_bundle_inventory(app_fixture) != inventory,
                "artifact App resource mutation was not detected")
        resource.write_text('{"value":1}\n')
        resource.chmod(0o700)
        require(app_bundle_inventory(app_fixture) != inventory,
                "artifact App resource mode mutation was not detected")
        resource.chmod(0o644)
        empty = app_fixture / "Contents/Empty"
        empty.mkdir()
        require(app_bundle_inventory(app_fixture) != inventory,
                "artifact App empty directory was not detected")
        for name, data in (("native.a", b"archive"), ("hidden", b"\xcf\xfa\xed\xfebinary"), ("legacy.go", b"package main")):
            (root / name).write_bytes(data)
            try:
                source_entry(root, Path(name))
            except ValueError:
                pass
            else:
                raise AssertionError("forbidden input accepted")
        (root / "escape").symlink_to(sample)
        try:
            source_entry(root, Path("escape"))
        except ValueError:
            pass
        else:
            raise AssertionError("symlink accepted")
        (root / "go").write_text("#!/bin/sh\nexit 99\n")
        (root / "go").chmod(0o755)
        try:
            assert_no_go(str(root))
        except ValueError:
            pass
        else:
            raise AssertionError("Go PATH contamination accepted")
        saved_identity = {key: os.environ.get(key) for key in
                          ("USER", "LOGNAME", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG", "LIBUSB_LICENSE_FILE")}
        try:
            os.environ.update(USER="injected-user", LOGNAME="injected-logname",
                              CARGO_PROFILE_DEV_DEBUG="2", CARGO_PROFILE_TEST_DEBUG="2",
                              LIBUSB_LICENSE_FILE="/synthetic/untrusted-license.json")
            env = clean_environment(root, {"developer_dir": "/xcode", "rustup_home": "/rust", "libusb": "/usb", "libusb_pkgconfig": "/pc"})
            require(env["USER"] == env["LOGNAME"] == pwd.getpwuid(os.getuid()).pw_name, "caller login-name injection survived")
            require(env["CARGO_PROFILE_DEV_DEBUG"] == env["CARGO_PROFILE_TEST_DEBUG"] == "0",
                    "caller debug profile injection survived")
            require("LIBUSB_LICENSE_FILE" not in env, "external libusb notice authority survived")
            require(not any(key.endswith(("OPT_LEVEL", "DEBUG_ASSERTIONS", "OVERFLOW_CHECKS"))
                            for key in env), "runtime/optimization profile defaults changed")
            require(len(env["TMPDIR"]) < 32 and Path(env["TMPDIR"]).is_dir(), "macOS test temp root must be short and available")
        finally:
            for key, value in saved_identity.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
        require(not set(env) & {"RUSTFLAGS", "RUSTC_WRAPPER", "SCCACHE_DIR", "CC", "CFLAGS", "DYLD_LIBRARY_PATH"}, "compiler injection survived")
        require(generated(Path("rust/target/debug/app")) and generated(Path("Switch.xcodeproj/project.pbxproj")), "generated output not excluded")
        fixture = root / "capsule"
        for folder in ("src", "tools", "build/cargo-home"):
            (fixture / folder).mkdir(parents=True, exist_ok=True)
        code = fixture / "src/example.rs"
        code.write_text("fn main() {}\n")
        global ARM_STD_ARCHIVE_SHA256
        original_arm_archive_sha256 = ARM_STD_ARCHIVE_SHA256
        arm_archive = fixture / ARM_STD_ARCHIVE_REL
        arm_archive.parent.mkdir(parents=True, exist_ok=True)
        arm_archive.write_bytes(b"test-only pinned archive fixture")
        ARM_STD_ARCHIVE_SHA256 = sha(arm_archive)
        dep = {"developer_dir": "/xcode", "rustup_home": "/rust", "libusb": "/usb", "libusb_pkgconfig": "/pc", "tools": {}, "input_hashes": {str(arm_archive): ARM_STD_ARCHIVE_SHA256}}
        sysroot = root / "fixture-sysroot"
        (sysroot / "bin").mkdir(parents=True)
        (sysroot / "lib/rustlib/aarch64-apple-darwin/lib").mkdir(parents=True)
        for name in ("cargo", "rustc", "rustdoc"):
            (sysroot / "bin" / name).write_text("fake compiler input, never executed\n")
        stdlib = sysroot / "lib/rustlib/aarch64-apple-darwin/lib/libstd-fixture.rlib"
        stdlib.write_bytes(b"fake stdlib input")
        identity = toolchain_identity(sysroot)
        require(identity["metadata_files"] == [], "manifest-free sysroot fixture unexpectedly has metadata")
        metadata = sysroot / "lib/rustlib/multirust-channel-manifest.toml"
        metadata.write_text("fixture installer metadata\n")
        require("lib/rustlib/multirust-channel-manifest.toml" in toolchain_identity(sysroot)["metadata_files"], "actual rustup metadata layout not detected")
        dep.update(toolchain=str(sysroot), toolchain_identity=toolchain_identity(sysroot))
        manifest = {"capsule": str(fixture), "source": [source_entry(fixture / "src", Path("example.rs"))], "dependencies": dep, "cache": [],
                    "arm_std_archive": {"path": ARM_STD_ARCHIVE_REL.as_posix(), "sha256": ARM_STD_ARCHIVE_SHA256}}
        write_json(fixture / "audit.json", manifest)
        validate(fixture, fresh=True)
        def rejected(operation, undo):
            operation()
            try:
                validate(fixture, fresh=True)
            except ValueError:
                pass
            else:
                raise AssertionError("contaminated capsule accepted")
            finally:
                undo()
        rejected(lambda: stdlib.write_bytes(b"changed stdlib"), lambda: stdlib.write_bytes(b"fake stdlib input"))
        rejected(lambda: arm_archive.write_bytes(b"modified test archive"), lambda: arm_archive.write_bytes(b"test-only pinned archive fixture"))
        mode = code.stat().st_mode
        rejected(lambda: code.chmod(mode ^ stat.S_IXUSR), lambda: code.chmod(mode))
        rejected(lambda: code.write_text("changed\n"), lambda: code.write_text("fn main() {}\n"))
        stale = fixture / "build/rust-target"
        rejected(lambda: (stale.mkdir(), (stale / "old.o").write_bytes(b"old")), lambda: shutil.rmtree(stale))
        outside = root / "outside-target"
        outside.mkdir()
        rejected(lambda: stale.symlink_to(outside, target_is_directory=True), lambda: stale.unlink())
        distribution = fixture / "dist"
        rejected(lambda: distribution.symlink_to(outside, target_is_directory=True), lambda: distribution.unlink())
        stale_source = fixture / "src/target"
        rejected(lambda: stale_source.mkdir(), lambda: stale_source.rmdir())
        config = fixture / ".cargo"
        rejected(lambda: (config.mkdir(), (config / "config.toml").write_text('[build]\nrustflags = ["-Copt-level=0"]\n')), lambda: shutil.rmtree(config))
        rejected(lambda: write_json(fixture / "audit.json", dict(manifest, capsule="/wrong/export")), lambda: write_json(fixture / "audit.json", manifest))
        ARM_STD_ARCHIVE_SHA256 = original_arm_archive_sha256
    print("clean-build audit self-test passed (no build commands)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    prep = sub.add_parser("prepare", help="export current source and downloaded cache; no compiler/build invocation")
    prep.add_argument("--run-id")
    prep.add_argument("--cargo-cache", default=str(BUILD / "cargo-home"))
    prep.add_argument("--rustup-home", default=str(Path.home() / ".rustup"))
    prep.add_argument("--developer-dir", default="/Applications/Xcode.app/Contents/Developer")
    prep.add_argument("--libusb", default="/opt/homebrew/opt/libusb/lib/libusb-1.0.0.dylib")
    prep.add_argument("--libusb-pkgconfig", default="/opt/homebrew/opt/libusb/lib/pkgconfig")
    prep.add_argument("--min-free-gib", type=int, default=20)
    sub.add_parser("validate").add_argument("capsule")
    execute_parser = sub.add_parser("execute")
    execute_parser.add_argument("capsule")
    execute_parser.add_argument("--with-tests", action="store_true",
                                help="run the complete no-device migration gate in the fresh capsule before building the App")
    verify_parser = sub.add_parser("verify")
    verify_parser.add_argument("capsule")
    verify_parser.add_argument("--require-tests", action="store_true",
                               help="require a successful complete software-test receipt from this capsule")
    sub.add_parser("self-test")
    args = parser.parse_args()
    if args.action == "prepare":
        require(args.min_free_gib >= 20, "cold build disk threshold must be at least 20 GiB")
        prepare(args)
    elif args.action == "self-test":
        self_test()
    elif args.action == "execute":
        execute(capsule_path(args.capsule), with_tests=args.with_tests)
    elif args.action == "verify":
        verify(capsule_path(args.capsule), require_tests=args.require_tests)
    else:
        validate(capsule_path(args.capsule), fresh=True)
        print("fresh clean-build capsule validation passed")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"clean-build audit failed: {error}", file=sys.stderr)
        sys.exit(1)
