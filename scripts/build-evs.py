#!/usr/bin/env python3
"""Build pinned EVS reference sources. All generated files stay in Switch/build."""
import concurrent.futures, contextlib, hashlib, json, os, pathlib, stat, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = root / 'third_party/evs'
out = root.parent / 'build/evs'
out.mkdir(parents=True, exist_ok=True)
flags = ['-Dsub=switch_evs_sub', '-DPow2=switch_evs_Pow2', '-O2', '-fPIC', '-fno-fast-math', '-arch', 'arm64', '-mmacosx-version-min=27.0', '-Wno-incompatible-pointer-types-discards-qualifiers']
flags += ['-I'+str(src/x) for x in ('lib_com','lib_enc','lib_dec')]
files = sorted(src.glob('lib_*/*.c')) + [src/'switch_evs.c']
headers = b''.join(p.read_bytes() for p in sorted(src.rglob('*.h')))
base = hashlib.sha256(headers + repr(flags).encode() + subprocess.check_output(['xcrun','clang','--version'])).digest()
def compile_one(p):
    name = p.relative_to(src).as_posix().replace('/','_')
    obj = out/(name+'.o')
    stamp = out/(name+'.sha256')
    digest = hashlib.sha256(base+p.read_bytes()).hexdigest()
    if not obj.exists() or not stamp.exists() or stamp.read_text() != digest:
        subprocess.run(['xcrun','clang',*flags,'-c',str(p),'-o',str(obj)],check=True,stdout=subprocess.DEVNULL)
        stamp.write_text(digest)
    return str(obj)
with concurrent.futures.ThreadPoolExecutor(max_workers=min(8, os.cpu_count() or 2)) as pool:
    objects = list(pool.map(compile_one,files))
archive = out / 'libswitch_evs.a'

@contextlib.contextmanager
def opened_archive(path, *, missing_ok=False, writable=False):
    try:
        named = path.lstat()
    except FileNotFoundError:
        if not missing_ok:
            raise
        yield None
        return
    if not stat.S_ISREG(named.st_mode):
        raise RuntimeError(f'EVS archive is not an ordinary file: {path}')
    access = os.O_RDWR if writable else os.O_RDONLY
    descriptor = os.open(path, access | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(descriptor, 'r+b' if writable else 'rb') as stream:
        opened = os.fstat(stream.fileno())
        if not stat.S_ISREG(opened.st_mode) or (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino):
            raise RuntimeError(f'EVS archive changed while opening: {path}')
        yield stream, opened
        current = path.lstat()
        finished = os.fstat(stream.fileno())
        def stamp(value):
            return (value.st_dev, value.st_ino, value.st_mode, value.st_nlink,
                    value.st_size, value.st_mtime_ns, value.st_ctime_ns)
        if stamp(opened) != stamp(finished) or stamp(finished) != stamp(current):
            raise RuntimeError(f'EVS archive changed while publishing: {path}')

def digest_stream(stream):
    digest = hashlib.sha256()
    while True:
        chunk = stream.read(1024 * 1024)
        if not chunk:
            return digest.hexdigest()
        digest.update(chunk)

def digest_file(path):
    with opened_archive(path) as opened:
        return digest_stream(opened[0])

def sync_parent():
    parent = os.open(out, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        os.fsync(parent)
    finally:
        os.close(parent)

# Builds are serialized in this workspace; this is not a cross-user isolation boundary.
if out.is_symlink() or not out.is_dir():
    raise RuntimeError('EVS output directory is not an ordinary directory')
libtool = pathlib.Path(subprocess.check_output(['xcrun','--find','libtool'], text=True).strip()).resolve(strict=True)
inputs = {
    'objects': [{'name': pathlib.Path(obj).relative_to(out).as_posix(),
                 'sha256': digest_file(pathlib.Path(obj))} for obj in objects],
    'libtool': {'path': str(libtool), 'sha256': digest_file(libtool)},
    'static_args': ['-static'],
    'output_name': archive.name,
    'script_sha256': digest_file(pathlib.Path(__file__).resolve()),
}
manifest = out / 'archive-manifest.json'
cached = None
with opened_archive(manifest, missing_ok=True) as existing:
    if existing is not None:
        raw = existing[0].read(1024 * 1024 + 1)
        if len(raw) <= 1024 * 1024:
            try:
                cached = json.loads(raw)
            except (ValueError, UnicodeError):
                pass
cache_hit = False
with opened_archive(archive, missing_ok=True) as existing:
    if existing is not None and isinstance(cached, dict):
        cache_hit = cached == {'version': 1, 'inputs': inputs,
                               'archive_sha256': digest_stream(existing[0])}
if cache_hit:
    # Retry directory durability even after a prior publication's fsync failure.
    sync_parent()
else:
    with tempfile.TemporaryDirectory(prefix='evs-archive-', dir=out) as staging:
        staging = pathlib.Path(staging)
        temporary = staging / 'libswitch_evs.a'
        subprocess.run(['xcrun','libtool','-static','-o',str(temporary),*objects],check=True)
        same = False
        with opened_archive(temporary, writable=True) as candidate:
            new, new_stat = candidate
            archive_digest = digest_stream(new)
            new.seek(0)
            with opened_archive(archive, missing_ok=True) as existing:
                if existing is not None:
                    previous, previous_stat = existing
                    if new_stat.st_size == previous_stat.st_size:
                        same = True
                        while True:
                            chunk = new.read(1024 * 1024)
                            if chunk != previous.read(1024 * 1024):
                                same = False
                                break
                            if not chunk:
                                break
            if not same:
                os.fsync(new.fileno())
        if not same:
            with opened_archive(archive, missing_ok=True):
                pass
            os.replace(temporary, archive)
        sync_parent()
        content = (json.dumps({'version': 1, 'inputs': inputs,
                               'archive_sha256': archive_digest},
                              sort_keys=True, separators=(',', ':')) + '\n').encode()
        with opened_archive(manifest, missing_ok=True) as existing:
            manifest_same = existing is not None and existing[0].read(len(content) + 1) == content
        if not manifest_same:
            temporary_manifest = staging / 'archive-manifest.json'
            with temporary_manifest.open('xb') as stream:
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
            with opened_archive(manifest, missing_ok=True):
                pass
            os.replace(temporary_manifest, manifest)
        sync_parent()
        # Remove only this invocation's private staging directory, never the published files.
print(f'EVS: {len(objects)} objects -> {out / "libswitch_evs.a"}')
