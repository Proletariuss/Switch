import hashlib
import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKER = ROOT / "scripts/verify-app-identity.py"
VERSION = "3.47"
BUILD = "347"
BACKEND_VERSION = "3.47.0"
SOURCE_DIGEST = hashlib.sha256(b"verified release source").hexdigest()


class AppIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="switch-app-identity-")
        self.addCleanup(self.temporary.cleanup)
        self.app = Path(self.temporary.name) / "Switch.app"
        self.info = self.app / "Contents/Info.plist"
        self.backend = self.app / "Contents/Resources/backend/switchd-rs"
        self.info.parent.mkdir(parents=True)
        self.backend.parent.mkdir(parents=True)
        self.write_identity()
        self.backend.write_bytes(
            f"{BACKEND_VERSION}\n{VERSION}\n{BUILD}\n{SOURCE_DIGEST}".encode()
        )

    def write_identity(self, version=VERSION, build=BUILD, bundle="com.proletarius.switch"):
        with self.info.open("wb") as stream:
            plistlib.dump({
                "CFBundleShortVersionString": version,
                "CFBundleVersion": build,
                "CFBundleIdentifier": bundle,
            }, stream)

    def check(self):
        return subprocess.run(
            [sys.executable, str(CHECKER), str(self.app), VERSION, BUILD, BACKEND_VERSION, SOURCE_DIGEST],
            capture_output=True, text=True,
        )

    def test_matching_app_build_and_backend_source_identity_pass(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("App identity: PASS", result.stdout)

    def test_release_metadata_mismatch_fails_closed(self):
        self.write_identity(build="345")
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CFBundleVersion", result.stderr)

    def test_backend_digest_mismatch_fails_closed(self):
        wrong_digest = hashlib.sha256(b"different source").hexdigest()
        self.backend.write_bytes(
            f"{BACKEND_VERSION}\n{VERSION}\n{BUILD}\n{wrong_digest}".encode()
        )
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("embedded source digest", result.stderr)

    def test_backend_version_mismatch_fails_closed(self):
        self.backend.write_bytes(
            f"3.45.0\n{VERSION}\n{BUILD}\n{SOURCE_DIGEST}".encode()
        )
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("embedded backend version", result.stderr)

    def test_missing_backend_fails_closed(self):
        self.backend.unlink()
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Rust backend executable is missing", result.stderr)


if __name__ == "__main__":
    unittest.main()
