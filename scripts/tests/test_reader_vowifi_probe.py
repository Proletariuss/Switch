import importlib.util
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "reader_vowifi_probe", ROOT / "scripts/probe-reader-vowifi.py"
)
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


class ReaderVoWiFiProbeTests(unittest.TestCase):
    def test_relative_runtime_root_is_resolved_before_passing_to_daemon(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            runtime = PROBE.isolated_runtime_directory(base / "relative-runtime", "test")

            self.assertTrue(runtime.is_absolute())
            self.assertEqual(runtime, (base / "relative-runtime").resolve() / "reader-vowifi-test")

    def test_public_and_companion_sockets_fit_darwin_unix_path_limit(self):
        socket_path = PROBE.isolated_socket_path("0123456789ab")
        paths = [str(socket_path) + suffix for suffix in ("", ".rs", ".in", ".go")]

        self.assertTrue(all(len(path.encode()) < 100 for path in paths), paths)

    def test_profile_overview_is_redacted_and_requires_complete_eid_group(self):
        summary = PROBE.profile_overview_summary(200, {
            "chip_info": {"eids": [{"eid": "private-eid"}]},
            "profiles": [{"eid": "private-eid", "read_state": "ready", "profiles": [
                {"iccid": "private-iccid", "name": "private-name"},
                {"iccid": "private-iccid-2", "name": "private-name-2"},
            ]}],
        })

        self.assertEqual(summary, {
            "http_status": 200,
            "complete": True,
            "eid_count": 1,
            "profile_group_count": 1,
            "profile_count": 2,
        })
        self.assertNotIn("private-eid", str(summary))
        self.assertNotIn("private-iccid", str(summary))
        self.assertFalse(PROBE.profile_overview_summary(200, {
            "chip_info": {"eids": [{"eid": "private-eid"}]},
            "profiles": [{"read_state": "error", "profiles": []}],
        })["complete"])


if __name__ == "__main__":
    unittest.main()
