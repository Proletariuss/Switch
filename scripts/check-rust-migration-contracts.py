#!/usr/bin/env python3
"""Offline route/consumer census; a passing baseline is NOT Go-removal approval.

The current source tree must contain no Go source or module/workspace manifests;
this check never executes a Go toolchain. Rust and Swift extraction is limited
to the forms documented in all-rust/README.md. Dynamic dispatch is explicit,
source-fingerprinted review data, never guessed as native.
"""
from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import ast

ROOT = Path(__file__).resolve().parents[1]
BASE = "docs/optimization/all-rust/"
RUST = "rust/crates/proletariusd/src/"
METHODS = {"GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT", "TRACE"}
RAW_STRING_PREFIX = re.compile(r'(?:br|r)(#+)?"')
CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
TOKEN = re.compile(r"[A-Za-z_][A-Za-z_0-9]*|::|=>|==|!=|&&|\|\||->|.")


class ContractError(Exception):
    pass


def lex(text):
    """Small lexical scanner, preserving locations and string tokens, dropping comments."""
    out, i = [], 0
    while i < len(text):
        start = i
        if text[i].isspace():
            i += 1
            continue
        if text.startswith("//", i):
            end = text.find("\n", i)
            i = len(text) if end < 0 else end
            continue
        if text.startswith("/*", i):
            depth, i = 1, i + 2
            while i < len(text) and depth:
                if text.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif text.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            if depth:
                raise ContractError("unterminated block comment")
            continue
        # Match against the original source at an offset. Slicing text[i:] on
        # every token made a full 2.5 MiB Rust source scan quadratic.
        raw = RAW_STRING_PREFIX.match(text, i)
        char = CHAR_LITERAL.match(text, i)
        if char:
            i = char.end()
        elif raw:
            suffix = '"' + (raw.group(1) or "")
            end = text.find(suffix, raw.end())
            if end < 0:
                raise ContractError("unterminated raw string")
            i = end + len(suffix)
        elif text.startswith('"""', i):
            end = text.find('"""', i + 3)
            if end < 0:
                raise ContractError("unterminated multiline string")
            i = end + 3
        elif text[i] == '"':
            i += 1
            while i < len(text):
                if text[i] == "\\":
                    i += 2
                elif text[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            else:
                raise ContractError("unterminated string")
        else:
            m = TOKEN.match(text, i)
            i = m.end()
        out.append((text[start:i], start, i))
    return out


def normalized(text):
    return " ".join(t[0] for t in lex(text))


def digest(text):
    return hashlib.sha256(normalized(text).encode()).hexdigest()


def string_value(token):
    if token.startswith('"') and not token.startswith('"""'):
        try:
            return json.loads(token)
        except ValueError:  # Swift interpolation remains explicit.
            return token[1:-1]
    raw = re.fullmatch(r'(?:br|r)(#*)"(.*)"\1', token, re.S)
    return raw.group(2) if raw else None


def rust_path_literal(token):
    """Decode only reviewed Rust source-path literals, never Swift fallback.

    Ordinary unescaped UTF-8 strings and raw r/# strings have exact path
    identity. Rust escapes (including hex, unicode, escaped backslash/quote and
    line continuation) are deliberately unsupported and fail closed. Raw
    backslashes remain literal filename characters, as required by Rust.
    Byte literals and dynamic expressions are not source-path strings.
    """
    if token.startswith('"') and token.endswith('"'):
        value = token[1:-1]
        if chr(92) in value or '"' in value:
            return None
    else:
        raw = re.fullmatch(r'r(?P<hashes>#{0,255})"(?P<value>.*)"(?P=hashes)', token, re.S)
        if raw is None:
            return None
        value = raw.group(2)
    if not value or any(ord(char) < 32 or ord(char) == 127 for char in value):
        return None
    return value


def balanced(tokens, start, left="{", right="}"):
    depth = 0
    for i in range(start, len(tokens)):
        if tokens[i][0] == left:
            depth += 1
        elif tokens[i][0] == right:
            depth -= 1
            if depth == 0:
                return i
    raise ContractError("unbalanced source scope")


def cfg_test_feature(attr, operator):
    """Only the reviewed two-term test/literal-feature spelling, no evaluator."""
    if len(attr) != 11 or attr[:4] != ["cfg", "(", operator, "("] or attr[-2:] != [")", ")"]:
        return False
    terms = attr[4:-2]
    if terms[:4] == ["test", ",", "feature", "="]:
        token = terms[4]
    elif terms[:2] == ["feature", "="] and terms[3:] == [",", "test"]:
        token = terms[2]
    else:
        return False
    return cfg_literal(token)


def cfg_literal(token):
    # These reviewed declarations use ordinary string literals. Do not guess
    # at macros, byte strings, cfg_attr or Rust-specific string escape builders.
    if not token.startswith('"'):
        return False
    try:
        value = json.loads(token)
    except ValueError:
        return False
    return isinstance(value, str) and bool(value)


def cfg_requires_test(attr):
    """Only cfg(test) and all(test, literal-feature) prove test-only."""
    return attr == ["cfg", "(", "test", ")"] or cfg_test_feature(attr, "all")


def cfg_reviewed_production(attr):
    """Keep every possible production branch; never evaluate host/defaults.

    Reviewed external forms: cfg(feature = "literal"), cfg(target_os =
    "literal"), and cfg(any(test, feature = "literal")) in either term order.
    In particular, any(test, feature) is NOT a test-only source edge.
    """
    if (len(attr) == 6 and attr[:2] == ["cfg", "("]
            and attr[2] in {"feature", "target_os"} and attr[3] == "="
            and attr[-1] == ")" and cfg_literal(attr[4])):
        return True
    return cfg_test_feature(attr, "any")


def rust_modules(text):
    """Parse module declarations and their immediately attached attributes.

    External modules support literal #[path], test-only predicates above,
    and the narrowly reviewed production-possible predicates above. Every
    possible production branch stays visible, independent of this machine or
    default features. Other builders require review. Inline cfg predicates
    remain conservative; cfg_attr cannot hide dynamic paths in either form.
    """
    tokens = lex(text)
    attributes, modules = {}, []
    for i in range(len(tokens) - 1):
        if [t[0] for t in tokens[i:i+2]] == ["#", "["]:
            end = balanced(tokens, i+1, "[", "]")
            attributes[end] = (i, [t[0] for t in tokens[i+2:end]])
    for i in range(len(tokens) - 2):
        if tokens[i][0] != "mod" or not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", tokens[i+1][0]):
            continue
        external = tokens[i+2][0] == ";"
        if not external and tokens[i+2][0] != "{":
            continue
        begin, attrs = i, []
        if begin and tokens[begin-1][0] == "pub":
            begin -= 1
        elif begin and tokens[begin-1][0] == ")":
            # Visibility such as pub(crate), not an arbitrary preceding call.
            j = begin - 2
            while j >= 0 and tokens[j][0] != "(":
                j -= 1
            if j > 0 and tokens[j-1][0] == "pub":
                begin = j - 1
        while begin-1 in attributes:
            begin, attr = attributes[begin-1]
            attrs.append(attr)
        test, path = False, None
        for attr in attrs:
            if cfg_requires_test(attr):
                test = True
            elif attr and attr[0] == "cfg_attr":
                raise ContractError("Rust module cfg_attr needs scanner review")
            elif attr and attr[0] == "cfg" and external:
                if not cfg_reviewed_production(attr):
                    raise ContractError("external Rust module conditional needs scanner review")
            elif attr and attr[0] == "path":
                if len(attr) != 3 or attr[1] != "=" or rust_path_literal(attr[2]) is None:
                    raise ContractError("external Rust module path must be one literal")
                if path is not None:
                    raise ContractError("duplicate external Rust module path")
                path = rust_path_literal(attr[2])
        end = i+2 if external else balanced(tokens, i+2)
        modules.append({"name": tokens[i+1][0], "test": test, "path": path,
                        "external": external, "start": tokens[begin][1], "end": tokens[end][2]})
    return tokens, modules


def production(text, language):
    if language != "rust":
        return text
    # Both inline and explicit external cfg(test) modules are excluded here.
    # The latter's source files are separately resolved by rust_test_sources.
    _, modules = rust_modules(text)
    erased = list(text)
    for module in modules:
        if module["test"]:
            for j in range(module["start"], module["end"]):
                if erased[j] != "\n":
                    erased[j] = " "
    return "".join(erased)


def rust_test_sources(root, texts):
    """Exclude only sources proved test-only by explicit module/include edges.

    Files named tests.rs are not inherently tests. A production reference wins
    over a test reference; unreferenced source also remains conservatively in
    the census. Never exclude an entire directory just because a parent is a
    test module.
    """
    edges = []
    known = {(root / file).resolve(): file for file in texts}

    def resolve(parent, candidates):
        found = [known[p.resolve()] for p in candidates if p.resolve() in known]
        if len(found) != 1:
            raise ContractError(f"external Rust module source is missing or ambiguous: {parent}")
        return found[0]

    for file, text in texts.items():
        if not re.search(r"\bmod\s+\w+\s*;|\binclude\s*!", text):
            continue
        tokens, modules = rust_modules(text)
        parent = root / file
        test_spans = [(m["start"], m["end"]) for m in modules if m["test"]]
        is_test = lambda offset: any(start <= offset < end for start, end in test_spans)
        for module in modules:
            if not module["external"]:
                continue
            path = module["path"]
            if path is not None:
                if Path(path).is_absolute() or ".." in Path(path).parts:
                    raise ContractError("external Rust module path escapes the reviewed source tree")
                candidates = [parent.parent / path]
            else:
                if any(not m["external"] and m["start"] < module["start"] < m["end"] for m in modules):
                    raise ContractError("nested external Rust module needs explicit path review")
                base = parent.parent if parent.name in {"main.rs", "lib.rs", "mod.rs"} else parent.with_suffix("")
                candidates = [base / (module["name"] + ".rs"), base / module["name"] / "mod.rs"]
            edges.append((file, resolve(file, candidates), is_test(module["start"])))
        for i in range(len(tokens) - 1):
            if [t[0] for t in tokens[i:i+2]] != ["include", "!"]:
                continue
            if i+2 >= len(tokens) or tokens[i+2][0] != "(":
                raise ContractError("Rust include source delimiter needs scanner review")
            if i+4 >= len(tokens) or (path := rust_path_literal(tokens[i+3][0])) is None or tokens[i+4][0] != ")":
                raise ContractError("Rust include source needs a literal path for test classification")
            if Path(path).is_absolute() or ".." in Path(path).parts:
                raise ContractError("Rust include path escapes the reviewed source tree")
            edges.append((file, resolve(file, [parent.parent / path]), is_test(tokens[i][1])))

    def reachable(seeds, production_only):
        seen, pending = set(seeds), list(seeds)
        while pending:
            node = pending.pop()
            for parent, child, test in edges:
                if parent == node and not (production_only and test) and child not in seen:
                    seen.add(child)
                    pending.append(child)
        return seen

    candidates = reachable({child for _, child, test in edges if test}, False)
    production_nodes = reachable(set(texts) - candidates, True)
    return candidates - production_nodes


def function_scope(text, name):
    tokens = lex(text)
    for i in range(len(tokens) - 1):
        if tokens[i][0] in {"fn", "func"} and tokens[i+1][0] == name:
            start = next((j for j in range(i+2, len(tokens)) if tokens[j][0] == "{"), None)
            if start is None:
                break
            end = balanced(tokens, start)
            return text[tokens[i][1]:tokens[end][2]], tokens[i][1]
    raise ContractError(f"review scope function disappeared: {name}")


def source(file, text, offset, **extra):
    return {"file": file, "line": text.count("\n", 0, offset)+1, **extra}


def read(root, file):
    return (root / file).read_text()


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


def go_census(root):
    """Verify Go absence before returning an empty *current* Go census.

    No frozen registrations are treated as live handlers. Historical injected
    census rows below exist solely for negative unit fixtures. Reintroducing
    even test/vendor/tool Go code requires a deliberate checker policy review.
    """
    if not root.is_dir():
        raise ContractError("source root is missing or is not a directory")
    for relative in (RUST + "main.rs", RUST + "ownership.rs"):
        path = root / relative
        if not path.is_file() or not path.read_text().strip():
            raise ContractError("source root missing or empty required source: " + relative)
    if not (root / "app/Sources").is_dir():
        raise ContractError("source root missing Swift source directory")
    try:
        if not switch_workspace_members(read(root, "rust/Cargo.toml")):
            raise ValueError("missing or malformed literal Switch workspace members")
    except (OSError, ValueError) as error:
        raise ContractError("source root has missing/malformed Rust workspace: " + str(error)) from error

    forbidden = []
    def fail(error):
        raise ContractError("cannot inspect source tree: " + str(error))
    for directory, dirs, files in os.walk(root, followlinks=False, onerror=fail):
        # Git object storage is repository metadata, not application source.
        dirs[:] = sorted(name for name in dirs if name != ".git")
        for name in dirs + sorted(files):
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                raise ContractError("source tree symlink cannot be verified: " + relative)
            if name.endswith(".go") or name in {"go.mod", "go.sum", "go.work", "go.work.sum"}:
                forbidden.append(relative)
    if forbidden:
        raise ContractError("Go source/module reintroduced or not retired: " + ", ".join(sorted(forbidden)))
    return {"registrations": [], "references": []}


def exact_rust_routes(text, file):
    tokens, result = lex(text), []
    # Production tuple arms: (&Method::GET, "/api/x" | "/api/y",).
    for i in range(len(tokens)-7):
        if [t[0] for t in tokens[i:i+4]] != ["(", "&", "Method", "::"]:
            continue
        method = tokens[i+4][0]
        if method not in METHODS or tokens[i+5][0] != ",":
            continue
        j, values = i+6, []
        while j < len(tokens):
            value = string_value(tokens[j][0])
            if value is None:
                break
            values.append((value, tokens[j][1]))
            j += 1
            if tokens[j][0] != "|":
                break
            j += 1
        if tokens[j][0] == ",":
            j += 1
        if tokens[j][0] == ")":
            for path, offset in values:
                if path.startswith("/api/"):
                    result.append({"method": method, "path": path, "source": source(file, text, offset)})
    return result


def owned_exact(text):
    result = set()
    code = normalized(function_scope(text, "route_owner")[0])
    for match in re.finditer(r'method == "([A-Z]+)" && matches ! \( path , (.*?) \)', code):
        for token, _, _ in lex(match.group(2)):
            path = string_value(token)
            if path and path.startswith("/api/"):
                result.add((match.group(1), path))
    return result


def explicit_go_compatibility_routes(text):
    """Read the exact Go compatibility allowlist, never a prefix wildcard."""
    tokens = lex(text)
    starts = [i for i in range(len(tokens)-1)
              if tokens[i][0] == "const" and tokens[i+1][0] == "GO_COMPATIBILITY_ROUTES"]
    if len(starts) != 1:
        raise ContractError("ownership needs one literal GO_COMPATIBILITY_ROUTES table")
    i = starts[0] + 2
    # Skip the type annotation (which itself contains `&[...]`) and locate
    # the assignment whose right-hand side borrows the literal array.
    while i + 2 < len(tokens) and not (
        tokens[i][0] == "=" and tokens[i+1][0] == "&" and tokens[i+2][0] == "["
    ):
        i += 1
    if i + 2 >= len(tokens):
        raise ContractError("GO_COMPATIBILITY_ROUTES has no initializer")
    i += 3
    routes = []
    while i < len(tokens) and tokens[i][0] != "]":
        if tokens[i][0] != "(":
            raise ContractError("compatibility entries must be (method, path) tuples")
        i += 1
        if i >= len(tokens) or (method := string_value(tokens[i][0])) not in METHODS:
            raise ContractError("compatibility method must be a supported literal")
        i += 1
        if i >= len(tokens) or tokens[i][0] != ",":
            raise ContractError("compatibility tuple is missing its comma")
        i += 1
        if i >= len(tokens) or (path := string_value(tokens[i][0])) is None or not path.startswith("/api/"):
            raise ContractError("compatibility path must be an API literal")
        i += 1
        if i < len(tokens) and tokens[i][0] == ",":
            i += 1
        if i >= len(tokens) or tokens[i][0] != ")":
            raise ContractError("compatibility tuple is unterminated")
        i += 1
        routes.append((method, path))
        if i < len(tokens) and tokens[i][0] == ",":
            i += 1
        elif i >= len(tokens) or tokens[i][0] != "]":
            raise ContractError("compatibility entries must be comma separated")
    if i+1 >= len(tokens) or [tokens[i][0], tokens[i+1][0]] != ["]", ";"]:
        raise ContractError("GO_COMPATIBILITY_ROUTES initializer is unterminated")
    if len(routes) != len(set(routes)):
        raise ContractError("GO_COMPATIBILITY_ROUTES contains duplicate method/path entries")
    return set(routes)



def public_policy_rows(text, file):
    """Read the compiled literal ROUTES policy, never treat it as a handler.

    Supported form is deliberately narrow: const ROUTES: &[(&str, &str)] =
    &[("/api/path", "GET, HEAD"), ...]. Unknown builders fail review rather than
    silently producing an incomplete allow-list.
    """
    tokens = lex(text)
    declarations = [i for i in range(len(tokens)-1)
                    if tokens[i][0] == "const" and tokens[i+1][0] == "ROUTES"]
    if len(declarations) != 1:
        raise ContractError("public HTTP policy needs exactly one literal const ROUTES")
    i = declarations[0] + 2

    def expect(value):
        nonlocal i
        if i >= len(tokens) or tokens[i][0] != value:
            raise ContractError("unsupported public HTTP ROUTES syntax; update extractor and negative fixtures")
        i += 1

    for value in [":", "&", "[", "(", "&", "str", ",", "&", "str", ")", "]", "=", "&", "["]:
        expect(value)
    rows = []
    while i < len(tokens) and tokens[i][0] != "]":
        expect("(")
        if i >= len(tokens) or (path := string_value(tokens[i][0])) is None:
            raise ContractError("public HTTP policy path must be a literal")
        offset = tokens[i][1]
        i += 1
        expect(",")
        if i >= len(tokens) or (methods := string_value(tokens[i][0])) is None:
            raise ContractError("public HTTP policy methods must be a literal")
        i += 1
        if i < len(tokens) and tokens[i][0] == ",":
            i += 1
        expect(")")
        rows.append({"path": path, "allow": methods, "source": source(file, text, offset)})
        if i < len(tokens) and tokens[i][0] == ",":
            i += 1
        elif i >= len(tokens) or tokens[i][0] != "]":
            raise ContractError("public HTTP policy entries must be comma separated")
    expect("]")
    expect(";")
    return rows



def public_legacy_paths(text, file):
    tokens = lex(text)
    positions = [i for i in range(len(tokens)-1)
                 if tokens[i][0] == "const" and tokens[i+1][0] == "LEGACY_METHOD_PATHS"]
    if not positions:
        return []
    if len(positions) != 1:
        raise ContractError("duplicate LEGACY_METHOD_PATHS declaration")
    i = positions[0] + 2
    for value in [":", "&", "[", "&", "str", "]", "=", "&", "["]:
        if i >= len(tokens) or tokens[i][0] != value:
            raise ContractError("unsupported LEGACY_METHOD_PATHS syntax")
        i += 1
    result = []
    while i < len(tokens) and tokens[i][0] != "]":
        value = string_value(tokens[i][0])
        if value is None:
            raise ContractError("LEGACY_METHOD_PATHS must contain only literal paths")
        result.append({"path": value, "source": source(file, text, tokens[i][1])})
        i += 1
        if i < len(tokens) and tokens[i][0] == ",":
            i += 1
        elif i >= len(tokens) or tokens[i][0] != "]":
            raise ContractError("LEGACY_METHOD_PATHS entries must be comma separated")
    if i+1 >= len(tokens) or [tokens[i][0], tokens[i+1][0]] != ["]", ";"]:
        raise ContractError("unterminated LEGACY_METHOD_PATHS")
    return result

def public_path_shape(path):
    """Exact path or one named, nonempty final segment; never a prefix wildcard.

    This describes policy matching only. Invalid/extra tails keep the existing
    handler's fallback/validation and are not newly certified HEAD aliases.
    """
    if not path.startswith("/api/") or any(c.isspace() for c in path) or any(c in path for c in "?#*"):
        return None
    parts = path.split("/")
    if any(not part for part in parts[1:]):
        return None
    normalized_parts = []
    for index, part in enumerate(parts):
        if re.fullmatch(r"\{[A-Za-z_][A-Za-z_0-9]*\}", part):
            if index != len(parts) - 1:
                return None
            normalized_parts.append("{}")
        elif "{" in part or "}" in part:
            return None
        else:
            normalized_parts.append(part)
    return "/".join(normalized_parts)


def public_method_policy(root, routes):
    file = RUST + "public_http.rs"
    if not (root / file).exists():
        return [], [], []
    text = production(read(root, file), "rust")
    rows = public_policy_rows(text, file)
    errors, aliases, seen = [], [], set()
    native = {public_path_shape(row["path"]) for row in routes.values()
              if row["owner"] == "rust" and "/internal/" not in row["path"]}
    if None in native:
        errors.append("public Rust route uses an unsupported method-policy path shape")
        native.remove(None)
    for row in rows:
        path, raw = row["path"], row["allow"]
        shape = public_path_shape(path)
        if shape is None:
            errors.append(f"invalid public HTTP policy path: {path}")
            continue
        if "/internal/" in path:
            errors.append(f"internal path must not enter public HTTP policy: {path}")
            continue
        if shape in seen:
            errors.append(f"duplicate public HTTP policy path: {path}")
            continue
        seen.add(shape)
        methods = raw.split(", ") if raw else []
        if not methods or any(method not in METHODS for method in methods):
            errors.append(f"invalid public HTTP policy method list: {path}: {raw}")
        if len(methods) != len(set(methods)):
            errors.append(f"duplicate public HTTP policy method: {path}: {raw}")
        if methods != sorted(set(methods)):
            errors.append(f"public HTTP Allow must use unique sorted methods: {path}: {raw}")
        if "HEAD" in methods and "GET" not in methods:
            errors.append(f"public HTTP HEAD alias requires GET: {path}")
        canonical = [entry for entry in routes.values()
                     if public_path_shape(entry["path"]) == shape]
        if shape not in native:
            errors.append(f"public HTTP policy has no Rust handler: {path}")
        expected = {entry["method"] for entry in canonical}
        if "GET" in expected:
            expected.add("HEAD")
        if set(methods) != expected:
            errors.append(f"public HTTP policy methods differ from real handlers: {path}: "
                          f"expected={', '.join(sorted(expected))}; policy={raw}")
        existing_head = next((entry for entry in canonical if entry["method"] == "HEAD"), None)
        get = next((entry for entry in canonical if entry["method"] == "GET"), None)
        if existing_head is not None:
            # The current runtime policy always aliases HEAD to GET. A future
            # independently implemented HEAD needs an explicit dispatch policy.
            errors.append(f"explicit HEAD handler requires public policy review: {path}")
        elif "HEAD" in methods and get is not None:
            aliases.append({"canonical": get["id"], "source": row["source"], "policy_path": path})
    for shape in sorted(native - seen):
        errors.append(f"public Rust handler missing HTTP method policy: {shape}")
    legacy_seen = set()
    for entry in public_legacy_paths(text, file):
        path = entry["path"]
        if "/internal/" in path or path not in {row["path"] for row in rows}:
            errors.append(f"legacy method path is not a public ROUTES member: {path}")
        if path in legacy_seen:
            errors.append(f"duplicate legacy method path: {path}")
        legacy_seen.add(path)
    return rows, aliases, errors

def call_arguments(tokens, start):
    end = balanced(tokens, start, "(", ")")
    depth, args, begin = 0, [], start+1
    for i in range(start+1, end):
        t = tokens[i][0]
        if t in {"(", "[", "{"}:
            depth += 1
        elif t in {")", "]", "}"}:
            depth -= 1
        elif t == "," and depth == 0:
            args.append(tokens[begin:i]); begin = i+1
    args.append(tokens[begin:end])
    return args


def swift_calls(text, file, review):
    tokens, result = lex(text), []
    constants = {m.group(1): m.group(2) for m in re.finditer(r'let\s+(\w+)\s*=\s*"(api/[^"\\]*)"', text)}
    for i in range(len(tokens)-3):
        if tokens[i][0] != "." or tokens[i+1][0] not in {"get", "getData", "send", "upload"} or tokens[i+2][0] != "(":
            continue
        args = call_arguments(tokens, i+2)
        if not args[0] or (len(args[0]) > 1 and args[0][1][0] == ":"):
            continue  # Labeled model operations (send(phone:), get(id:)).
        expression = " ".join(t[0] for t in args[0])
        non_http = review.get("non_http_calls", {}).get(file, {}).get(expression)
        if non_http:
            result.append({"source": source(file, text, tokens[i][1]), "api_method": tokens[i+1][0],
                           "expression": expression, "method": None, "paths": [],
                           "resolution": "reviewed_non_http", "note": non_http})
            continue
        method = "GET" if tokens[i+1][0] in {"get", "getData"} else "POST"
        for arg in args[1:]:
            if len(arg) >= 3 and arg[0][0] == "method" and arg[1][0] == ":":
                method = string_value(arg[2][0]) if len(arg) == 3 else None
        paths, manual = [], False
        value = string_value(args[0][0][0])
        if value is not None:
            for name, prefix in constants.items():
                value = value.replace("\\("+name+")", prefix)
            if value.startswith(("api/", "/api/")):
                value = re.sub(r"\\\(([^()]*)\)", r"{\1}", value)
                # A trailing concatenation may change the route, not just query.
                if len(args[0]) > 1 and "?" not in value:
                    manual = True
                else:
                    paths = ["/" + value.lstrip("/").split("?", 1)[0]]
        if not paths:
            manual = True
            binding = review.get("swift_dynamic_calls", {}).get(file, {}).get(expression)
            if binding is not None:
                paths = binding
        result.append({"source": source(file, text, tokens[i][1]), "api_method": tokens[i+1][0],
                       "expression": expression, "method": method, "paths": paths,
                       "resolution": "reviewed_dynamic" if manual else "literal_or_interpolated"})
    return result


def category(path):
    for fragment, name in [("sms/forwarding", "sms_forwarding"), ("multistandby", "n_wait"),
                           ("device-send", "sms_ledger"), ("sms", "sms"), ("esim", "esim"),
                           ("call", "voice"), ("vowifi", "vowifi"), ("reader", "reader"),
                           ("network", "network"), ("module", "module")]:
        if fragment in path:
            return name
    return "runtime_diagnostics"


def compatible(template, actual):
    regex = re.escape(template)
    regex = re.sub(r"\\\{[^}]*\\\}", "[^/]+", regex)
    # Compare interpolated client paths as one opaque component.
    actual = re.sub(r"\{[^}]*\}", "_value_", actual)
    return re.fullmatch(regex, actual) is not None


def explicit_unknown_route_404(ready_scope):
    """Recognize only the reviewed legacy owner arm or native terminal arm."""
    if "RouteOwner :: Unknown =>" in ready_scope and "route_not_found" in ready_scope:
        return True
    # The native daemon's final wildcard arm returns this exact JSON 404,
    # followed directly by the match close and Ok(response). A similar body
    # in an earlier branch, comment, string, or arbitrary fallback is not proof.
    terminal = normalized('''_ => json_response(
        StatusCode::NOT_FOUND,
        &serde_json::json!({"code":"route_not_found","error":"route not found"}),
    ),
    };
    Ok(response)
    }''')
    return ready_scope.endswith(terminal)


def build_inventory(root, review, go=None):
    go = go if go is not None else go_census(root)
    errors, scopes, routes, references, consumers = [], [], {}, [], []
    rust_texts = {path.relative_to(root).as_posix(): path.read_text()
                  for path in sorted((root / RUST).rglob("*.rs"))}
    excluded_test_sources = rust_test_sources(root, rust_texts)
    for item in review["scopes"]:
        file = item["file"]
        if file in excluded_test_sources:
            errors.append("production review scope points to a test-only source: " + file)
            continue
        text = production(read(root, file), "rust" if file.endswith(".rs") else "swift")
        scope, offset = function_scope(text, item["function"]) if item.get("function") else (text, 0)
        actual = hashlib.sha256(scope.encode()).hexdigest() if item.get("fingerprint") == "sha256-raw" else digest(scope)
        if actual != item["sha256"]:
            errors.append(f"MANUAL REVIEW REQUIRED: {file}:{item.get('function', 'whole production file')} changed; actual SHA256={actual}")
        scopes.append({**item, "source": source(file, text, offset)})

    def add(method, path, owner, origin, evidence):
        key = method + " " + path
        row = routes.setdefault(key, {"id": key, "method": method, "path": path,
            "category": category(path), "audience": "internal_backend" if "/internal/" in path else "app_or_local_client",
            "owner": owner, "migration_status": "go_compatibility_pending" if owner == "go" else "rust_handler_present_end_to_end_not_certified",
            "sources": [], "consumers": [], "go_dependencies": []})
        row["sources"].append({"kind": origin, **evidence})
        if owner == "rust":
            row["owner"], row["migration_status"] = "rust", "rust_handler_present_end_to_end_not_certified"

    for r in go["registrations"]:
        if r["dynamic"]:
            errors.append(f"UNRESOLVED Go registration: {r['source']} {r['expression']}")
            continue
        match = re.fullmatch(r"([A-Z]+) (/api/\S+)", r["pattern"])
        if not match or match.group(1) not in METHODS:
            errors.append(f"UNSUPPORTED Go mux pattern requires review: {r['pattern']}")
            continue
        add(*match.groups(), "go", "go_ast_registration", {**r["source"], "handler": r["handler"]})
    for ref in go["references"]:
        references.append({"language": "go", **ref})

    owner_text = production(read(root, RUST+"ownership.rs"), "rust")
    owned = owned_exact(owner_text)
    extracted = set()
    for file, raw_text in rust_texts.items():
        if file in excluded_test_sources:
            continue
        text = production(raw_text, "rust")
        for token, offset, _ in lex(text):
            value = string_value(token)
            if value and "/api/" in value:
                references.append({"language": "rust", "source": source(file, text, offset), "value": value})
        if file == RUST+"main.rs":
            for r in exact_rust_routes(text, file):
                add(r["method"], r["path"], "rust", "rust_tuple_dispatch", r["source"])
                extracted.add((r["method"], r["path"]))
                if (r["method"], r["path"]) not in owned:
                    errors.append(f"Rust handler missing ownership declaration: {r['method']} {r['path']}")
    for r in review["dynamic_routes"]:
        file, fn = r["file"], r["function"]
        if file in excluded_test_sources:
            errors.append(f"dynamic production route points to a test-only source: {r['method']} {r['path']}")
            continue
        text = production(read(root, file), "rust")
        _, offset = function_scope(text, fn)
        if not any(s["file"] == file and (s.get("function") == fn or not s.get("function")) for s in scopes):
            errors.append(f"dynamic route lacks fingerprinted scope: {r['method']} {r['path']}")
        add(r["method"], r["path"], "rust", "reviewed_dynamic_dispatch",
            source(file, text, offset, function=fn, note=r["note"]))
        extracted.add((r["method"], r["path"]))
    for method, path in sorted(owned - extracted):
        errors.append(f"Rust ownership without recorded handler: {method} {path}")

    method_policy, head_aliases, policy_errors = public_method_policy(root, routes)
    errors.extend(policy_errors)
    policy_file = RUST + "public_http.rs"
    if (root / policy_file).exists() and not any(
        scope["file"] == policy_file and not scope.get("function") for scope in scopes
    ):
        errors.append("public HTTP policy requires a whole-production-file fingerprint: " + policy_file)
    for alias in head_aliases:
        parent = routes[alias["canonical"]]
        add("HEAD", parent["path"], parent["owner"], "public_get_head_alias",
            {**alias["source"], "alias_of": parent["id"], "policy_path": alias["policy_path"]})
        routes["HEAD " + parent["path"]]["alias_of"] = parent["id"]

    for path in sorted((root / "app/Sources").rglob("*.swift")):
        file = path.relative_to(root).as_posix()
        text = path.read_text()
        consumers.extend(swift_calls(text, file, review))
        for token, offset, _ in lex(text):
            value = string_value(token)
            if value and ("/api/" in value or value.startswith("api/")):
                references.append({"language": "swift", "source": source(file, text, offset), "value": value})
    for c in consumers:
        if c["resolution"] in {"reviewed_dynamic", "reviewed_non_http"} and not any(
            s["file"] == c["source"]["file"] and not s.get("function") for s in scopes
        ):
            errors.append(f"reviewed Swift call lacks file fingerprint: {c['source']}")
        if c["resolution"] == "reviewed_non_http":
            continue
        if not c["paths"] or c["method"] not in METHODS:
            errors.append(f"UNRESOLVED Swift consumer: {c['source']} {c['expression']}")
        for path in c["paths"]:
            matches = [row for row in routes.values() if c["method"] == row["method"] and compatible(row["path"], path)]
            if not matches:
                errors.append(f"Swift consumer has no route: {c['method']} {path} at {c['source']}")
            for row in matches:
                row["consumers"].append({**c["source"], "kind": "swift_api_client", "resolution": c["resolution"]})
    for group in ("swift_dynamic_calls", "non_http_calls"):
        for file, bindings in review.get(group, {}).items():
            expressions = {c["expression"] for c in consumers if c["source"]["file"] == file}
            for expression in bindings.keys() - expressions:
                errors.append(f"stale {group} review: {file}: {expression}")
    for key, dependencies in review.get("go_dependencies", {}).items():
        if key not in routes:
            errors.append("stale dependency annotation: " + key)
        else:
            routes[key]["go_dependencies"] = dependencies
    for row in routes.values():
        if row["owner"] == "go":
            row["go_dependencies"] = ["Go implementation and compatibility forwarding"]
        if not row["consumers"]:
            row["consumer_coverage"] = "no_direct_swift_call_found; internal/backend/test clients require review"
        else:
            row["consumer_coverage"] = "direct_swift_calls_recorded; not a runtime reachability proof"
    # Alias provenance follows the implemented GET, including all reviewed Go
    # business dependencies. HEAD never becomes an independent native capability.
    for row in routes.values():
        if row.get("alias_of"):
            parent = routes[row["alias_of"]]
            inherited = list(parent["go_dependencies"])
            explicit = review.get("go_dependencies", {}).get(row["id"])
            if explicit is not None and explicit != inherited:
                errors.append("HEAD dependency annotation differs from GET: " + row["id"])
            row["go_dependencies"] = inherited
            row["migration_status"] = "get_alias_no_additional_implementation"

    try:
        explicit_go_routes = explicit_go_compatibility_routes(owner_text)
    except ContractError as error:
        errors.append(str(error))
        explicit_go_routes = set()
    inventoried_go_routes = {
        (row["method"], row["path"])
        for row in routes.values()
        if row["owner"] == "go" and not row.get("alias_of")
    }
    if explicit_go_routes != inventoried_go_routes:
        missing = sorted(inventoried_go_routes - explicit_go_routes)
        extra = sorted(explicit_go_routes - inventoried_go_routes)
        errors.append(
            "Go compatibility allowlist differs from extracted Go route owners; "
            f"missing={missing}, extra={extra}"
        )

    main = read(root, RUST+"main.rs")
    ready, _ = function_scope(main, "handle_ready_request")
    owner_scope = normalized(function_scope(owner_text, "route_owner")[0])
    ready_scope = normalized(ready)
    fallback = {
        "ownership_default_go": owner_scope.endswith("RouteOwner :: GoCompatibility }"),
        "gateway_forwards_to_go": (
            "RouteOwner :: GoCompatibility =>" in ready_scope
            and "upstream :: forward" in ready_scope
        ),
        "unknown_route_policy": (
            "explicit Rust 404"
            if explicit_unknown_route_404(ready_scope)
            else "requires explicit review"
        ),
        "explicit_go_route_count": len(explicit_go_routes),
    }
    return {"schema_version": 1, "scope": review["scope"], "fallback": fallback,
            "routes": [routes[k] for k in sorted(routes)], "swift_calls": consumers,
            "source_api_references": references, "manual_review_scopes": scopes,
            "public_method_policy": method_policy,
            "excluded_rust_test_sources": sorted(excluded_test_sources)}, errors


def encode(value):
    return json.dumps(value, ensure_ascii=False, indent=2) + "\n"


def compare(expected, actual):
    if expected == actual:
        return []
    old = {r["id"] for r in expected.get("routes", [])}
    new = {r["id"] for r in actual.get("routes", [])}
    changes = ["contract census changed; review the diff, then explicitly --write-inventory"]
    changes += ["added route: "+r for r in sorted(new-old)]
    changes += ["removed route: "+r for r in sorted(old-new)]
    return changes


def rust_only_errors(inventory):
    errors = []
    if inventory["fallback"].get("ownership_default_go"):
        errors.append("Rust-only blocked: unknown routes default to Go")
    if inventory["fallback"].get("gateway_forwards_to_go"):
        errors.append("Rust-only blocked: explicit Go compatibility forwarding remains")
    if inventory["fallback"].get("unknown_route_policy") != "explicit Rust 404":
        errors.append("Rust-only blocked: unknown route policy is not explicit Rust 404")
    pending = [r["id"] for r in inventory["routes"] if r["owner"] == "go" or r["go_dependencies"]]
    if pending:
        errors.append(f"Rust-only blocked: {len(pending)} route(s) still have Go ownership/dependencies")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--write-inventory", action="store_true", help="explicitly refresh census AFTER manual review; cannot bless changed review fingerprints")
    parser.add_argument("--require-rust-only", action="store_true", help="additional route-layer exit gate; not the complete build/device acceptance gate")
    parser.add_argument("--diff", action="store_true")
    args = parser.parse_args()
    try:
        review = json.loads(read(args.root, BASE+"api-contract-review.json"))
        actual, errors = build_inventory(args.root, review)
        destination = args.root / (BASE+"api-contract-inventory.json")
        if args.require_rust_only:
            errors += rust_only_errors(actual)
        if not errors and args.write_inventory:
            destination.write_text(encode(actual))
        elif not args.write_inventory:
            expected = json.loads(destination.read_text())
            errors += compare(expected, actual)
            if args.diff and expected != actual:
                sys.stdout.writelines(difflib.unified_diff(encode(expected).splitlines(True), encode(actual).splitlines(True), fromfile="recorded", tofile="current"))
        counts = {owner: sum(r["owner"] == owner for r in actual["routes"]) for owner in ("rust", "go")}
        http_calls = sum(c["resolution"] != "reviewed_non_http" for c in actual["swift_calls"])
        print(f"Census: {len(actual['routes'])} method/path contracts; declared Rust HTTP owner={counts['rust']}, Go owner={counts['go']}; {http_calls} Swift HTTP call sites; {len(actual['manual_review_scopes'])} fingerprinted review scopes.")
        aliases = sum(bool(row.get("alias_of")) for row in actual["routes"])
        if aliases:
            print(f"Includes {aliases} HEAD alias contract(s); no additional handler implementation or migration credit.")
        print("P0 baseline consistency only; Rust handlers are not certified end-to-end Go-free.")
        if errors:
            for error in errors:
                print("FAIL: " + error, file=sys.stderr)
            return 1
        print("PASS: recorded route/consumer census matches current source.")
        return 0
    except (ContractError, OSError, ValueError) as error:
        print("FAIL: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
