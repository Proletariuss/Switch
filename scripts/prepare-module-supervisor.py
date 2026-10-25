#!/usr/bin/env python3
"""Prepare one hash-pinned ARM soft-float Rust stdlib inside this workspace.

The default is offline validation. Network access requires --fetch; no toolchain
is installed into HOME, rustup, Xcode, or an existing application.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
TARGET = 'armv7-unknown-linux-musleabi'
VERSION = '1.98.0'
URL = 'https://static.rust-lang.org/dist/2026-08-20/rust-std-1.98.0-armv7-unknown-linux-musleabi.tar.xz'
SHA256 = '01503f49f85d80cc2d852e6278f0bb226e86bc20e94ef40c39bca4566b60c5a6'
SYSROOT = ROOT / 'build/toolchains/rust-1.98.0-armv7-musleabi'
ARCHIVE = ROOT / 'build/toolchains/downloads/rust-std-1.98.0-armv7-unknown-linux-musleabi.tar.xz'


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for part in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(part)
    return result.hexdigest()


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def verify():
    require(SYSROOT.is_dir() and not SYSROOT.is_symlink(),
            'ARM Rust std is missing; run scripts/prepare-module-supervisor.py --fetch once')
    manifest_path = SYSROOT / 'source-manifest.json'
    require(manifest_path.is_file() and not manifest_path.is_symlink(), 'ARM std manifest is missing')
    manifest = json.loads(manifest_path.read_text())
    require(manifest['url'] == URL and manifest['archive_sha256'] == SHA256
            and manifest['target'] == TARGET and manifest['rust_version'] == VERSION,
            'ARM std provenance does not match the pinned compiler component')
    actual = {}
    for path in sorted((SYSROOT / 'lib').rglob('*')):
        require(not path.is_symlink(), 'Linked entry in ARM sysroot')
        if path.is_file():
            actual[str(path.relative_to(SYSROOT))] = digest(path)
        else:
            require(path.is_dir(), 'Special file in ARM sysroot')
    require(actual == manifest['files'] and actual, 'ARM std component changed or is incomplete')
    names = [Path(name).name for name in actual]
    for prefix in ('libstd-', 'libcore-', 'libcompiler_builtins-'):
        require(any(name.startswith(prefix) and name.endswith('.rlib') for name in names),
                'Required ARM Rust standard library is missing: ' + prefix)
    require(any('/self-contained/' in name for name in actual), 'ARM musl CRT closure is missing')
    return manifest


def fetch_archive():
    ARCHIVE.parent.mkdir(parents=True, exist_ok=True)
    if ARCHIVE.exists():
        require(ARCHIVE.is_file() and not ARCHIVE.is_symlink() and digest(ARCHIVE) == SHA256,
                'Existing ARM component archive is not the pinned download; it was preserved')
        return
    descriptor, temporary_name = tempfile.mkstemp(prefix='rust-arm-download-', dir=ARCHIVE.parent)
    temporary = Path(temporary_name)
    try:
        total = 0
        # The default configured HTTPS/proxy path is retained. No DNS override,
        # alternate resolver, TLS bypass, or direct-network fallback is used.
        with os.fdopen(descriptor, 'wb') as output, urllib.request.urlopen(URL, timeout=60) as response:
            while True:
                part = response.read(1024 * 1024)
                if not part:
                    break
                total += len(part)
                require(total <= 100 * 1024 * 1024, 'ARM component download exceeded its bound')
                output.write(part)
            output.flush()
            os.fsync(output.fileno())
        require(digest(temporary) == SHA256, 'ARM component checksum mismatch; no installation performed')
        # A pre-existing name cannot be replaced even if another preparer raced.
        os.link(temporary, ARCHIVE)
        parent = os.open(ARCHIVE.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
    finally:
        temporary.unlink(missing_ok=True)


def install_component():
    SYSROOT.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix='rust-arm-component-', dir=SYSROOT.parent))
    files = {}
    total = 0
    marker = '/lib/rustlib/' + TARGET + '/lib/'
    try:
        with tarfile.open(ARCHIVE, 'r:xz') as archive:
            for member in archive:
                # Extract only the stdlib payload. Never run its install.sh or
                # unpack arbitrary archive paths, links, scripts, or devices.
                if marker not in member.name:
                    continue
                suffix = member.name.split(marker, 1)[1]
                parts = Path(suffix).parts
                require(parts and all(part not in ('..', '.') for part in parts)
                        and not Path(suffix).is_absolute(), 'Unsafe ARM component path')
                relative = Path('lib/rustlib') / TARGET / 'lib' / Path(*parts)
                destination = stage / relative
                if member.isdir():
                    destination.mkdir(parents=True, exist_ok=True)
                    continue
                require(member.isfile(), 'Unexpected linked or special ARM component entry')
                total += member.size
                require(total <= 512 * 1024 * 1024 and len(files) < 4096,
                        'ARM component expanded beyond its bound')
                require(str(relative) not in files, 'Duplicate ARM component payload')
                destination.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                require(source is not None, 'ARM component payload unreadable')
                with source, destination.open('xb') as output:
                    shutil.copyfileobj(source, output, 1024 * 1024)
                destination.chmod(0o644)
                require(destination.stat().st_size == member.size, 'Truncated ARM component file')
                files[str(relative)] = digest(destination)
        require(files, 'ARM component did not contain the expected target payload')
        manifest = dict(rust_version=VERSION, target=TARGET, url=URL,
                        archive_sha256=SHA256, files=files, bytes=total,
                        purpose='Offline module voice supervisor static ARM build dependency')
        (stage / 'source-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        require(not SYSROOT.exists() and not SYSROOT.is_symlink(), 'ARM sysroot already exists; preserved')
        stage.rename(SYSROOT)
        return verify()
    finally:
        if stage.exists():
            # Only this invocation's uninstalled staging dependency is removed.
            shutil.rmtree(stage)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fetch', action='store_true')
    args = parser.parse_args()
    if args.fetch and not SYSROOT.exists():
        fetch_archive()
        result = install_component()
    else:
        result = verify()
    print(json.dumps(dict(sysroot=str(SYSROOT), target=TARGET, rust_version=VERSION,
                          files=len(result['files']), bytes=result['bytes'],
                          archive_sha256=SHA256, verified=True)))


if __name__ == '__main__':
    main()
