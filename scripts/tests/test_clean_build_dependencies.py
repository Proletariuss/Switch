import re
import importlib.util
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ARCHIVE = "$(PROJECT_DIR)/../build/rust-target/release/libproletarius_audio_core.a"


class CleanBuildDependencyTests(unittest.TestCase):
    def test_full_cold_acceptance_runs_tests_before_app_build(self):
        spec = importlib.util.spec_from_file_location(
            "switch_clean_build_audit", ROOT / "scripts/audit-clean-build.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        capsule = Path("/private/tmp/switch-clean-capsule")
        ordinary = module.execution_commands(capsule, False)
        full = module.execution_commands(capsule, True)
        gate = ["/bin/sh", "scripts/test-migration-gates.sh"]
        build = ["/bin/sh", "scripts/build.sh"]
        self.assertNotIn(gate, ordinary)
        self.assertEqual(full.count(gate), 1)
        self.assertLess(full.index(gate), full.index(build))

    def test_audio_archive_is_declared_as_a_script_output(self):
        project = (ROOT / "project.yml").read_text()
        phase = re.search(r"preBuildScripts:\n(?P<body>.*?)(?=\n    sources:)", project, re.S)
        self.assertIsNotNone(phase)
        body = phase.group("body")
        audio_phase = re.search(r"- name: 构建 Rust 实时音频核心(?P<body>.*?)(?=\n      - name:|\Z)", body, re.S)
        self.assertIsNotNone(audio_phase)
        self.assertIn("outputFiles:", audio_phase.group("body"))
        self.assertIn(ARCHIVE, audio_phase.group("body"))

    def test_linker_and_build_script_use_the_same_archive(self):
        project = (ROOT / "project.yml").read_text()
        script = (ROOT / "scripts/build-audio-core.sh").read_text()
        self.assertIn(f'-force_load \\"{ARCHIVE}\\"', project)
        self.assertIn('ARCHIVE="$CARGO_TARGET_DIR/release/libproletarius_audio_core.a"', script)

    def test_workspace_environment_honors_explicit_short_test_temp_root(self):
        with tempfile.TemporaryDirectory(prefix="switch-workspace-env-") as temporary:
            root = Path(temporary) / "workspace"
            source = root / "src"
            scripts = source / "scripts"
            scripts.mkdir(parents=True)
            shutil.copyfile(ROOT / "scripts/workspace-env.sh", scripts / "workspace-env.sh")
            command = '. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"; printf %s "$TMPDIR"'
            base = {"PATH": "/usr/bin:/bin", "HOME": temporary, "SWITCH_SOURCE_ROOT": str(source)}

            default = subprocess.run(["/bin/sh", "-c", command], cwd=source, env=base,
                                     check=True, capture_output=True, text=True).stdout
            self.assertEqual(default, str(root.resolve() / "build/tmp") + "/")

            isolated = dict(base, SWITCH_TEST_TMPDIR="/private/tmp/")
            selected = subprocess.run(["/bin/sh", "-c", command], cwd=source, env=isolated,
                                      check=True, capture_output=True, text=True).stdout
            self.assertEqual(selected, "/private/tmp/")

    def test_generated_xcode_and_bundle_outputs_do_not_change_source_identity(self):
        spec = importlib.util.spec_from_file_location("switch_source_identity", ROOT / "scripts/source-identity.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            module.ROOT = root
            (root / "project.yml").write_text("name: Switch\n")
            (root / "Switch.xcodeproj").mkdir()
            (root / "Switch.xcodeproj/project.pbxproj").write_text("generated one\n")
            (root / "build/DerivedData").mkdir(parents=True)
            (root / "build/DerivedData/stale.o").write_bytes(b"one")
            (root / "Switch.app/Contents/MacOS").mkdir(parents=True)
            (root / "Switch.app/Contents/MacOS/Switch").write_bytes(b"one")
            before = module.digest_tree()
            (root / "Switch.xcodeproj/project.pbxproj").write_text("generated two\n")
            (root / "build/DerivedData/stale.o").write_bytes(b"two")
            (root / "Switch.app/Contents/MacOS/Switch").write_bytes(b"two")
            self.assertEqual(module.digest_tree(), before)


if __name__ == "__main__":
    unittest.main()
