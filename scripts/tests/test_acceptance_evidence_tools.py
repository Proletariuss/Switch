#!/usr/bin/env python3
"""Offline positive/negative controls; all mutations use fresh synthetic files."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SOURCE / "scripts/acceptance"))
import evidence


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, SOURCE / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


daemon = load("daemon_evidence", "scripts/acceptance/test-daemon-capsule.py")
g3 = load("g3_hygiene", "scripts/test-g3-fixture-hygiene.py")
ROOT = Path(os.environ["SWITCH_EVIDENCE_TEST_ROOT"])
ROOT.mkdir(mode=0o700)


def tree_bytes(root):
    return {str(p.relative_to(root)): (p.read_bytes() if p.is_file() else None,
                                     p.stat().st_mtime_ns, p.stat().st_mode, p.stat().st_uid, p.stat().st_gid)
            for p in [root] + sorted(root.rglob("*"))}


def captured(label, argv, env=None):
    result = subprocess.run(argv, env=env, capture_output=True)
    with (ROOT / (label + ".stdout")).open("xb") as stream:
        stream.write(result.stdout)
    with (ROOT / (label + ".stderr")).open("xb") as stream:
        stream.write(result.stderr)
    evidence.write_json(ROOT / (label + ".command.json"), {"argv": argv, "exit_code": result.returncode})
    return result


class EvidenceTests(unittest.TestCase):
    def test_old_root_refused_before_any_command_or_cleanup(self):
        root = ROOT / "old-root"
        root.mkdir()
        for name in ("bin", "cache", "compiler-cache"):
            (root / name).mkdir()
        for name in ("sentinel.log", "bin/acceptance-companion-phase-a", "bin/daemon-capsule-fixture", "manifest.json", "commands.json", "compiler-cache/sentinel", "cache/sentinel"):
            (root / name).write_bytes(b"retained sentinel\x00\xff")
        before = tree_bytes(root)
        with patch.object(sys, "argv", ["test-daemon-capsule.py", str(root)]), \
             patch.object(daemon, "run_command", side_effect=AssertionError("compiler reached")), \
             patch.object(daemon, "clean_owned_caches", side_effect=AssertionError("cleanup reached")):
            with self.assertRaises(FileExistsError):
                daemon.main()
        for index, args in enumerate(([str(root)], [str(root), "--resume-owned-report"])):
            result = captured("daemon-old-" + str(index), [sys.executable, str(SOURCE / "scripts/acceptance/test-daemon-capsule.py")] + args)
            self.assertNotEqual(result.returncode, 0)
        command = ["/bin/sh", str(SOURCE / "scripts/test-acceptance-companion-phase-a.sh")]
        for index, args in enumerate(([str(root)], [])):
            env = dict(os.environ)
            env.pop("SWITCH_PHASE_A_REPORT", None)
            result = captured("phase-old-" + str(index), command + args, env)
            self.assertNotEqual(result.returncode, 0)
        result = captured("phase-old-env", command, dict(os.environ, SWITCH_PHASE_A_REPORT=str(root)))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(tree_bytes(root), before)
        evidence.write_json(ROOT / "old-root-sentinel.json", {"bytes_modes_mtime_uid_gid_unchanged": True,
                                                             "files_and_directories": sorted(before)})

    def test_mock_stable_and_each_drift_fail_closed(self):
        for case in ("stable", "source", "fixture", "driver", "added", "deleted", "config", "tool", "generated", "generated-added", "generated-deleted"):
            with self.subTest(case=case):
                inputs = ROOT / ("inputs-" + case)
                inputs.mkdir()
                for name in ("source.rs", "fixture.swift", "driver.py", "Cargo.toml", "tool"):
                    (inputs / name).write_text("original\n")
                output = ROOT / ("case-" + case)
                def batch(out, env, cargo, swift, checks, generated):
                    daemon.actual_generated_snapshot(out, "before")
                    binary = out / "bin/fixture"
                    binary.write_bytes(b"mock binary")
                    generated[str(binary)] = evidence.sha(binary)
                    changes = {"source": "source.rs", "fixture": "fixture.swift", "driver": "driver.py",
                               "config": "Cargo.toml", "tool": "tool"}
                    if case in changes:
                        (inputs / changes[case]).write_text("changed\n")
                    elif case == "added":
                        (inputs / "new.rs").write_text("new\n")
                    elif case == "deleted":
                        (inputs / "fixture.swift").unlink()
                    elif case == "generated":
                        binary.write_bytes(b"changed binary")
                    elif case == "generated-added":
                        (out / "bin/new-binary").write_bytes(b"new binary")
                    elif case == "generated-deleted":
                        binary.unlink()
                    return {"scope": "mock-only"}
                with patch.object(sys, "argv", ["test-daemon-capsule.py", str(output)]), \
                     patch.object(daemon, "input_roots", return_value=[inputs]), \
                     patch.object(daemon, "tool_snapshot", return_value=({"cargo": "/mock/cargo", "rustc": "/mock/rustc", "swiftc": "/mock/swiftc"}, {"mock": True, "--show-sdk-path": "/mock/sdk"})), \
                     patch.object(daemon, "run_batch", side_effect=batch):
                    status = daemon.main()
                report = json.loads((output / "report.json").read_text())
                self.assertEqual(status == 0, case == "stable")
                self.assertEqual(report["passed"], case == "stable")
                self.assertTrue((output / "inputs-before.json").exists())
                self.assertTrue((output / "inputs-after.json").exists())

    def test_actual_generated_inventory_kinds_and_early_failure(self):
        output = ROOT / "actual-kinds"
        output.mkdir()
        directory = output / "compiler-cache/debug/build/mock/out"
        directory.mkdir(parents=True)
        (directory / "input.rs").write_bytes(b"stable input")
        (directory / "empty-directory").mkdir()
        (directory / "file-link").symlink_to("input.rs")
        before = daemon.actual_generated_snapshot(output, "before")
        self.assertFalse(before["errors"])
        self.assertEqual(before["entries"]["debug/build/mock/out/file-link"]["target"], "input.rs")
        self.assertIn("sha256", before["entries"]["debug/build/mock/out/empty-directory"])
        (directory / "file-link").unlink()
        (directory / "file-link").symlink_to("missing")
        after = daemon.actual_generated_snapshot(output, "after")
        self.assertTrue(after["errors"])
        self.assertNotEqual(before["entries"], after["entries"])

        failed = ROOT / "actual-early-failure"
        def batch(out, *args):
            generated = out / "compiler-cache/debug/build/mock/out"
            generated.mkdir(parents=True)
            (generated / "partial.rs").write_bytes(b"partial before compile failure")
            raise RuntimeError("synthetic early compiler failure")
        with patch.object(sys, "argv", ["test-daemon-capsule.py", str(failed)]), \
             patch.object(daemon, "input_roots", return_value=[]), \
             patch.object(daemon, "tool_snapshot", return_value=({"cargo": "/mock/cargo", "rustc": "/mock/rustc", "swiftc": "/mock/swiftc"}, {"--show-sdk-path": "/mock/sdk"})), \
             patch.object(daemon, "run_batch", side_effect=batch):
            self.assertEqual(daemon.main(), 1)
        self.assertFalse(json.loads((failed / "report.json").read_text())["passed"])
        self.assertFalse(json.loads((failed / "actual-generated-before.json").read_text())["baseline_established"])
        self.assertTrue((failed / "compiler-cache/debug/build/mock/out/partial.rs").exists())

    def test_create_only_file_and_root(self):
        path = ROOT / "receipt.json"
        evidence.write_json(path, {"original": True})
        with self.assertRaises(FileExistsError):
            evidence.write_json(path, {"replacement": True})
        self.assertEqual(json.loads(path.read_text()), {"original": True})
        for raw in ("relative", str(SOURCE.parent / "build/outside-reports")):
            with self.assertRaises(ValueError):
                evidence.create_report(raw, SOURCE.parent)

    def test_nested_parent_summary(self):
        child = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        parent = "test result: ok. 366 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 280.00s\n"
        matches, selected = g3.parent_summary("running 366 tests\nrunning 1 test\n" + child + parent)
        self.assertEqual(len(matches), 2)
        self.assertEqual(selected["passed"], 366)
        _, failed = g3.parent_summary("running 366 tests\n" + child + parent.replace("ok. 366 passed; 0 failed", "FAILED. 365 passed; 1 failed"))
        self.assertEqual(failed["status"], "FAILED")
        self.assertIsNone(g3.parent_summary("running 366 tests\n" + child)[1])
        self.assertEqual(g3.FILTER_TIMEOUTS["native_vowifi_session::service::tests::"], 420)

    def test_g3_parent_failure_and_timeout_stay_failed(self):
        for case in ("success", "parent-failure", "timeout"):
            out = ROOT / ("g3-" + case)
            out.mkdir()
            binary = out / "synthetic-test"
            summary = "FAILED. 365 passed; 1 failed" if case == "parent-failure" else "ok. 366 passed; 0 failed"
            text = "running 366 tests\nrunning 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\ntest result: " + summary + "; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
            binary.write_text("#!/usr/bin/python3\nprint(" + repr(text) + ")\n")
            binary.chmod(0o700)
            argv = ["test-g3-fixture-hygiene.py", "--binary", str(binary), "--report-dir", str(out),
                    "--runtime-parent", str(out), "--filter", "native_vowifi_session::service::tests::"]
            original_popen = subprocess.Popen
            class TimedOut:
                pid = 123456
                def __init__(self, args, **kwargs):
                    kwargs["stdout"].write(text)
                def wait(self, timeout=None):
                    if timeout is not None:
                        raise subprocess.TimeoutExpired("mock", timeout)
                    return -9
                def kill(self):
                    pass
            with patch.object(sys, "argv", argv), patch.object(g3.subprocess, "Popen", TimedOut if case == "timeout" else original_popen):
                code = g3.main()
            report = json.loads(next(out.glob("g3*.json")).read_text())
            self.assertEqual(code == 0, case == "success")
            self.assertEqual(report["passed"], case == "success")
            self.assertEqual(report["filters"][0]["passed_tests"], 365 if case == "parent-failure" else 366)


if __name__ == "__main__":
    unittest.main()
