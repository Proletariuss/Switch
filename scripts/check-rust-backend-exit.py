#!/usr/bin/env python3
"""Inventory the production build/runtime edges that must disappear for Rust-only Switch."""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import ast
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Rule:
    key: str
    path: str
    pattern: str
    explanation: str


RULES = (
    Rule(
        "xcode-builds-go",
        "scripts/build-backends.sh",
        r"(?m)^\s*GOARCH\s*=.*\bgo\s+build\b",
        "Xcode still compiles the Go backend",
    ),
    Rule(
        "app-bundles-go-binary",
        "scripts/build-backends.sh",
        r"(?m)^\s*cp\s+\"\$\{TARGET_TEMP_DIR\}/backend/switchd\"",
        "the app still embeds the Go backend executable",
    ),
    Rule(
        "rust-supervises-go",
        "rust/crates/proletariusd/src/gateway.rs",
        r"\bGoBackendProcess\b|PROLETARIUS_GO_BACKEND_PATH|join\(\"switchd\"\)",
        "the Rust daemon still launches and supervises the Go backend",
    ),
    Rule(
        "rust-dispatches-to-go",
        "rust/crates/proletariusd/src/main.rs",
        r"GoBackendProcess::spawn|upstream::forward\(request,\s*&state\.go_socket_path\)",
        "the Rust gateway still depends on the Go Unix-socket server",
    ),
    Rule(
        "swift-go-compatibility-mode",
        "app/Sources/BackendProcess.swift",
        r"go-compatibility|return\s+\"switchd\"",
        "Swift still exposes a Go executable selection path",
    ),
)

# Runtime selector symbols are forbidden across the complete production Swift
# tree. A constant-true branch is still a dormant backend selection surface and
# can silently revive after a later edit.
SWIFT_RUNTIME_RULES = (
    (
        "swift-go-runtime-selector",
        re.compile(r"\busesRustGateway\b|\bgoBackend\b|\bgo_backend\b|go-compatibility"),
        "Swift production source still contains a Go/Rust backend selector",
    ),
    (
        "swift-go-read-path",
        re.compile(r'\breadPath\s*:\s*"go"'),
        "Swift production source can publish a Go backend read path",
    ),
)


DEFAULT_GATE_FILES = (
    "scripts/build-backends.sh",
    "scripts/test-migration-gates.sh",
    "scripts/workspace-env.sh",
    "scripts/check-rust-migration-contracts.py",
    "scripts/test-frozen-protocol-fixtures.py",
)
SOURCE_EXPORT_FILES = ("scripts/capture-optimization-baseline.rb",)
# These are reachable from the CI/Xcode/release entrypoints, but are not all
# covered by the historical Go-backend signatures in RULES.  Keep the list
# explicit so a newly added build or release command cannot hide in a file
# that the structural gate never reads.
PRODUCTION_SHELL_FILES = (
    "project.yml",
    "scripts/build.sh",
    "scripts/build-audio-core.sh",
    "scripts/build-ble-core.sh",
    "scripts/build-handoff-core.sh",
    "scripts/release.sh",
    "scripts/package-dmg.sh",
    "scripts/sign-release-app.sh",
    "scripts/generate-release-manifest.sh",
    "scripts/preflight-source.sh",
    "scripts/version.sh",
    "scripts/bump-version.sh",
)
PRODUCTION_PYTHON_FILES = (
    "scripts/audit-clean-build.py",
    "scripts/build-evs.py",
    "scripts/build-module-supervisor.py",
    "scripts/prepare-module-supervisor.py",
    "scripts/source-identity.py",
    "scripts/verify-app-identity.py",
    "scripts/check-no-device-software-acceptance.py",
)
GO_COMMANDS = {"build", "test", "vet", "run", "env", "version", "generate", "install", "get", "mod", "work", "tool"}


def invokes_go(text: str, shell: bool) -> bool:
    if shell:
        lexer = shlex.shlex(text, posix=True, punctuation_chars=";&|()")
        lexer.whitespace_split = True
        tokens = list(lexer)
        if any(
            Path(token).name == "go" and tokens[index + 1] in GO_COMMANDS
            for index, token in enumerate(tokens[:-1])
        ):
            return True
        return any(
            Path(token).name in {"sh", "bash", "zsh"}
            and tokens[index + 1] in {"-c", "-lc"}
            and invokes_go(tokens[index + 2], shell=True)
            for index, token in enumerate(tokens[:-2])
        )
    # Default Python gates use explicit subprocess argv; quoted explanatory
    # prose and historical notice paths do not match an executable argv pair.
    return re.search(r"[\[,(]\s*['\"](?:[^'\"]*/)?go['\"]\s*,\s*['\"](?:" + "|".join(sorted(GO_COMMANDS)) + r")[ '\"]", text) is not None


def python_invokes_go(text: str) -> bool:
    """Parse production Python and detect literal Go process invocations."""
    tree = ast.parse(text)
    process_calls = {"run", "call", "check_call", "check_output", "Popen",
                     "system", "popen", "create_subprocess_exec", "create_subprocess_shell"}
    literal_names = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign) and isinstance(node.value, ast.Constant):
            for target in node.targets:
                if isinstance(target, ast.Name) and isinstance(node.value.value, str):
                    literal_names[target.id] = node.value.value

    def literal_string(value):
        if isinstance(value, ast.Constant) and isinstance(value.value, str):
            return value.value
        if isinstance(value, ast.Name):
            return literal_names.get(value.id)
        return None

    for node in ast.walk(tree):
        if not isinstance(node, ast.Call) or not node.args:
            continue
        function = node.func
        if not isinstance(function, ast.Attribute) or function.attr not in process_calls:
            continue
        argument = node.args[0]
        if literal_string(argument) is not None:
            if invokes_go(literal_string(argument), shell=True):
                return True
        elif isinstance(argument, (ast.List, ast.Tuple)):
            values = [literal_string(item) for item in argument.elts]
            if any(Path(value).name == "go" and values[index + 1] in GO_COMMANDS
                   for index, value in enumerate(values[:-1]) if value is not None):
                return True
            if all(value is not None for value in values) and invokes_go(
                " ".join(shlex.quote(value) for value in values), shell=True
            ):
                return True
    return False


def workflow_requires_go(text: str) -> bool:
    if re.search(r"(?mi)^\s*(?:-\s*)?uses:\s*['\"]?actions/setup-go@", text):
        return True
    if re.search(r"(?mi)^\s*go-version(?:-file)?:", text):
        return True
    lines = text.splitlines()
    for index, line in enumerate(lines):
        run = re.match(r"^(\s*)(?:-\s*)?run:\s*(.*)$", line)
        if not run:
            continue
        script = run.group(2)
        if script in {"|", "|-", "|+", ">", ">-", ">+"}:
            body = []
            for following in lines[index + 1:]:
                if following.strip() and len(following) - len(following.lstrip()) <= len(run.group(1)):
                    break
                body.append(following.strip())
            script = "\n".join(body)
        elif len(script) >= 2 and script[0] == script[-1] and script[0] in "\"'":
            script = script[1:-1]
        if invokes_go(script, shell=True):
            return True
    return False


def toml_line(line: str) -> str:
    """Strip comments while rejecting unterminated single-line TOML strings."""
    quote, escaped = None, False
    for index, char in enumerate(line):
        if quote:
            if escaped:
                escaped = False
            elif char == "\\" and quote == '"':
                escaped = True
            elif char == quote:
                quote = None
        elif char in "\"'":
            quote = char
        elif char == "#":
            return line[:index].strip()
    if quote:
        raise ValueError("unterminated TOML string")
    return line.strip()


def mise_requires_go(text: str) -> bool:
    # Checked-in mise.toml uses tables and single-line scalar assignments.
    # Unknown TOML forms fail closed rather than inventing a partial success.
    atom = r'(?:[A-Za-z0-9_-]+|"(?:\\.|[^"\\])*"|\'[^\']*\')'
    key = atom + r'(?:\s*\.\s*' + atom + r')*'
    section, seen, tables = [], set(), set()
    for number, raw in enumerate(text.splitlines(), 1):
        line = toml_line(raw)
        if not line:
            continue
        table = re.fullmatch(r'\[\s*(' + key + r')\s*\]', line)
        assignment = re.fullmatch(r'(' + key + r')\s*=\s*(.+)', line)
        def parts(value):
            return [json.loads(token) if token.startswith('"') else token[1:-1] if token.startswith("'") else token
                    for token in re.findall(atom, value)]
        if table:
            section = parts(table.group(1))
            if tuple(section) in tables:
                raise ValueError(f"duplicate TOML table at line {number}")
            tables.add(tuple(section))
            if len(section) >= 2 and section[0] == "tools" and section[1] in {"go", "golang", "core:go"}:
                return True
            continue
        if not assignment:
            raise ValueError(f"unsupported/malformed TOML at line {number}")
        path = section + parts(assignment.group(1))
        if len(path) >= 2 and path[0] == "tools" and path[1] in {"go", "golang", "core:go"}:
            return True
        if tuple(path) in seen:
            raise ValueError(f"duplicate TOML key at line {number}")
        seen.add(tuple(path))
        value = assignment.group(2)
        if re.fullmatch(r'"(?:\\.|[^"\\])*"', value):
            parsed = json.loads(value)
        elif re.fullmatch(r"'[^']*'", value):
            parsed = value[1:-1]
        elif value in {"true", "false"} or re.fullmatch(r'[+-]?[0-9]+', value):
            parsed = value
        else:
            raise ValueError(f"unsupported/malformed TOML scalar at line {number}")
        if path[0] == "tasks" and path[-1] == "run" and invokes_go(parsed, shell=True):
            return True
    return False


def tool_versions_requires_go(text: str) -> bool:
    for number, line in enumerate(text.splitlines(), 1):
        parts = shlex.split(line, comments=True)
        if not parts:
            continue
        if len(parts) < 2:
            raise ValueError(f"missing tool version at line {number}")
        if parts[0] in {"go", "golang"}:
            return True
    return False


def switch_workspace_members(text):
    # Deliberately recognize the checked-in literal workspace declaration rather
    # than depend on a TOML package or a newer Python than the Xcode toolchain.
    table = re.search(r"(?ms)^\[workspace\]\s*\n(.*?)(?=^\[|\Z)", text)
    declaration = re.search(r"(?ms)^members\s*=\s*(\[.*?\])\s*(?:#.*)?$", table.group(1)) if table else None
    try:
        members = ast.literal_eval(declaration.group(1)) if declaration else None
    except (ValueError, SyntaxError):
        members = None
    return isinstance(members, list) and all(isinstance(item, str) for item in members) and "crates/proletariusd" in members


def collect_blockers(root: Path) -> list[tuple[str, str, str]]:
    blockers: list[tuple[str, str, str]] = []
    if not root.is_dir():
        return [("source-root-invalid", str(root), "source root missing or not a directory")]
    try:
        if not switch_workspace_members((root / "rust/Cargo.toml").read_text(encoding="utf-8")):
            raise ValueError("missing or malformed literal Switch workspace members")
    except (OSError, ValueError) as error:
        blockers.append(("source-root-invalid", "rust/Cargo.toml", "source root has missing/malformed Rust workspace: " + str(error)))

    texts = {}
    required = ({rule.path for rule in RULES} | set(DEFAULT_GATE_FILES) |
                set(SOURCE_EXPORT_FILES) | set(PRODUCTION_SHELL_FILES) |
                set(PRODUCTION_PYTHON_FILES))
    for relative in sorted(required):
        path = root / relative
        try:
            text = path.read_text(encoding="utf-8")
            if not text.strip():
                raise ValueError("empty required source")
            texts[relative] = text
        except (OSError, ValueError) as error:
            blockers.append(("required-source-unverified", relative, "source missing/unreadable/empty; exit state cannot be verified: " + str(error)))
    for rule in RULES:
        if rule.path in texts and re.search(rule.pattern, texts[rule.path]):
            blockers.append((rule.key, rule.path, rule.explanation))

    swift_root = root / "app/Sources"
    if not swift_root.is_dir():
        blockers.append((
            "swift-runtime-tree-unverified",
            "app/Sources",
            "production Swift source tree is missing",
        ))
    else:
        for path in sorted(swift_root.rglob("*.swift")):
            relative = path.relative_to(root).as_posix()
            try:
                source = path.read_text(encoding="utf-8")
            except OSError as error:
                blockers.append((
                    "swift-runtime-tree-unverified",
                    relative,
                    "production Swift source is unreadable: " + str(error),
                ))
                continue
            for key, pattern, explanation in SWIFT_RUNTIME_RULES:
                if pattern.search(source):
                    blockers.append((key, relative, explanation))
    for relative in DEFAULT_GATE_FILES:
        if relative not in texts:
            continue
        try:
            if invokes_go(texts[relative], relative.endswith(".sh")):
                blockers.append(("default-gate-invokes-go", relative, "default build/test/check path executes the Go toolchain"))
        except ValueError as error:
            blockers.append(("default-gate-unverified", relative, "malformed shell cannot be verified: " + str(error)))

    for relative in SOURCE_EXPORT_FILES:
        if relative in texts and invokes_go(texts[relative], shell=False):
            blockers.append(("source-export-invokes-go", relative, "source snapshot/export tooling executes the Go toolchain"))

    for relative in PRODUCTION_SHELL_FILES:
        if relative not in texts:
            continue
        try:
            if invokes_go(texts[relative], shell=True):
                blockers.append(("production-entry-invokes-go", relative,
                                 "build, release, or Xcode entrypoint executes the Go toolchain"))
        except ValueError as error:
            blockers.append(("production-entry-unverified", relative,
                             "build/release entrypoint cannot be parsed: " + str(error)))

    for relative in PRODUCTION_PYTHON_FILES:
        if relative not in texts:
            continue
        try:
            if python_invokes_go(texts[relative]):
                blockers.append(("production-entry-invokes-go", relative,
                                 "build or release helper executes the Go toolchain"))
        except (SyntaxError, ValueError) as error:
            blockers.append(("production-entry-unverified", relative,
                             "build/release helper cannot be parsed: " + str(error)))

    for relative, inspect in (("mise.toml", mise_requires_go), (".tool-versions", tool_versions_requires_go)):
        path = root / relative
        if not path.exists():
            continue
        try:
            if inspect(path.read_text(encoding="utf-8")):
                blockers.append(("tool-manager-requires-go", relative, "tool manager installs/selects or executes Go"))
        except (OSError, ValueError) as error:
            blockers.append(("tool-manager-unverified", relative, "tool-manager configuration cannot be verified: " + str(error)))

    workflows = root / ".github/workflows"
    if workflows.exists():
        for path in sorted(workflows.rglob("*")):
            if not path.is_file() or path.suffix not in {".yml", ".yaml"}:
                continue
            relative = path.relative_to(root).as_posix()
            try:
                if workflow_requires_go(path.read_text(encoding="utf-8")):
                    blockers.append(("ci-requires-go", relative, "CI workflow installs/selects or executes the Go toolchain"))
            except (OSError, ValueError) as error:
                blockers.append(("ci-workflow-unverified", relative, "workflow cannot be verified: " + str(error)))

    sources, manifests = [], []
    def walk_error(error):
        blockers.append(("source-tree-unverified", str(error.filename), "cannot inspect source tree: " + str(error)))
    for directory, dirs, files in os.walk(root, followlinks=False, onerror=walk_error):
        # Git object storage is metadata; all source/vendor/tool directories count.
        dirs[:] = sorted(name for name in dirs if name != ".git")
        for name in dirs + sorted(files):
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                blockers.append(("source-tree-unverified", relative, "source symlink cannot be verified"))
            if name.endswith(".go"):
                sources.append(relative)
            if name in {"go.mod", "go.sum", "go.work", "go.work.sum"}:
                manifests.append(relative)
    if sources:
        blockers.append(("go-sources-remain", ", ".join(sorted(sources)), f"{len(sources)} Go source file(s) remain, including tests/vendor/tooling"))
    if manifests:
        blockers.append(("go-module-remains", ", ".join(sorted(manifests)), f"{len(manifests)} Go module/workspace manifest or lockfile(s) remain"))
    return blockers


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument(
        "--require-go-free",
        action="store_true",
        help="fail unless the production app, Rust daemon, and Swift launcher have no Go source/toolchain/backend edge",
    )
    args = parser.parse_args()
    blockers = collect_blockers(args.root.resolve())
    if blockers:
        print(f"Go backend exit: BLOCKED ({len(blockers)} build/runtime/source categories)")
        for key, path, reason in blockers:
            print(f"- {key}: {path}: {reason}")
        if args.require_go_free:
            return 1
    else:
        print("Go backend exit: PASS (no Go build, bundle, launch, dispatch, default-gate, source-export, or source edges found)")
    print("This structural check complements route ownership and does not replace app, migration, recovery, or device acceptance.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
