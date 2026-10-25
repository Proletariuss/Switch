"""Exercise the real diagnostic writer against stalled/available OS pipes.

Uses standalone rustc, no Cargo dependencies, hardware or external services.
All generated Rust and binaries live in a temporary Switch build directory.
"""
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

SOURCE = Path(__file__).resolve().parents[2]
DIAGNOSTICS = SOURCE / "rust/crates/proletariusd/src/diagnostics.rs"


class NonblockingDiagnostics(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        runtime = SOURCE.parent / "build/runtime"
        runtime.mkdir(parents=True, exist_ok=True)
        cls.temporary = tempfile.TemporaryDirectory(prefix="diagnostics-test-", dir=runtime)
        cls.root = Path(cls.temporary.name)
        cls.binary = cls.root / "pipe-harness"
        cls.units = cls.root / "diagnostics-units"
        harness = cls.root / "main.rs"
        harness.write_text('''#[path = PATH]
mod diagnostics;
fn main() {
    let mode = std::env::args().nth(1).unwrap();
    if mode == "flood" {
        for i in 0..10_000 {
            diagnostics::write_line(format_args!("queued diagnostic {i}: {}", "界".repeat(1800)));
        }
    } else {
        diagnostics::write_line(format_args!("ordinary diagnostic"));
    }
    diagnostics::write_line(format_args!("switchd-rs: native shutdown complete"));
    let started = std::time::Instant::now();
    let flushed = diagnostics::flush(std::time::Duration::from_millis(100));
    println!("{flushed} {}", started.elapsed().as_millis());
}
'''.replace("PATH", json.dumps(str(DIAGNOSTICS))))
        rustc = shutil.which("rustc")
        if rustc is None:
            raise RuntimeError("rustc is required; do not skip the real pipe tests")
        try:
            for source, target, flags in [(harness, cls.binary, []), (DIAGNOSTICS, cls.units, ["--test"])]:
                subprocess.run([rustc, "--edition=2024", *flags, str(source), "-o", str(target)],
                               cwd=SOURCE / "rust", check=True, capture_output=True, text=True, timeout=60)
        except BaseException:
            cls.temporary.cleanup()
            raise

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def test_diagnostic_unit_contracts(self):
        result = subprocess.run([str(self.units)], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("5 passed", result.stdout)

    def test_available_pipe_preserves_final_shutdown_line(self):
        result = subprocess.run([str(self.binary), "normal"], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "ordinary diagnostic\nswitchd-rs: native shutdown complete\n")
        self.assertEqual(result.stdout.split()[0], "true")

    def test_full_open_pipe_does_not_block_process_exit(self):
        read_fd, write_fd = os.pipe()
        try:
            # Fill before spawn; retain the open read end and never read it
            # until the child has exited. This is not a closed-pipe fixture.
            flags = fcntl.fcntl(write_fd, fcntl.F_GETFL)
            fcntl.fcntl(write_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
            while True:
                try:
                    os.write(write_fd, b"x" * 4096)
                except BlockingIOError:
                    break
            fcntl.fcntl(write_fd, fcntl.F_SETFL, flags)
            started = time.monotonic()
            child = subprocess.Popen([str(self.binary), "flood"], stdout=subprocess.PIPE, stderr=write_fd)
            try:
                # wait() deliberately does not drain stderr. A timeout is a
                # test failure; kill below is cleanup and can never count pass.
                result = child.wait(timeout=5)
                self.assertEqual(result, 0)
                self.assertLess(time.monotonic() - started, 5)
                output = child.stdout.read().decode().split()
                self.assertEqual(output[0], "false")
                self.assertLess(int(output[1]), 1000)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=2)
                child.stdout.close()
        finally:
            os.close(write_fd)
            os.close(read_fd)


if __name__ == "__main__":
    unittest.main()
