#!/usr/bin/env python3

import importlib.util
import plistlib
import subprocess
from unittest import mock
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-no-device-software-acceptance.py"
SPEC = importlib.util.spec_from_file_location("check_no_device_software_acceptance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
CHECKER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECKER
SPEC.loader.exec_module(CHECKER)


class DefectClosureEvidenceTests(unittest.TestCase):
    def setUp(self):
        reports = CHECKER.BUILD / "reports"
        capsules = CHECKER.BUILD / "clean-build"
        reports.mkdir(parents=True, exist_ok=True)
        capsules.mkdir(parents=True, exist_ok=True)
        self.report_temp = tempfile.TemporaryDirectory(
            prefix="defect-closure-test-", dir=reports
        )
        self.cold_temp = tempfile.TemporaryDirectory(
            prefix="defect-closure-cold-test-", dir=capsules
        )
        self.report_root = Path(self.report_temp.name)
        self.cold_root = Path(self.cold_temp.name)
        self.cold_path = self.cold_root / "build.log"
        self.cold_path.write_text("migration gates passed\n")
        self.cold_blob = CHECKER.read_regular(self.cold_path, "test cold build.log")
        self.support = self.report_root / "support.log"
        self.support.write_text("independent defect support\n")
        self.digest = "d" * 64
        self.closure_path = self.report_root / "closure.json"

    def tearDown(self):
        self.cold_temp.cleanup()
        self.report_temp.cleanup()

    def closure(self):
        return {
            "schema_version": 1,
            "scope": CHECKER.SCOPE,
            "source_digest": self.digest,
            "defects": [
                {
                    "id": f"B{index:02d}",
                    "status": "SOFTWARE_CLOSED",
                    "evidence": [
                        {
                            "category": "integration_test",
                            **CHECKER.record(self.cold_path),
                        },
                        {
                            "category": "source_review",
                            **CHECKER.record(self.support),
                        },
                    ],
                }
                for index in range(1, 13)
            ],
            "software_defects_closed": 12,
            "product_acceptance_complete": False,
            "device_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
        }

    def verify(self, closure):
        CHECKER.verify_defect_closure_rows(
            closure, self.digest, self.closure_path, self.cold_blob
        )

    def test_accepts_exact_core_cold_log_and_independent_report(self):
        self.verify(self.closure())

    def test_rejects_same_hash_clean_build_path_spoof(self):
        spoof = self.cold_root / "other-build.log"
        spoof.write_bytes(self.cold_blob.data)
        closure = self.closure()
        closure["defects"][0]["evidence"][0] = {
            "category": "integration_test",
            **CHECKER.record(spoof),
        }
        with self.assertRaises(ValueError):
            self.verify(closure)

    def test_rejects_cold_log_changed_after_core_verification(self):
        self.cold_path.write_text("changed after core verification\n")
        closure = self.closure()
        with self.assertRaises(ValueError):
            self.verify(closure)

    def test_rejects_missing_independent_support(self):
        closure = self.closure()
        closure["defects"][0]["evidence"] = [
            {
                "category": "integration_test",
                **CHECKER.record(self.cold_path),
            }
        ]
        with self.assertRaises(ValueError):
            self.verify(closure)

    def test_rejects_support_outside_reports(self):
        external = self.cold_root / "support.log"
        external.write_text("outside reports\n")
        closure = self.closure()
        closure["defects"][0]["evidence"][1] = {
            "category": "source_review",
            **CHECKER.record(external),
        }
        with self.assertRaises(ValueError):
            self.verify(closure)

    def test_rejects_empty_support_report(self):
        empty = self.report_root / "empty.log"
        empty.write_bytes(b"")
        closure = self.closure()
        closure["defects"][0]["evidence"][1] = {
            "category": "source_review",
            **CHECKER.record(empty),
        }
        with self.assertRaises(ValueError):
            self.verify(closure)

    def test_rejects_wrong_category_for_exact_cold_log(self):
        closure = self.closure()
        closure["defects"][0]["evidence"][0]["category"] = "source_review"
        with self.assertRaises(ValueError):
            self.verify(closure)


class ColdMarkerContractTests(unittest.TestCase):
    FLOW = "release flow tests passed (30 cases; mandatory initialization once, two bundle gates, cp-RX ordering, isolated fake app/text image only)"
    READER = "PASS: 4 production stale-recovery scenarios, 2 live timer controls and 9 production startup/cancellation/failure scenarios (virtual platform; no devices or network)"
    OLD_FLOW = "release flow tests passed (isolated fake app and text image only)"
    OLD_READER = "PASS: 4 production stale-recovery scenarios and 2 live timer controls (virtual platform; no devices or network)"

    def cold_log(self):
        version, build, backend = CHECKER.release_identity()
        return "\n".join([
            *("test " + name + " ... ok" for name in CHECKER.CLOSURE_COLD_TESTS),
            *CHECKER.CLOSURE_SWIFT_MARKERS, self.FLOW, self.READER,
            f"Version contract: PASS (app={version}, build={build}, Rust={backend})",
            "PASS: ReaderESIM identity/recovery groups", "migration gates passed", "",
        ])

    def test_current_exact_producer_markers_accepted(self):
        CHECKER.verify_closure_cold_markers(self.cold_log())

    def test_legacy_release_flow_marker_rejected(self):
        with self.assertRaises(ValueError):
            CHECKER.verify_closure_cold_markers(self.cold_log().replace(self.FLOW, self.OLD_FLOW))

    def test_legacy_reader_audio_marker_rejected(self):
        with self.assertRaises(ValueError):
            CHECKER.verify_closure_cold_markers(self.cold_log().replace(self.READER, self.OLD_READER))

    def test_both_legacy_producer_markers_rejected(self):
        old = self.cold_log().replace(self.FLOW, self.OLD_FLOW).replace(self.READER, self.OLD_READER)
        with self.assertRaises(ValueError):
            CHECKER.verify_closure_cold_markers(old)

    def test_missing_current_producer_marker_rejected(self):
        for marker in (self.FLOW, self.READER):
            with self.subTest(marker=marker), self.assertRaises(ValueError):
                CHECKER.verify_closure_cold_markers(self.cold_log().replace(marker, ""))

    def test_duplicate_current_producer_marker_rejected(self):
        for marker in (self.FLOW, self.READER):
            with self.subTest(marker=marker), self.assertRaises(ValueError):
                CHECKER.verify_closure_cold_markers(self.cold_log() + marker + "\n")


class MountedLayoutTests(unittest.TestCase):
    def test_mounted_audit_before_detach_and_failure_is_fenced(self):
        for refuse in [False, True]:
            with self.subTest(refuse=refuse), tempfile.TemporaryDirectory(prefix="mounted-call-", dir=CHECKER.BUILD) as value:
                root=Path(value); image=root/"synthetic-image";image.write_bytes(b"synthetic fixture")
                calls=[]
                def run(argv, **kwargs):
                    calls.append(argv)
                    if argv[:2]==["/usr/bin/hdiutil","attach"]:
                        mount=Path(argv[argv.index("-mountpoint")+1]);(mount/"Switch.app").mkdir();(mount/"Applications").symlink_to("/Applications")
                        return subprocess.CompletedProcess(argv,0,stdout=plistlib.dumps({"system-entities":[{"dev-entry":"/dev/disk99s1","mount-point":str(mount)}]}),stderr=b"")
                    if "--stage-root" in argv and refuse: raise subprocess.CalledProcessError(1,argv)
                    return subprocess.CompletedProcess(argv,0,stdout=b"",stderr=b"")
                with mock.patch.object(CHECKER,"verify_report",return_value={}), mock.patch.object(CHECKER,"verify_app_tree"), mock.patch.object(CHECKER.subprocess,"run",side_effect=run):
                    if refuse:
                        with self.assertRaises(subprocess.CalledProcessError): CHECKER.verify_dmg_app(root/"report.json",image)
                    else: CHECKER.verify_dmg_app(root/"report.json",image)
                self.assertEqual(len(calls),3)
                self.assertIn("audit-release-bundle.py",calls[1][1])
                self.assertIn("--stage-root",calls[1]);self.assertEqual(calls[-1][:2],["/usr/bin/hdiutil","detach"])

    def test_extra_files_and_link_substitution_refuse(self):
        with tempfile.TemporaryDirectory(prefix="mounted-layout-", dir=CHECKER.BUILD) as value:
            root = Path(value)
            app = root / "Switch.app"
            app.mkdir()
            with self.assertRaises(ValueError): CHECKER.mounted_switch_app(root)
            link = root / "Applications"
            link.symlink_to("/Applications")
            self.assertEqual(CHECKER.mounted_switch_app(root), app)
            extra = root / "private.json"
            extra.write_text('{"bark":{"device_key":"synthetic"}}')
            with self.assertRaises(ValueError): CHECKER.mounted_switch_app(root)
            extra.unlink(); link.unlink(); link.symlink_to("/tmp")
            with self.assertRaises(ValueError): CHECKER.mounted_switch_app(root)

if __name__ == "__main__":
    unittest.main()
