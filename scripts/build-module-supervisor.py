#!/usr/bin/env python3
"""Build and audit the Rust module helper owner; never deploy or run it."""
import hashlib
import json
import os
import stat
from pathlib import Path
import struct
import subprocess
import sys

SOURCE = Path(__file__).resolve().parents[1]
WORKSPACE = SOURCE.parent
TARGET = 'armv7-unknown-linux-musleabi'
VERSION = '1.98.0'
SYSROOT = WORKSPACE / 'build/toolchains/rust-1.98.0-armv7-musleabi'
OUTPUT = WORKSPACE / 'build/module-supervisor'
TARGET_DIR = WORKSPACE / 'build/module-supervisor-target'
MAX_BYTES = 1_100_000


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def audit_elf(data):
    require(52 <= len(data) <= MAX_BYTES, 'ARM supervisor exceeds the artifact bound')
    require(data[:7] == b'\x7fELF\x01\x01\x01', 'Expected ELF32 little-endian ARM executable')
    header = struct.unpack_from('<HHIIIIIHHHHHH', data, 16)
    kind, machine, version, entry, phoff, shoff, flags, ehsize, phsize, phnum, _, _, _ = header
    require(kind == 2 and machine == 40 and version == 1 and ehsize == 52 and entry != 0,
            'Expected static ARM ET_EXEC')
    require(flags & 0xff000000 == 0x05000000 and flags & 0x200 != 0 and flags & 0x400 == 0,
            'Expected EABI5 soft-float ABI, not hard-float')
    require(phsize == 32 and 0 < phnum <= 64 and phoff + phnum * phsize <= len(data),
            'Invalid ARM program header table')
    kinds = []
    for index in range(phnum):
        item = struct.unpack_from('<IIIIIIII', data, phoff + index * phsize)
        require(item[1] + item[4] <= len(data), 'ARM segment exceeds artifact')
        require(item[0] not in (2, 3), 'ARM supervisor must not need a dynamic loader or shared libraries')
        kinds.append(item[0])
    require(1 in kinds, 'ARM supervisor has no load segment')
    return dict(format='ELF32', machine='ARM', type='ET_EXEC', abi='EABI5 soft-float',
                dynamic_loader=False, dynamic_segment=False, bytes=len(data), program_headers=phnum)


def source_files():
    fixed = ['rust/Cargo.toml', 'rust/Cargo.lock', 'scripts/build-module-supervisor.py',
             'scripts/prepare-module-supervisor.py']
    crate = SOURCE / 'rust/crates/switch-voice-supervisor'
    paths = [SOURCE / name for name in fixed] + [p for p in crate.rglob('*') if p.is_file()]
    require(paths and all(p.is_file() and not p.is_symlink() for p in paths),
            'Supervisor source must be ordinary files')
    return {str(p.relative_to(SOURCE)): digest(p) for p in sorted(set(paths))}


def same_output(path, expected, mode=None):
    """Read an existing ordinary output without following a named link.

    A successful comparison does not rewrite, chmod or touch its timestamps.
    Unexpected shapes/permissions fail instead of silently replacing evidence.
    """
    try:
        named = path.lstat()
    except FileNotFoundError:
        return False
    require(stat.S_ISREG(named.st_mode), 'Existing supervisor output is not an ordinary file')
    if mode is not None:
        require(stat.S_IMODE(named.st_mode) == mode, 'Existing supervisor artifact mode is unsafe')
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
    descriptor = os.open(path, flags)
    with os.fdopen(descriptor, 'rb') as stream:
        opened = os.fstat(stream.fileno())
        require(stat.S_ISREG(opened.st_mode)
                and (opened.st_dev, opened.st_ino) == (named.st_dev, named.st_ino),
                'Supervisor output name changed while opening')
        if mode is not None:
            require(stat.S_IMODE(opened.st_mode) == mode, 'Supervisor artifact mode changed')
        actual = stream.read(len(expected) + 1) if opened.st_size == len(expected) else None
        finished = os.fstat(stream.fileno())
        current = path.lstat()
        def stamp(value):
            return (value.st_dev, value.st_ino, value.st_mode, value.st_nlink,
                    value.st_size, value.st_mtime_ns, value.st_ctime_ns)
        require(stamp(opened) == stamp(finished) == stamp(current),
                'Supervisor output changed while comparing')
        return actual == expected


def replace_output(path, data, mode=None):
    temporary = OUTPUT / (path.name + '-' + str(os.getpid()) + '.tmp')
    with temporary.open('xb') as stream:
        stream.write(data)
        stream.flush()
        if mode is not None:
            os.fchmod(stream.fileno(), mode)
        os.fsync(stream.fileno())
    # Recheck shape before the existing atomic replacement; do not tolerate
    # a link substituted during preparation. Workspace builds are serialized.
    try:
        current = path.lstat()
    except FileNotFoundError:
        current = None
    require(current is None or stat.S_ISREG(current.st_mode),
            'Supervisor output name became unsafe before publication')
    os.replace(temporary, path)


def main():
    # Validate the pinned, workspace-local component before invoking cargo.
    subprocess.run([sys.executable, str(SOURCE / 'scripts/prepare-module-supervisor.py')], check=True)
    env = os.environ.copy()
    rustup = Path(env.get('RUSTUP_HOME', str(Path.home() / '.rustup')))
    host = rustup / ('toolchains/' + VERSION + '-aarch64-apple-darwin')
    cargo = host / 'bin/cargo'
    linker = host / 'lib/rustlib/aarch64-apple-darwin/bin/rust-lld'
    require(cargo.is_file() and linker.is_file(), 'The pinned installed Rust compiler/linker is missing')
    env.update(RUSTUP_HOME=str(rustup), RUSTUP_TOOLCHAIN=VERSION, RUSTC=str(host / 'bin/rustc'),
               CARGO_HOME=str(WORKSPACE / 'build/cargo-home'), CARGO_TARGET_DIR=str(TARGET_DIR),
               CARGO_INCREMENTAL='0', CARGO_PROFILE_RELEASE_PANIC='abort',
               DYLD_LIBRARY_PATH=str(host / 'lib'),
               CARGO_PROFILE_RELEASE_DEBUG='0',
               CFFIXED_USER_HOME=str(WORKSPACE / 'build/test-home'),
               SWITCH_DATA_DIR=str(WORKSPACE / 'build/runtime'))
    flags = ['--sysroot=' + str(SYSROOT), '-C', 'linker=' + str(linker), '-C',
             'linker-flavor=ld.lld', '-C', 'link-self-contained=yes', '-C',
             'target-feature=+crt-static', '-C', 'relocation-model=static']
    env.pop('RUSTFLAGS', None)
    env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(flags)
    before = source_files()
    subprocess.run([str(cargo), 'build', '--locked', '--offline', '--release',
                    '--manifest-path', str(SOURCE / 'rust/Cargo.toml'),
                    '-p', 'switch-voice-supervisor', '--bin', 'switch-voice-supervisor',
                    '--target', TARGET], env=env, check=True)
    require(source_files() == before, 'Supervisor source changed during its build; artifact not published')
    binary = TARGET_DIR / TARGET / 'release/switch-voice-supervisor'
    data = binary.read_bytes()
    elf = audit_elf(data)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    require(not OUTPUT.is_symlink() and OUTPUT.is_dir(), 'Supervisor output directory is unsafe')
    artifact = OUTPUT / 'switch-voice-supervisor.armv7'
    manifest_path = OUTPUT / 'manifest.json'
    manifest = dict(schema=1, artifact=artifact.name, sha256=hashlib.sha256(data).hexdigest(),
                    rust_version=VERSION, target=TARGET, source_files=before,
                    sysroot_manifest_sha256=digest(SYSROOT / 'source-manifest.json'),
                    rustflags=flags, elf=elf,
                    validation_boundary='Compiled and statically audited; not a physical module execution')
    manifest_data = (json.dumps(manifest, indent=2) + '\n').encode('utf-8')
    # Validate both existing names before changing either. Exact byte identity
    # keeps both inode/mtime values intact, so daemon Cargo stays incremental.
    artifact_same = same_output(artifact, data, mode=0o755)
    manifest_same = same_output(manifest_path, manifest_data)
    if not artifact_same:
        replace_output(artifact, data, mode=0o755)
    if not manifest_same:
        replace_output(manifest_path, manifest_data)
    # Retry directory durability even when a prior rename succeeded but its
    # parent fsync failed. This does not replace/touch either identical file.
    parent = os.open(OUTPUT, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(parent)
    finally:
        os.close(parent)
    print(json.dumps(dict(artifact=str(artifact), sha256=manifest['sha256'], elf=elf,
                          source_files=len(before), validated=True)))


if __name__ == '__main__':
    main()
