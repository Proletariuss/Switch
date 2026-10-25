import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check-rust-backend-exit.py"
SPEC = importlib.util.spec_from_file_location("rust_backend_exit", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)
BUILD = SCRIPT.parents[1].parent / "build/optimization/all-rust-exit-tool/tests"
BUILD.mkdir(parents=True, exist_ok=True)


class RustBackendExitTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir=BUILD)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.fixture(self.root)

    def put(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def fixture(self, root):
        required = ({rule.path for rule in MODULE.RULES} | set(MODULE.DEFAULT_GATE_FILES) |
                    set(MODULE.SOURCE_EXPORT_FILES) | set(MODULE.PRODUCTION_SHELL_FILES) |
                    set(MODULE.PRODUCTION_PYTHON_FILES))
        for relative in required:
            self.put(relative, "# native implementation\n")
        self.put("rust/Cargo.toml", '[workspace]\nmembers = ["crates/proletariusd"]\n')

    def keys(self):
        return {item[0] for item in MODULE.collect_blockers(self.root)}

    def test_full_backend_deletion_passes(self):
        self.assertFalse((self.root / "backend").exists())
        self.assertEqual(MODULE.collect_blockers(self.root), [])

    def test_swift_runtime_selector_is_rejected_in_every_production_file(self):
        samples = {
            "app/Sources/BackendProcess.swift": "var usesRustGateway: Bool { true }\n",
            "app/Sources/Nested/Status.swift": 'let readPath = DashboardState(readPath: "go")\n',
            "app/Sources/Feature.swift": 'let goBackend = chooseBackend()\n',
        }
        for relative, source in samples.items():
            with self.subTest(relative=relative):
                self.put(relative, source)
                keys = self.keys()
                self.assertTrue(
                    {"swift-go-runtime-selector", "swift-go-read-path"} & keys,
                    f"runtime selector escaped gate: {relative}",
                )
                (self.root / relative).unlink()

    def test_missing_swift_runtime_tree_is_unverified(self):
        for path in sorted((self.root / "app/Sources").rglob("*"), reverse=True):
            if path.is_file():
                path.unlink()
            elif path.is_dir():
                path.rmdir()
        (self.root / "app/Sources").rmdir()
        self.assertIn("swift-runtime-tree-unverified", self.keys())

    def test_each_known_build_and_runtime_edge_is_detected(self):
        samples = {
            "xcode-builds-go": ("scripts/build-backends.sh", "GOARCH=arm64 go build ./cmd/proletarius-macos\n"),
            "app-bundles-go-binary": ("scripts/build-backends.sh", 'cp "${TARGET_TEMP_DIR}/backend/switchd" "$BACKEND_DIR/switchd"\n'),
            "rust-supervises-go": ("rust/crates/proletariusd/src/gateway.rs", "pub(crate) struct GoBackendProcess;\n"),
            "rust-dispatches-to-go": ("rust/crates/proletariusd/src/main.rs", "GoBackendProcess::spawn(); upstream::forward(request, &state.go_socket_path)\n"),
            "swift-go-compatibility-mode": ("app/Sources/BackendProcess.swift", 'if mode == "go-compatibility" { return "switchd" }\n'),
        }
        for key, (relative, source) in samples.items():
            with self.subTest(key=key):
                self.put(relative, source)
                self.assertIn(key, self.keys())
                self.put(relative, "# native implementation\n")

    def test_all_go_sources_include_pkg_vendor_test_and_tool_directories(self):
        for relative in ["backend/cmd/tool/main.go", "backend/pkg/codec.go",
                         "backend/vendor/lib/codec.go", "third_party/nested/legacy_test.go",
                         "renamed/legacy.go", "scripts/scanner.go"]:
            with self.subTest(relative=relative):
                self.put(relative, "package legacy\n")
                blockers = MODULE.collect_blockers(self.root)
                self.assertIn("go-sources-remain", {key for key, _, _ in blockers})
                self.assertTrue(any(relative in path for _, path, _ in blockers))
                (self.root / relative).unlink()
        self.assertEqual(MODULE.collect_blockers(self.root), [])

    def test_module_and_workspace_metadata_in_any_directory_are_blockers(self):
        for relative in ["go.mod", "go.sum", "go.work", "go.work.sum",
                         "backend/go.mod", "third_party/library/go.mod", "renamed/nested/go.sum"]:
            with self.subTest(relative=relative):
                self.put(relative, "fixture metadata\n")
                self.assertIn("go-module-remains", self.keys())
                (self.root / relative).unlink()

    def test_nonexistent_or_file_root_is_unverified(self):
        for path in [self.root / "missing", self.root / "rust/Cargo.toml"]:
            self.assertIn("source-root-invalid", {b[0] for b in MODULE.collect_blockers(path)})

    def test_malformed_workspace_is_not_a_false_pass(self):
        for content in ["", "[workspace", "[package]\nname='unrelated'", "workspace=[]",
                        '[workspace]\nmembers="crates/proletariusd"']:
            with self.subTest(content=content):
                self.put("rust/Cargo.toml", content)
                self.assertIn("source-root-invalid", self.keys())

    def test_missing_or_empty_required_current_sources_fail(self):
        relative = "rust/crates/proletariusd/src/main.rs"
        (self.root / relative).unlink()
        self.assertIn("required-source-unverified", self.keys())
        self.put(relative, " ")
        self.assertIn("required-source-unverified", self.keys())

    def test_default_gate_go_invocations_fail(self):
        for relative, source in [
            ("scripts/test-migration-gates.sh", 'env GOTOOLCHAIN=local go test ./...\n'),
            ("scripts/workspace-env.sh", 'version=$(go version)\n'),
            ("scripts/build-backends.sh", '"/opt/toolchain/bin/go" build ./tool\n'),
            ("scripts/check-rust-migration-contracts.py", 'subprocess.run(["go", "run", "scan.go"])\n'),
            ("scripts/test-frozen-protocol-fixtures.py", 'command = ["go", "test", "./..."]\n'),
        ]:
            with self.subTest(relative=relative):
                self.put(relative, source)
                self.assertIn("default-gate-invokes-go", self.keys())
                self.put(relative, "# native implementation\n")

    def test_build_and_release_entrypoints_reject_direct_go_invocations(self):
        samples = {
            "scripts/build.sh": "go build ./cmd/unwanted\n",
            "scripts/release.sh": "go test ./...\n",
            "scripts/package-dmg.sh": '"/opt/go/bin/go" build ./cmd/unwanted\n',
            "project.yml": 'postBuildScripts:\n  - script: |\n      go build ./cmd/unwanted\n',
            "scripts/build-audio-core.sh": "go build ./cmd/unwanted\n",
        }
        for relative, source in samples.items():
            with self.subTest(relative=relative):
                self.put(relative, source)
                self.assertIn("production-entry-invokes-go", self.keys())
                self.put(relative, "# native implementation\n")

    def test_python_build_helpers_reject_go_invocations(self):
        samples = {
            "scripts/build-evs.py": 'import subprocess\nsubprocess.run(["go", "build", "./cmd/unwanted"])\n',
            "scripts/build-module-supervisor.py": 'import subprocess\nsubprocess.check_output("go version", shell=True)\n',
            "scripts/prepare-module-supervisor.py": 'import os\nos.system("/opt/go/bin/go env")\n',
            "scripts/source-identity.py": 'import subprocess\ngo_binary = "go"\nsubprocess.run([go_binary, "test", "./..."])\n',
            "scripts/verify-app-identity.py": 'import subprocess\nsubprocess.run(["sh", "-c", "go build ./cmd/unwanted"])\n',
        }
        for relative, source in samples.items():
            with self.subTest(relative=relative):
                self.put(relative, source)
                self.assertIn("production-entry-invokes-go", self.keys())
                self.put(relative, "# native implementation\n")

    def test_unreadable_or_malformed_production_entry_is_not_certified(self):
        (self.root / "scripts/release.sh").unlink()
        self.assertIn("required-source-unverified", self.keys())
        self.put("scripts/release.sh", "# native implementation\n")
        self.put("scripts/build-evs.py", "def broken(:\n")
        self.assertIn("production-entry-unverified", self.keys())

    def test_historical_license_text_and_comments_do_not_trigger_production_scan(self):
        self.put("licenses/NOTICE.txt", "Historic go build instructions retained for provenance.\n")
        self.put("project.yml", "# historic go build instructions\n")
        self.put("scripts/release.sh", "# historic go build instructions\n")
        self.put("scripts/build-evs.py", '"""Historic example: subprocess.run(["go", "build"])."""\n')
        self.assertEqual(MODULE.collect_blockers(self.root), [])

    def test_snapshot_tool_cannot_require_go_after_source_retirement(self):
        path = "scripts/capture-optimization-baseline.rb"
        self.put(path, "toolchain = { go: command('go', 'version') }\n")
        self.assertIn("source-export-invokes-go", self.keys())
        self.put(path, "toolchain = { rustc: command('rustc', '--version') }\n")
        self.assertEqual(MODULE.collect_blockers(self.root), [])

    def test_missing_snapshot_tool_cannot_be_certified(self):
        (self.root / "scripts/capture-optimization-baseline.rb").unlink()
        self.assertIn("required-source-unverified", self.keys())

    def test_ci_go_toolchain_and_module_version_file_are_blockers(self):
        for source in [
            "steps:\n  - uses: actions/setup-go@v5\n",
            "steps:\n  - uses: 'actions/setup-go@v5'\n",
            "with:\n  go-version-file: backend/go.mod\n",
            "with:\n  go-version: '1.26'\n",
            "steps:\n  - run: go test ./...\n",
            "steps:\n  - run: 'go build ./tool'\n",
            "steps:\n  - run: |\n      cd source\n      go vet ./...\n",
        ]:
            with self.subTest(source=source):
                self.put(".github/workflows/release.yml", source)
                self.assertIn("ci-requires-go", self.keys())

    def test_native_ci_with_historical_notices_is_allowed(self):
        self.put(".github/workflows/release.yml", "steps:\n  - uses: actions/checkout@v4\n  - run: |\n      cargo test --workspace\n      cp licenses/go-notices.txt output/backend/\n")
        self.assertEqual(MODULE.collect_blockers(self.root), [])

    def test_mise_go_install_or_selection_is_blocked(self):
        for source in [
            '[tools]\ngo = "1.26.3"\n',
            '["tools"]\n"go" = "latest"\n',
            "['tools']\n'go' = 'system'\n",
            'tools.go = "1.26"\n',
            '[tools.go]\nversion = "1.26"\n',
            '[tools]\n"core:go" = "1.26"\n',
            '[tasks.check]\nrun = "go test ./..."\n',
            '[tasks.check]\nrun = "sh -c \'go test ./...\'"\n',
        ]:
            with self.subTest(source=source):
                self.put("mise.toml", source)
                self.assertIn("tool-manager-requires-go", self.keys())

    def test_native_mise_and_tool_versions_allow_historical_description(self):
        self.put("mise.toml", '[tools]\nrust = "1.98.0"\n[tasks."rust:check"]\ndescription = "Historical go build retired"\nrun = "sh -c \'cargo test --workspace\'"\n')
        self.put(".tool-versions", 'rust 1.98.0\npython 3.9.6 # historical Go tool removed\n')
        self.assertEqual(MODULE.collect_blockers(self.root), [])

    def test_malformed_tool_manager_configs_fail_closed(self):
        for source in ['[tools', '[tools]\nrust = "unterminated', '[tools]\nnot an assignment',
                       '[tools]\nrust = "1"\nrust = "2"', '[tools]\n[tools]',
                       '[tools]\nrust = {version = "1.98"}']:
            with self.subTest(source=source):
                self.put("mise.toml", source)
                self.assertIn("tool-manager-unverified", self.keys())
        (self.root / "mise.toml").unlink()
        for source in ['rust', 'rust "unterminated']:
            with self.subTest(source=source):
                self.put(".tool-versions", source)
                self.assertIn("tool-manager-unverified", self.keys())

    def test_asdf_go_and_golang_selections_are_blocked(self):
        for source in ['go 1.26.3', 'golang system', 'golang 1.25.0 1.26.3']:
            with self.subTest(source=source):
                self.put(".tool-versions", source)
                self.assertIn("tool-manager-requires-go", self.keys())

    def test_unparseable_default_gate_fails_closed(self):
        self.put("scripts/test-migration-gates.sh", 'echo "unterminated\n')
        self.assertIn("default-gate-unverified", self.keys())

    def test_historical_notices_and_backend_bundle_directory_are_harmless(self):
        self.put("scripts/build-backends.sh", '# Historical backend/go.mod attribution\ncp "licenses/euicc-go.txt" "$BACKEND_DIR/euicc-go.txt"\nmkdir -p "$APP/Contents/Resources/backend"\n')
        self.put("licenses/NOTICE.txt", "Historical Go module and go build instructions; no executable source.\n")
        self.assertEqual(MODULE.collect_blockers(self.root), [])

    def test_symlink_cannot_hide_uninspected_source(self):
        (self.root / "hidden").symlink_to(self.root / "missing", target_is_directory=True)
        self.assertIn("source-tree-unverified", self.keys())

    def test_git_object_metadata_is_not_source(self):
        self.put(".git/objects/legacy.go", "object metadata")
        self.assertEqual(MODULE.collect_blockers(self.root), [])


if __name__ == "__main__":
    unittest.main()
