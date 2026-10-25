#!/usr/bin/env python3
"""Build/run an isolated EVS allocator-fault archive; never touch production EVS.

Examples (the owner starts these sequentially, not from a build hook):
  ... build --source third_party/evs --out-dir ../build/native-codec-allocation/after
  ... enumerate --out-dir ../build/native-codec-allocation/after
  ... all --out-dir ../build/native-codec-allocation/after --control-archive BASELINE.a
  ... before-proof --source BASELINE_SOURCE --out-dir BEFORE --fail-at N --expect-unsafe

`before-proof` records an unsafe result separately. It is not a passing fault
test. No environment variable or process-wide allocator interposition exists.
"""
import argparse
import datetime
import hashlib
import json
import pathlib
import re
import resource
import signal
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKSPACE = ROOT.parent
BUILD = WORKSPACE / "build"
FAULT_BUILD = BUILD / "native-codec-allocation"
SHIM = ROOT / "scripts/native-codec-allocation"
FIXTURES = ROOT / "rust/crates/proletarius-call/testdata/evs"
MODES = tuple(range(-1, 9))
SANDBOX_PROFILE = None
BASE_FLAGS = ["-std=c11", "-O2", "-fPIC", "-fno-fast-math", "-arch", "arm64",
              "-mmacosx-version-min=27.0", "-Wno-incompatible-pointer-types-discards-qualifiers"]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)


def bounded_path(raw, parent):
    path = pathlib.Path(raw).resolve()
    path.relative_to(parent.resolve())
    return path


def inputs(source):
    files = sorted(source.glob("lib_*/*.c")) + [source / "switch_evs.c"]
    headers = sorted(source.rglob("*.h"))
    instrumentation = sorted(SHIM.glob("*.[ch]"))
    for path in files + headers + instrumentation:
        path.resolve().relative_to(WORKSPACE)
    if not files or not all(path.is_file() for path in files):
        raise RuntimeError("missing pinned native sources")
    # A new allocator family must be explicitly instrumented before claiming a
    # complete constructor denominator. Current pinned sources use malloc/calloc.
    unsupported = re.compile(rb"\b(realloc|aligned_alloc|posix_memalign|strdup|strndup)\s*\(")
    for path in files:
        if unsupported.search(path.read_bytes()):
            raise RuntimeError(f"uncovered native allocator family: {path}")
    return files, {str(path): sha(path) for path in files + headers + instrumentation}


def execute(command):
    subprocess.run([str(part) for part in command], check=True)


def build(source, out):
    files, frozen_inputs = inputs(source)
    objects = bounded_path(out / "objects", FAULT_BUILD)
    evs = bounded_path(out / "evs", FAULT_BUILD)
    objects.mkdir(parents=True, exist_ok=True)
    evs.mkdir(parents=True, exist_ok=True)
    version = subprocess.check_output(["xcrun", "clang", "--version"]).decode()
    flags = BASE_FLAGS + ["-Dsub=switch_evs_sub", "-DPow2=switch_evs_Pow2"]
    flags += ["-I" + str(source / part) for part in ("lib_com", "lib_enc", "lib_dec")]
    flags += ["-include", str(SHIM / "force_include.h")]
    common = hashlib.sha256(json.dumps([frozen_inputs, version, flags], sort_keys=True).encode()).hexdigest()
    native_objects = []
    for path in files:
        name = path.relative_to(source).as_posix().replace("/", "_")
        obj = bounded_path(objects / (name + ".o"), FAULT_BUILD)
        stamp = bounded_path(objects / (name + ".sha256"), FAULT_BUILD)
        digest = hashlib.sha256((common + frozen_inputs[str(path)]).encode()).hexdigest()
        if not obj.is_file() or not stamp.is_file() or stamp.read_text() != digest:
            execute(["xcrun", "clang", *flags, "-c", path, "-o", obj])
            stamp.write_text(digest)
        native_objects.append(obj)
    shim_object = bounded_path(objects / "allocation_shim.o", FAULT_BUILD)
    execute(["xcrun", "clang", *BASE_FLAGS, "-I" + str(SHIM), "-c",
             SHIM / "allocation_shim.c", "-o", shim_object])
    archive = bounded_path(evs / "libswitch_evs.a", FAULT_BUILD)
    execute(["xcrun", "libtool", "-static", "-o", archive, *native_objects, shim_object])
    harness = bounded_path(out / "native-codec-allocation-harness", FAULT_BUILD)
    execute(["xcrun", "clang", *BASE_FLAGS, "-I" + str(SHIM), "-I" + str(source),
             SHIM / "fault_harness.c", archive, "-lm", "-lpthread", "-o", harness])
    if inputs(source)[1] != frozen_inputs:
        raise RuntimeError("source changed while building isolated native archive")
    manifest = {"source": str(source), "source_inputs": frozen_inputs,
                "compiler": version, "flags": flags, "archive": str(archive),
                "archive_sha256": sha(archive), "harness": str(harness),
                "harness_sha256": sha(harness), "production_archive_modified": False}
    save(out / "build-manifest.json", manifest)
    return manifest


def disable_core():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def child(binary, arguments, timeout):
    command = [str(binary), *map(str, arguments)]
    if SANDBOX_PROFILE is not None:
        command = ["/usr/bin/sandbox-exec", "-f", str(SANDBOX_PROFILE), *command]
    try:
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=timeout, preexec_fn=disable_core, start_new_session=True)
        record = {"command": command, "returncode": result.returncode,
                  "signal": -result.returncode if result.returncode < 0 else None,
                  "stdout": result.stdout.decode(errors="replace"),
                  "stderr": result.stderr.decode(errors="replace")}
    except subprocess.TimeoutExpired as error:
        return {"command": command, "timed_out": True, "returncode": None,
                "stdout": (error.stdout or b"").decode(errors="replace"),
                "stderr": (error.stderr or b"").decode(errors="replace")}
    lines = [line for line in record["stdout"].splitlines() if line.startswith("{")]
    if len(lines) == 1:
        try:
            record["result"] = json.loads(lines[0])
        except json.JSONDecodeError:
            pass
    return record


def passed(record):
    return record.get("returncode") == 0 and record.get("result", {}).get("ok") is True


def check_manifest(source, out):
    manifest = json.loads((out / "build-manifest.json").read_text())
    if manifest["source_inputs"] != inputs(source)[1]:
        raise RuntimeError("stale isolated native archive: rebuild after source changes")
    archive = bounded_path(manifest["archive"], FAULT_BUILD)
    harness = bounded_path(manifest["harness"], FAULT_BUILD)
    if sha(archive) != manifest["archive_sha256"]:
        raise RuntimeError("isolated archive fingerprint changed")
    if sha(harness) != manifest["harness_sha256"]:
        raise RuntimeError("isolated harness fingerprint changed")
    return manifest


def enumerate_profiles(binary, timeout):
    records = [child(binary, ["enumerate", mode], timeout) for mode in MODES]
    for record in records:
        result = record.get("result", {})
        count = result.get("report", {}).get("attempts", 0)
        if not passed(record) or count < 1 or len(result.get("sequence", [])) != count:
            raise RuntimeError("constructor enumeration failed: " + json.dumps(record))
        if [item["ordinal"] for item in result["sequence"]] != list(range(1, count + 1)):
            raise RuntimeError("non-contiguous allocation trace")
    return records


def run(args):
    global SANDBOX_PROFILE
    source = bounded_path(args.source, WORKSPACE)
    out = bounded_path(args.out_dir, FAULT_BUILD)
    report_path = bounded_path(args.report or str(out / "results.json"), BUILD)
    report = {"action": args.action, "source": str(source), "output": str(out),
              "timestamp_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "test_only": True, "runner_sha256": sha(pathlib.Path(__file__)),
              "hardware_or_operator_acceptance": False,
              "overall_stage": "P5 in progress; P6 final acceptance incomplete",
              "interface_takeover": "294/294, separate from product completion"}
    out.mkdir(parents=True, exist_ok=True)
    try:
        if args.action in ("vectors", "all") and not args.control_archive:
            raise RuntimeError("vectors require a separate frozen --control-archive")
        if args.sandbox_profile:
            SANDBOX_PROFILE = bounded_path(args.sandbox_profile, WORKSPACE)
            report["sandbox_profile"] = {"path": str(SANDBOX_PROFILE), "sha256": sha(SANDBOX_PROFILE)}
        manifest = build(source, out) if args.action in ("build", "all", "before-proof") else check_manifest(source, out)
        report["build"] = manifest
        binary = pathlib.Path(manifest["harness"])
        if args.action == "build":
            report["status"] = "built_not_tested"
            return 0
        if args.action == "before-proof":
            if not args.expect_unsafe:
                raise RuntimeError("before-proof requires --expect-unsafe; its signal is never counted as passing")
            if args.fail_at is None or args.fail_at < 1:
                raise RuntimeError("before-proof requires explicit --fail-at N")
            report["enumeration"] = enumerate_profiles(binary, args.timeout)
            proof = child(binary, ["fail", args.mode, args.fail_at], args.timeout)
            report["unsafe_proof"] = proof
            expected_signal = signal.Signals[args.expected_signal].value
            report["expected_unsafe_signal"] = {"name": args.expected_signal, "number": expected_signal}
            unsafe = proof.get("signal") == expected_signal
            report["status"] = "expected_unsafe_observed" if unsafe else "unsafe_signal_not_observed"
            report["counts_as_passing_fault_test"] = False
            return 0 if unsafe else 1
        if args.action in ("enumerate", "faults", "all"):
            report["enumeration"] = enumerate_profiles(binary, args.timeout)
            plan = {"archive_sha256": manifest["archive_sha256"],
                    "profiles": [record["result"] for record in report["enumeration"]]}
            save(out / "allocation-plan.json", plan)
            report["allocation_plan_sha256"] = sha(out / "allocation-plan.json")
            report["actual_failure_points"] = sum(profile["report"]["attempts"] for profile in plan["profiles"])
        ok = True
        if args.action in ("faults", "all"):
            report["injector_selfchecks"] = [child(binary, [name], args.timeout)
                                             for name in ("selfcheck", "ledger-limit")]
            ok &= all(passed(record) for record in report["injector_selfchecks"])
            failures = []
            report["faults"] = failures
            for profile in plan["profiles"]:
                sequence = profile["sequence"]
                for nth in range(1, len(sequence) + 1):
                    record = child(binary, ["fail", profile["mode"], nth], args.timeout)
                    result = record.get("result", {})
                    actual = result.get("sequence", [])
                    expected = sequence[:nth]
                    matching = len(actual) == nth and all(
                        all(a[key] == b[key] for key in ("ordinal", "bytes", "line", "file", "kind"))
                        for a, b in zip(actual, expected))
                    record["matches_enumerated_prefix"] = matching
                    record["failure_point_passed"] = passed(record) and matching
                    ok &= record["failure_point_passed"]
                    failures.append(record)
                    save(report_path, report)  # Preserve every completed child before proceeding.
            report["actual_failure_points_tested"] = len(failures)
            report["actual_failure_points_passed"] = sum(record["failure_point_passed"] for record in failures)
        if args.action in ("vectors", "all"):
            if not args.control_archive:
                raise RuntimeError("vectors require --control-archive: frozen production baseline, never the test archive")
            control = bounded_path(args.control_archive, WORKSPACE)
            if control == pathlib.Path(manifest["archive"]).resolve() or sha(control) == manifest["archive_sha256"]:
                raise RuntimeError("instrumented archive cannot serve as its own control")
            symbols = subprocess.check_output(["xcrun", "nm", "-g", str(control)], stderr=subprocess.STDOUT)
            if b"switch_test_codec_" in symbols:
                raise RuntimeError("control archive contains allocation-test hooks")
            control_binary = bounded_path(out / "native-codec-vector-control", FAULT_BUILD)
            execute(["xcrun", "clang", *BASE_FLAGS, "-DSWITCH_CODEC_CONTROL", "-I" + str(SHIM),
                     "-I" + str(source), SHIM / "fault_harness.c", control, "-lm", "-lpthread", "-o", control_binary])
            report["control_archive"] = {"path": str(control), "sha256": sha(control),
                                         "fault_symbols_absent": True, "symbols_sha256": hashlib.sha256(symbols).hexdigest()}
            report["frozen_oracles"] = {str(path): sha(path) for path in sorted(FIXTURES.iterdir()) if path.is_file()}
            vector_dir = bounded_path(out / "vectors", FAULT_BUILD)
            vector_dir.mkdir(exist_ok=True)
            report["vectors"] = []
            for mode in MODES:
                original_prefix = vector_dir / f"mode-{mode}-control"
                tested_prefix = vector_dir / f"mode-{mode}-tested"
                for prefix in (original_prefix, tested_prefix):
                    for suffix in (".bits", ".pcm"):
                        bounded_path(str(prefix) + suffix, FAULT_BUILD)
                original = child(control_binary, ["vectors", mode, FIXTURES, original_prefix], args.timeout)
                tested = child(binary, ["vectors", mode, FIXTURES, tested_prefix], args.timeout)
                comparisons = []
                for suffix in (".bits", ".pcm"):
                    a, b = pathlib.Path(str(original_prefix) + suffix), pathlib.Path(str(tested_prefix) + suffix)
                    same = a.is_file() and b.is_file() and a.read_bytes() == b.read_bytes()
                    comparisons.append({"control": str(a), "tested": str(b), "equal": same,
                                        "control_sha256": sha(a) if a.is_file() else None,
                                        "tested_sha256": sha(b) if b.is_file() else None})
                vector_ok = passed(original) and passed(tested) and all(item["equal"] for item in comparisons)
                ok &= vector_ok
                report["vectors"].append({"mode": mode, "control": original, "tested": tested,
                                          "comparisons": comparisons, "passed": vector_ok})
        report["status"] = "passed" if ok else "failed"
        return 0 if ok else 1
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
        return 1
    finally:
        save(report_path, report)
        print(json.dumps({"report": str(report_path), "status": report.get("status", "incomplete")}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("build", "enumerate", "faults", "vectors", "all", "before-proof"))
    parser.add_argument("--source", default=str(ROOT / "third_party/evs"))
    parser.add_argument("--out-dir", default=str(BUILD / "native-codec-allocation/after"))
    parser.add_argument("--report")
    parser.add_argument("--control-archive")
    parser.add_argument("--sandbox-profile", help="wrap only runtime children in sandbox-exec; compiler remains outside")
    parser.add_argument("--expect-unsafe", action="store_true")
    parser.add_argument("--expected-signal", choices=("SIGSEGV", "SIGBUS", "SIGABRT"), default="SIGSEGV")
    parser.add_argument("--fail-at", type=int)
    parser.add_argument("--mode", type=int, choices=MODES, default=-1)
    parser.add_argument("--timeout", type=float, default=30)
    return run(parser.parse_args())


if __name__ == "__main__":
    sys.exit(main())
