#!/usr/bin/env python3
"""Offline negative fixtures: never import a backend package or use sockets."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SOURCE = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("contracts", SOURCE / "scripts/check-rust-migration-contracts.py")
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
BUILD = SOURCE.parent / "build/optimization/all-rust-contract-tool/tests"
BUILD.mkdir(parents=True, exist_ok=True)


class CensusTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=BUILD)
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.put("backend/server.go", '''package main
import "net/http"
func routes(mux *http.ServeMux) { mux.HandleFunc("GET /api/status", handler) }
''')
        self.put(c.RUST+"ownership.rs", '''const GO_COMPATIBILITY_ROUTES: &[(&str, &str)] = &[
("GET", "/api/status"),
];
fn route_owner(method: &str, path: &str) -> RouteOwner {
if method == "GET" && matches!(path, "/api/v2/runtime") { return RouteOwner::Rust; }
if GO_COMPATIBILITY_ROUTES.contains(&(method, path)) { return RouteOwner::GoCompatibility; }
RouteOwner::Unknown
}''')
        self.put(c.RUST+"main.rs", '''fn handle_request() {
match (request.method(), request.uri().path()) { (&Method::GET, "/api/v2/runtime") => reply(), _ => recover() }
}
fn handle_ready_request() { upstream::forward(request, &state.go_socket_path) }
''')
        self.put("app/Sources/Client.swift", '''func refresh() { try await client.get("api/status?fresh=true") }''')
        self.review = {"scope": {}, "scopes": [], "dynamic_routes": [], "swift_dynamic_calls": {}, "non_http_calls": {}, "go_dependencies": {}}
        self.go = {"registrations": [{"source": {"file": "backend/server.go", "line": 3, "function": "routes"},
                   "pattern": "GET /api/status", "expression": "mux.HandleFunc(...)", "handler": "handler", "dynamic": False}], "references": []}

    def put(self, file, text):
        p = self.root / file
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def build(self):
        return c.build_inventory(self.root, self.review, self.go)

    def put_policy(self, rows=None, legacy=None, suffix=""):
        rows = [("/api/v2/runtime", "GET, HEAD")] if rows is None else rows
        text = "const ROUTES: &[(&str, &str)] = &[\n"
        text += "".join(f"    ({json.dumps(path)}, {json.dumps(methods)}),\n" for path, methods in rows)
        text += "];\n"
        if legacy is not None:
            text += "const LEGACY_METHOD_PATHS: &[&str] = &[\n"
            text += "".join(f"    {json.dumps(path)},\n" for path in legacy)
            text += "];\n"
        text += suffix
        file = c.RUST + "public_http.rs"
        self.put(file, text)
        self.review["scopes"] = [scope for scope in self.review["scopes"] if scope["file"] != file]
        self.review["scopes"].append({"file": file, "sha256": c.digest(c.production(text, "rust"))})

    def add_native(self, method, path):
        file = self.root / (c.RUST + "main.rs")
        file.write_text(file.read_text().replace("_ => recover()", f'(&Method::{method}, "{path}") => reply(), _ => recover()'))
        file = self.root / (c.RUST + "ownership.rs")
        file.write_text(file.read_text().replace("RouteOwner::GoCompatibility", f'if method == "{method}" && matches!(path, "{path}") {{ return RouteOwner::Rust; }}\nRouteOwner::GoCompatibility'))

    def add_go(self, method, path):
        route = copy.deepcopy(self.go["registrations"][0])
        route["pattern"] = f"{method} {path}"
        self.go["registrations"].append(route)
        owner = self.root / (c.RUST+"ownership.rs")
        source = owner.read_text()
        source = source.replace('("GET", "/api/status"),',
                                f'("GET", "/api/status"),\n("{method}", "{path}"),')
        owner.write_text(source)

    def test_baseline_matches_and_go_exit_remains_blocked(self):
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        self.assertEqual(c.compare(inventory, inventory), [])
        self.assertEqual(len(c.rust_only_errors(inventory)), 2)
        self.assertEqual(inventory["routes"][0]["consumers"][0]["file"], "app/Sources/Client.swift")

    def test_added_go_route_fails_baseline_diff(self):
        before, _ = self.build()
        self.add_go("POST", "/api/new-action")
        after, errors = self.build()
        self.assertEqual(errors, [])
        self.assertIn("added route: POST /api/new-action", c.compare(before, after))

    def test_added_rust_handler_without_ownership_fails(self):
        f = self.root / (c.RUST+"main.rs")
        f.write_text(f.read_text().replace("_ => recover()", '(&Method::POST, "/api/new-rust") => reply(), _ => recover()'))
        _, errors = self.build()
        self.assertTrue(any("missing ownership" in e and "POST /api/new-rust" in e for e in errors))

    def test_ownership_without_dispatch_fails(self):
        f = self.root / (c.RUST+"ownership.rs")
        f.write_text(f.read_text().replace('"/api/v2/runtime"', '"/api/v2/runtime" | "/api/missing-handler"'))
        _, errors = self.build()
        self.assertTrue(any("without recorded handler" in e for e in errors))

    def test_dynamic_go_registration_never_silently_passes(self):
        self.go["registrations"][0].update(dynamic=True, expression="mux.HandleFunc(runtimePath(), handler)")
        _, errors = self.build()
        self.assertTrue(any("UNRESOLVED Go registration" in e for e in errors))

    def test_new_swift_client_route_must_exist(self):
        self.put("app/Sources/NewClient.swift", 'func send() { try await APIClient().send("api/new-write") }')
        _, errors = self.build()
        self.assertTrue(any("Swift consumer has no route: POST /api/new-write" in e for e in errors))

    def test_unknown_swift_path_is_not_assumed_successful(self):
        self.put("app/Sources/Client.swift", 'func refresh() { try await client.get(newDynamicPath()) }')
        _, errors = self.build()
        self.assertTrue(any("UNRESOLVED Swift consumer" in e for e in errors))

    def test_manual_review_fingerprint_detects_changed_builder(self):
        file = "app/Sources/Client.swift"
        text = 'func refresh() { let path = "api/status"; try await client.get(path) }'
        self.put(file, text)
        self.review["scopes"] = [{"file": file, "sha256": c.digest(text)}]
        self.review["swift_dynamic_calls"] = {file: {"path": ["/api/status"]}}
        self.assertEqual(self.build()[1], [])
        self.put(file, text.replace('"api/status"', '"api/deleted-route"'))
        self.assertTrue(any("MANUAL REVIEW REQUIRED" in e for e in self.build()[1]))

    def test_raw_sha256_review_scope_handles_go_raw_strings(self):
        text = 'package main\nfunc buildRequest() { payload := `{"method":"INVITE"}`; send(payload) }\n'
        self.put("backend/call_runtime.go", text)
        self.review["scopes"] = [{
            "file": "backend/call_runtime.go",
            "fingerprint": "sha256-raw",
            "sha256": hashlib.sha256(text.encode()).hexdigest(),
        }]
        self.assertEqual(self.build()[1], [])

    def test_dynamic_review_requires_fingerprint(self):
        self.put("app/Sources/Client.swift", 'func refresh() { try await client.get(path) }')
        self.review["swift_dynamic_calls"] = {"app/Sources/Client.swift": {"path": ["/api/status"]}}
        self.assertTrue(any("lacks file fingerprint" in e for e in self.build()[1]))

    def test_comments_and_test_literals_do_not_become_routes(self):
        text = '''// (&Method::GET, "/api/comment")
fn handle() { match x { (&Method::POST, "/api/a" | "/api/b",) => {}, _ => {} } }
#[cfg(test)] mod tests { fn fake() { let x = (&Method::GET, "/api/test"); } }
'''
        routes = c.exact_rust_routes(c.production(text, "rust"), "fixture.rs")
        self.assertEqual([(r["method"], r["path"]) for r in routes], [("POST", "/api/a"), ("POST", "/api/b")])

    def test_swift_interpolation_query_and_explicit_method(self):
        calls = c.swift_calls('try await client.send("api/v2/sms-export/\\(token)", method: "DELETE")', "x.swift", {})
        self.assertEqual(calls[0]["paths"], ["/api/v2/sms-export/{token}"])
        self.assertEqual(calls[0]["method"], "DELETE")
        self.assertTrue(c.compatible("/api/v2/sms-export/{id}", calls[0]["paths"][0]))
        self.assertFalse(c.compatible("/api/v2/sms-export/{id}", "/api/v2/sms-export/x/extra"))

    def test_rust_raw_strings_are_real_routes_and_not_comment_boundaries(self):
        text = '''fn routes() { match x {
(&Method::POST, r"/api/raw") => {},
(&Method::GET, r#"/api/raw-hash"#) => {},
(&Method::DELETE, r###"/api/raw-three"###) => {},
_ => {} } }
#[cfg(test)] mod different_test_name { let fake = r#"/api/test"#; }
'''
        routes = c.exact_rust_routes(c.production(text, "rust"), "raw.rs")
        self.assertEqual([r["path"] for r in routes], ["/api/raw", "/api/raw-hash", "/api/raw-three"])
        tokens = c.lex('let x = r##"literal // /* \\" quotes"##; /* outside */')
        self.assertTrue(any(t[0].startswith('r##"') for t in tokens))
        self.assertNotIn("outside", [t[0] for t in tokens])

    def test_lexer_scans_large_source_and_preserves_raw_string_tokens(self):
        count = 20_000
        text = 'let message = r#"// retained as a string"#;\n' * count
        tokens = c.lex(text)
        self.assertEqual(sum(token[0].startswith('r#"') for token in tokens), count)

    def test_raw_rust_route_added_to_gateway_fails_ownership(self):
        file = self.root / (c.RUST+"main.rs")
        file.write_text(file.read_text().replace('_ => recover()', '(&Method::GET, r#"/api/new-raw"#) => reply(), _ => recover()'))
        self.assertTrue(any("missing ownership declaration: GET /api/new-raw" in e for e in self.build()[1]))

    def test_character_literal_braces_do_not_truncate_scope(self):
        body, _ = c.function_scope("fn route() { let a = '{'; let b = '}'; finish(); } fn next() {}", "route")
        self.assertIn("finish();", body)
        self.assertNotIn("next", body)

    def native_census_fixture(self):
        (self.root / "backend/server.go").unlink()
        self.put("rust/Cargo.toml", '[workspace]\nmembers = ["crates/proletariusd"]\n')

    def test_current_go_census_needs_no_toolchain_or_backend_directory(self):
        self.native_census_fixture()
        (self.root / "backend").rmdir()
        with mock.patch("subprocess.Popen", side_effect=AssertionError("toolchain process invoked")), \
                mock.patch("os.system", side_effect=AssertionError("shell process invoked")):
            self.assertEqual(c.go_census(self.root), {"registrations": [], "references": []})

    def test_any_go_source_or_module_reintroduction_fails_without_execution(self):
        self.native_census_fixture()
        for name in ["backend/new.go", "new/legacy_test.go", "vendor/lib/file.go",
                     "third_party/renamed/file.go", "scripts/scan.go", "go.mod", "go.sum",
                     "nested/go.mod", "nested/go.sum", "go.work", "go.work.sum"]:
            with self.subTest(name=name):
                self.put(name, "fixture source")
                with self.assertRaisesRegex(c.ContractError, "Go source/module reintroduced"):
                    c.go_census(self.root)
                (self.root / name).unlink()

    def test_go_absence_does_not_bless_missing_or_malformed_roots(self):
        self.native_census_fixture()
        with self.assertRaises(c.ContractError):
            c.go_census(self.root / "missing")
        for manifest in ["", "[workspace", "[package]\nname='other'", "workspace = []",
                         '[workspace]\nmembers = "crates/proletariusd"']:
            with self.subTest(manifest=manifest):
                self.put("rust/Cargo.toml", manifest)
                with self.assertRaises(c.ContractError):
                    c.go_census(self.root)
        self.put("rust/Cargo.toml", '[workspace]\nmembers = ["crates/proletariusd"]\n')
        self.put(c.RUST + "main.rs", "")
        with self.assertRaises(c.ContractError):
            c.go_census(self.root)

    def test_source_symlink_cannot_hide_reintroduced_go(self):
        self.native_census_fixture()
        (self.root / "hidden").symlink_to(self.root / "missing", target_is_directory=True)
        with self.assertRaisesRegex(c.ContractError, "symlink cannot be verified"):
            c.go_census(self.root)

    def test_rust_only_route_gate_requires_no_fallback_or_go_dependencies(self):
        inventory, _ = self.build()
        inventory["fallback"] = {
            "ownership_default_go": False,
            "gateway_forwards_to_go": False,
            "unknown_route_policy": "explicit Rust 404",
        }
        inventory["routes"] = [r for r in inventory["routes"] if r["owner"] == "rust"]
        self.assertEqual(c.rust_only_errors(inventory), [])
        inventory["fallback"]["gateway_forwards_to_go"] = True
        self.assertTrue(any("explicit Go compatibility forwarding" in error for error in c.rust_only_errors(inventory)))
        inventory["fallback"]["gateway_forwards_to_go"] = False
        inventory["fallback"]["ownership_default_go"] = True
        self.assertTrue(any("unknown routes default to Go" in error for error in c.rust_only_errors(inventory)))
        inventory["fallback"]["ownership_default_go"] = False
        inventory["fallback"]["unknown_route_policy"] = "requires explicit review"
        self.assertTrue(any("unknown route policy" in error for error in c.rust_only_errors(inventory)))
        inventory["fallback"]["unknown_route_policy"] = "explicit Rust 404"
        inventory["routes"][0]["go_dependencies"] = ["still calls Go hardware"]
        self.assertTrue(c.rust_only_errors(inventory))

    def test_native_terminal_404_is_recognized_without_accepting_other_fallbacks(self):
        native = '''fn handle_ready_request() {
            let response = match (request.method(), request.uri().path()) {
                (&Method::GET, "/api/status") => native_status(),
                _ => json_response(StatusCode::NOT_FOUND,
                    &serde_json::json!({"code":"route_not_found","error":"route not found"}),
                ),
            };
            Ok(response)
        }'''
        self.assertTrue(c.explicit_unknown_route_404(c.normalized(native)))
        rejected = [
            native.replace("StatusCode::NOT_FOUND", "StatusCode::OK"),
            native.replace('"route_not_found"', '"backend_unavailable"'),
            native.replace("Ok(response)", "upstream::forward(request, socket).await"),
            native.replace("_ => json_response", '(&Method::GET, "/api/missing") => json_response'),
            "fn handle_ready_request() { upstream::forward(request, socket) }",
            "fn handle_ready_request() { arbitrary_fallback() } /* " + native + " */",
        ]
        for source in rejected:
            self.assertFalse(c.explicit_unknown_route_404(c.normalized(source)), source)

    def test_empty_explicit_go_allowlist_remains_a_valid_literal(self):
        self.assertEqual(c.explicit_go_compatibility_routes(
            'const GO_COMPATIBILITY_ROUTES: &[(&str, &str)] = &[];'), set())

    def test_explicit_go_compatibility_routes_are_literal_and_unique(self):
        routes = c.explicit_go_compatibility_routes('''
            const GO_COMPATIBILITY_ROUTES: &[(&str, &str)] = &[
                ("GET", "/api/legacy/read"),
                ("POST", "/api/legacy/write"),
            ];
        ''')
        self.assertEqual(routes, {
            ("GET", "/api/legacy/read"),
            ("POST", "/api/legacy/write"),
        })
        with self.assertRaises(c.ContractError):
            c.explicit_go_compatibility_routes('const GO_COMPATIBILITY_ROUTES: &[(&str, &str)] = &[ ("GET", "/api/x"), ("GET", "/api/x") ];')

    def test_public_head_is_a_get_alias_with_the_same_dependencies(self):
        self.put_policy()
        self.review["go_dependencies"] = {"GET /api/v2/runtime": ["Go backend availability"]}
        self.put("app/Sources/Head.swift", 'func head() { try await client.send("api/v2/runtime", method: "HEAD") }')
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        routes = {route["id"]: route for route in inventory["routes"]}
        alias = routes["HEAD /api/v2/runtime"]
        self.assertEqual(alias["owner"], "rust")
        self.assertEqual(alias["alias_of"], "GET /api/v2/runtime")
        self.assertEqual(alias["go_dependencies"], routes[alias["alias_of"]]["go_dependencies"])
        self.assertEqual(alias["migration_status"], "get_alias_no_additional_implementation")
        self.assertEqual(alias["sources"][0]["kind"], "public_get_head_alias")
        self.assertEqual(alias["sources"][0]["alias_of"], alias["alias_of"])
        self.assertEqual(alias["consumers"][0]["file"], "app/Sources/Head.swift")
        self.assertEqual(len(inventory["public_method_policy"]), 1)
        self.assertNotIn("HEAD /api/status", routes)

    def test_reviewed_internal_go_rpc_is_a_public_rust_route_dependency(self):
        self.put_policy()
        self.review["go_dependencies"] = {
            "GET /api/v2/runtime": ["Rust calls Go POST /api/internal/reader/reconcile"]
        }
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        routes = {route["id"]: route for route in inventory["routes"]}
        self.assertEqual(
            routes["GET /api/v2/runtime"]["go_dependencies"],
            ["Rust calls Go POST /api/internal/reader/reconcile"],
        )
        self.assertEqual(
            routes["HEAD /api/v2/runtime"]["go_dependencies"],
            routes["GET /api/v2/runtime"]["go_dependencies"],
        )
        self.assertTrue(c.rust_only_errors(inventory))

    def test_head_cannot_erase_get_go_dependencies(self):
        self.put_policy()
        self.review["go_dependencies"] = {
            "GET /api/v2/runtime": ["Go hardware"], "HEAD /api/v2/runtime": []}
        inventory, errors = self.build()
        self.assertTrue(any("HEAD dependency annotation differs from GET" in error for error in errors))
        alias = next(row for row in inventory["routes"] if row["method"] == "HEAD")
        self.assertEqual(alias["go_dependencies"], ["Go hardware"])

    def test_policy_requires_union_of_go_and_rust_methods_on_same_path(self):
        self.add_go("POST", "/api/v2/runtime")
        for methods, valid in [("GET, HEAD, POST", True), ("GET, HEAD", False), ("DELETE, GET, HEAD, POST", False)]:
            with self.subTest(methods=methods):
                self.put_policy([("/api/v2/runtime", methods)])
                errors = self.build()[1]
                self.assertEqual(errors == [], valid)
                if not valid:
                    self.assertTrue(any("methods differ from real handlers" in error for error in errors))

    def test_head_of_go_get_on_a_mixed_path_keeps_go_owner(self):
        self.add_native("POST", "/api/status")
        self.put_policy([("/api/v2/runtime", "GET, HEAD"), ("/api/status", "GET, HEAD, POST")])
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        alias = next(row for row in inventory["routes"] if row["id"] == "HEAD /api/status")
        self.assertEqual(alias["owner"], "go")
        self.assertEqual(alias["go_dependencies"], ["Go implementation and compatibility forwarding"])

    def test_policy_is_not_handler_evidence(self):
        self.put_policy([("/api/v2/runtime", "GET, HEAD"), ("/api/missing", "POST")])
        inventory, errors = self.build()
        self.assertTrue(any("policy has no Rust handler: /api/missing" in error for error in errors))
        self.assertNotIn("POST /api/missing", {row["id"] for row in inventory["routes"]})

    def test_policy_does_not_replace_ownership_declaration(self):
        self.put(c.RUST + "ownership.rs", "fn route_owner() { RouteOwner::GoCompatibility }")
        self.put_policy()
        self.assertTrue(any("handler missing ownership declaration" in error for error in self.build()[1]))

    def test_new_owned_native_handler_requires_policy_entry(self):
        self.put_policy()
        self.add_native("POST", "/api/new-native")
        self.assertIn("public Rust handler missing HTTP method policy: /api/new-native", self.build()[1])

    def test_empty_policy_cannot_omit_all_public_native_handlers(self):
        self.put_policy([])
        self.assertIn("public Rust handler missing HTTP method policy: /api/v2/runtime", self.build()[1])

    def test_policy_rejects_go_only_and_internal_paths(self):
        self.add_native("POST", "/api/internal/control")
        self.put_policy()
        self.assertEqual(self.build()[1], [])  # Internal methods need no public policy.
        for path, methods, expected in [
            ("/api/status", "GET, HEAD", "policy has no Rust handler"),
            ("/api/internal/control", "POST", "internal path must not enter")]:
            with self.subTest(path=path):
                self.put_policy([("/api/v2/runtime", "GET, HEAD"), (path, methods)])
                self.assertTrue(any(expected in error for error in self.build()[1]))

    def test_policy_rejects_missing_head_invalid_duplicate_and_unsorted_methods(self):
        for methods, expected in [
            ("GET", "methods differ"), ("HEAD", "HEAD alias requires GET"),
            ("HEAD, POST", "HEAD alias requires GET"),
            ("GET, GET, HEAD", "duplicate public HTTP policy method"),
            ("HEAD, GET", "unique sorted methods"),
            ("get, head", "invalid public HTTP policy method list"),
            ("GET,HEAD", "invalid public HTTP policy method list"),
            ("GET, HEAD, BREW", "invalid public HTTP policy method list"),
            ("", "invalid public HTTP policy method list")]:
            with self.subTest(methods=methods):
                self.put_policy([("/api/v2/runtime", methods)])
                self.assertTrue(any(expected in error for error in self.build()[1]))

    def test_policy_rejects_duplicate_paths_even_renamed_templates(self):
        self.add_native("GET", "/api/items/{id}")
        for duplicate in ["/api/items/{id}", "/api/items/{renamed}"]:
            with self.subTest(duplicate=duplicate):
                self.put_policy([("/api/v2/runtime", "GET, HEAD"),
                                 ("/api/items/{id}", "GET, HEAD"), (duplicate, "GET, HEAD")])
                self.assertTrue(any("duplicate public HTTP policy path" in error for error in self.build()[1]))

    def test_template_policy_names_are_opaque_but_match_only_final_segments(self):
        self.add_native("GET", "/api/items/{id}")
        self.put_policy([("/api/v2/runtime", "GET, HEAD"), ("/api/items/{token}", "GET, HEAD")])
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        alias = next(row for row in inventory["routes"] if row["id"] == "HEAD /api/items/{id}")
        self.assertEqual(alias["sources"][0]["policy_path"], "/api/items/{token}")
        self.assertTrue(c.compatible(alias["path"], "/api/items/opaque"))
        self.assertFalse(c.compatible(alias["path"], "/api/items/"))
        self.assertFalse(c.compatible(alias["path"], "/api/items/opaque/extra"))

    def test_policy_rejects_shapes_runtime_does_not_match(self):
        for path in ["/api/items/", "/api//items", "/api/items/*", "/api/items?x=1",
                     "/api/items#fragment", "/api/items/{id}/extra", "/api/{kind}/{id}",
                     "/api/items/{id}tail", "/api/items/{bad-name}", "/api/items/{id", "/elsewhere", "/api/has space"]:
            with self.subTest(path=path):
                self.put_policy([("/api/v2/runtime", "GET, HEAD"), (path, "GET, HEAD")])
                self.assertTrue(any("invalid public HTTP policy path" in error for error in self.build()[1]))

    def test_explicit_head_handler_cannot_silently_become_get_alias(self):
        self.add_native("HEAD", "/api/v2/runtime")
        self.put_policy()
        inventory, errors = self.build()
        self.assertIn("explicit HEAD handler requires public policy review: /api/v2/runtime", errors)
        head = next(row for row in inventory["routes"] if row["id"] == "HEAD /api/v2/runtime")
        self.assertNotIn("alias_of", head)

    def test_policy_parser_supports_comments_raw_literals_and_trailing_tuple_commas(self):
        text = '''// const ROUTES: unknown
const ROUTES: &[(&str, &str)] = &[(r#"/api/v2/runtime"#, "GET, HEAD",),];
#[cfg(test)] mod tests { const ROUTES: &[(&str, &str)] = &[("/api/test", "GET")]; }
'''
        rows = c.public_policy_rows(c.production(text, "rust"), "policy.rs")
        self.assertEqual([(row["path"], row["allow"]) for row in rows], [("/api/v2/runtime", "GET, HEAD")])
        self.assertEqual(c.exact_rust_routes(text, "policy.rs"), [])

    def test_policy_dynamic_or_ambiguous_declarations_require_extractor_review(self):
        for text in [
            "const ROUTES: &[(&str, &str)] = make_routes();",
            'const ROUTES: &[(&str, &str)] = &[(PATH, "GET")];',
            'const ROUTES: &[(&str, &str)] = &[("/api/x", METHODS)];',
            "const ROUTES: &[(&str, &str)] = &[]; const ROUTES: &[(&str, &str)] = &[];",
            'const ROUTES: &[(&str, &str)] = &[("/api/x", "GET") ("/api/y", "POST")];',
            "const ROUTES: &[(&str, &str)] = &[", "const OTHER: &[(&str, &str)] = &[];"
        ]:
            with self.subTest(text=text):
                with self.assertRaises(c.ContractError):
                    c.public_policy_rows(text, "policy.rs")

    def test_policy_requires_whole_file_review_and_guards_matcher_semantics(self):
        suffix = "fn allowed_methods(path: &str) { exact_match(path); }"
        self.put_policy(suffix=suffix)
        self.assertEqual(self.build()[1], [])
        scope = self.review["scopes"][0]
        text = (self.root / scope["file"]).read_text()
        body, _ = c.function_scope(text, "allowed_methods")
        scope.update(function="allowed_methods", sha256=c.digest(body))
        self.assertTrue(any("whole-production-file fingerprint" in error for error in self.build()[1]))
        self.put_policy(suffix=suffix)
        self.put(c.RUST + "public_http.rs", text.replace("exact_match", "prefix_match"))
        self.assertTrue(any("MANUAL REVIEW REQUIRED" in error for error in self.build()[1]))

    def test_legacy_method_error_subset_must_be_a_unique_public_policy_member(self):
        self.put_policy(legacy=["/api/v2/runtime"])
        self.assertEqual(self.build()[1], [])
        for legacy, expected in [
            (["/api/v2/runtime", "/api/v2/runtime"], "duplicate legacy method path"),
            (["/api/missing"], "not a public ROUTES member"),
            (["/api/internal/hidden"], "not a public ROUTES member"),
            (["/api/status"], "not a public ROUTES member")]:
            with self.subTest(legacy=legacy):
                self.put_policy(legacy=legacy)
                self.assertTrue(any(expected in error for error in self.build()[1]))

    def test_legacy_method_subset_cannot_use_dynamic_or_unparsed_syntax(self):
        for text in [
            "const LEGACY_METHOD_PATHS: &[&str] = computed();",
            "const LEGACY_METHOD_PATHS: &[&str] = &[PATH];",
            "const LEGACY_METHOD_PATHS: &[&str] = &[]; const LEGACY_METHOD_PATHS: &[&str] = &[];",
            'const LEGACY_METHOD_PATHS: &[&str] = &["/api/x" "/api/y"];',
            'const LEGACY_METHOD_PATHS: &[&str] = &["/api/x"'
        ]:
            with self.subTest(text=text):
                with self.assertRaises(c.ContractError):
                    c.public_legacy_paths(text, "policy.rs")

    def test_external_cfg_test_sources_are_excluded_by_module_edges(self):
        self.put_policy(suffix="#[cfg(test)] mod fixtures;\n")
        file = c.RUST + "public_http/fixtures.rs"
        self.put(file, 'fn sample() { let x = "/api/fixture-only"; }\nmod nested;')
        nested = c.RUST + "public_http/fixtures/nested.rs"
        self.put(nested, 'const SAMPLE: &str = "/api/nested-fixture";')
        unrelated = c.RUST + "public_http/real.rs"
        self.put(unrelated, 'const URL: &str = "/api/unreferenced-still-visible";')
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        self.assertEqual(inventory["excluded_rust_test_sources"], sorted([file, nested]))
        values = [ref["value"] for ref in inventory["source_api_references"]]
        self.assertNotIn("/api/fixture-only", values)
        self.assertNotIn("/api/nested-fixture", values)
        self.assertIn("/api/unreferenced-still-visible", values)

    def test_literal_path_and_include_follow_explicit_test_cfg(self):
        self.put_policy(suffix='#[cfg(test)] #[path = "custom/checks.rs"] mod checks;\n#[cfg(test)] mod inline { include!("custom/included.rs"); }')
        for name in ["checks", "included"]:
            self.put(c.RUST + f"custom/{name}.rs", f'const URL: &str = "/api/{name}-fixture";')
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        self.assertEqual(len(inventory["excluded_rust_test_sources"]), 2)
        self.assertFalse(any(ref["value"].endswith("-fixture") for ref in inventory["source_api_references"]))

    def test_all_test_and_literal_feature_is_provably_test_only(self):
        for condition in ['all(test, feature = "native-codec-fault-tests")',
                          'all(feature = "native-codec-fault-tests", test)']:
            with self.subTest(condition=condition):
                self.put_policy(suffix=f'#[cfg({condition})] mod fixtures;')
                file = c.RUST + "public_http/fixtures.rs"
                self.put(file, 'const URL: &str = "/api/fault-fixture-only";')
                inventory, errors = self.build()
                self.assertEqual(errors, [])
                self.assertEqual(inventory["excluded_rust_test_sources"], [file])
                self.assertFalse(any(ref["value"] == "/api/fault-fixture-only"
                                     for ref in inventory["source_api_references"]))

    def test_production_reference_wins_over_feature_gated_test_edge(self):
        self.put_policy(suffix='#[cfg(all(test, feature = "fixtures"))] mod shared;')
        file = c.RUST + "public_http/shared.rs"
        self.put(file, 'const URL: &str = "/api/live-shared-with-fault-tests";')
        self.put(c.RUST + "live.rs", '#[path = "public_http/shared.rs"] mod live_shared;')
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        self.assertNotIn(file, inventory["excluded_rust_test_sources"])
        self.assertTrue(any(ref["value"] == "/api/live-shared-with-fault-tests"
                            for ref in inventory["source_api_references"]))

    def test_name_tests_does_not_hide_a_production_module(self):
        self.put_policy(suffix="mod tests;")
        file = c.RUST + "public_http/tests.rs"
        self.put(file, 'const URL: &str = "/api/production-despite-name";')
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        self.assertEqual(inventory["excluded_rust_test_sources"], [])
        self.assertTrue(any(ref["value"] == "/api/production-despite-name" for ref in inventory["source_api_references"]))

    def test_production_reference_wins_over_test_module_reference(self):
        for production_reference in ['#[path = "public_http/shared.rs"] mod live;', 'include!("public_http/shared.rs");']:
            with self.subTest(reference=production_reference):
                self.put_policy(suffix="#[cfg(test)] mod shared;")
                file = c.RUST + "public_http/shared.rs"
                self.put(file, 'const URL: &str = "/api/shared-production";')
                self.put(c.RUST + "live.rs", production_reference)
                inventory, errors = self.build()
                self.assertEqual(errors, [])
                self.assertNotIn(file, inventory["excluded_rust_test_sources"])
                self.assertTrue(any(ref["value"] == "/api/shared-production" for ref in inventory["source_api_references"]))

    def test_reviewed_external_production_conditions_are_not_test_only(self):
        conditions = ['feature = "native-codec-fault-tests"',
                      'target_os = "macos"', 'target_os = "windows"',
                      'any(test, feature = "device-send-authority-codec")',
                      'any(feature = "device-send-authority-codec", test)']
        for condition in conditions:
            with self.subTest(condition=condition):
                self.put_policy(suffix=f'#[cfg({condition})] mod fixtures;')
                file = c.RUST + "public_http/fixtures.rs"
                self.put(file, 'const A: &str = "/api/conditional-live";'
                               'const B: &str = "/api/conditional-live";')
                inventory, errors = self.build()
                self.assertEqual(errors, [])
                self.assertNotIn(file, inventory["excluded_rust_test_sources"])
                refs = [ref for ref in inventory["source_api_references"]
                        if ref["value"] == "/api/conditional-live"]
                self.assertEqual(len(refs), 2, "duplicate source references must stay visible")
                self.assertTrue(all(ref["source"]["file"] == file for ref in refs))

    def test_feature_and_any_test_feature_edges_keep_nested_production_modules(self):
        for condition in ['feature = "codec"', 'any(test, feature = "codec")']:
            with self.subTest(condition=condition):
                self.put_policy(suffix=f'#[cfg({condition})] mod codec;')
                parent = c.RUST + "public_http/codec.rs"
                live = c.RUST + "public_http/codec/live.rs"
                checks = c.RUST + "public_http/codec/checks.rs"
                self.put(parent, 'mod live; #[cfg(test)] mod checks;')
                self.put(live, 'const LIVE: &str = "/api/nested-conditional-live";')
                self.put(checks, 'const TEST: &str = "/api/nested-only-test";')
                inventory, errors = self.build()
                self.assertEqual(errors, [])
                self.assertEqual(inventory["excluded_rust_test_sources"], [checks])
                values = [ref["value"] for ref in inventory["source_api_references"]]
                self.assertIn("/api/nested-conditional-live", values)
                self.assertNotIn("/api/nested-only-test", values)

    def test_production_conditional_literal_path_wins_over_explicit_test_edge(self):
        for condition in ['feature = "codec"', 'target_os = "macos"',
                          'any(test, feature = "codec")']:
            with self.subTest(condition=condition):
                self.put_policy(suffix='#[cfg(test)] mod shared;')
                shared = c.RUST + "public_http/shared.rs"
                self.put(shared, 'mod nested; const URL: &str = "/api/shared-gated-live";')
                nested = c.RUST + "public_http/shared/nested.rs"
                self.put(nested, 'const URL: &str = "/api/shared-nested-live";')
                self.put(c.RUST + "live.rs",
                         f'#[cfg({condition})] #[path = "public_http/shared.rs"] mod shared;')
                inventory, errors = self.build()
                self.assertEqual(errors, [])
                self.assertNotIn(shared, inventory["excluded_rust_test_sources"])
                self.assertNotIn(nested, inventory["excluded_rust_test_sources"])
                values = [ref["value"] for ref in inventory["source_api_references"]]
                self.assertIn("/api/shared-gated-live", values)
                self.assertIn("/api/shared-nested-live", values)

    def test_any_test_feature_inline_route_does_not_hide_duplicate_dispatch(self):
        main = self.root / (c.RUST + "main.rs")
        main.write_text(main.read_text() + '\n#[cfg(any(test, feature = "codec"))] '
                        'mod codec { fn handler() { match x { '
                        '(&Method::GET, "/api/v2/runtime") => reply(), _ => recover() } } }')
        inventory, errors = self.build()
        self.assertEqual(errors, [])
        routes = c.exact_rust_routes(c.production(main.read_text(), "rust"), c.RUST + "main.rs")
        matches = [route for route in routes if route["method"] == "GET"
                   and route["path"] == "/api/v2/runtime"]
        self.assertEqual(len(matches), 2)
        self.assertTrue(any(route["id"] == "GET /api/v2/runtime" for route in inventory["routes"]))

    def test_escaped_rust_path_with_real_decoy_files_never_excludes_production(self):
        # Actual files reproduce the reviewed failure: the ordinary Rust hex/
        # unicode/continuation path denotes hidden.rs, not the backslash decoy.
        for spelling in [r"\x68idden.rs", r"\u{68}idden.rs", "h\\\n    idden.rs"]:
            for form in ["path", "include"]:
                with self.subTest(spelling=spelling, form=form):
                    with tempfile.TemporaryDirectory(dir=BUILD) as directory:
                        root = Path(directory)
                        directive = (f'#[cfg(feature="prod")] #[path="{spelling}"] mod live;'
                                     if form == "path" else
                                     f'fn live() {{ include!("{spelling}"); }}')
                        texts = {
                            "main.rs": '#[cfg(test)] mod hidden; ' + directive,
                            "hidden.rs": 'const LIVE: &str = "/api/real-production";',
                            spelling: 'const DECOY: &str = "/api/decoy";',
                        }
                        for name, text in texts.items():
                            (root / name).write_text(text)
                        actual = {name: (root / name).read_text() for name in texts}
                        with self.assertRaises(c.ContractError):
                            c.rust_test_sources(root, actual)

    def test_strict_rust_source_path_literals_keep_plain_and_raw_production_edges(self):
        for literal in ['"hidden.rs"', 'r"hidden.rs"', 'r##"hidden.rs"##']:
            for form in ["path", "include"]:
                with self.subTest(literal=literal, form=form):
                    with tempfile.TemporaryDirectory(dir=BUILD) as directory:
                        root = Path(directory)
                        directive = (f'#[cfg(feature="prod")] #[path={literal}] mod live;'
                                     if form == "path" else
                                     f'fn live() {{ include!({literal}); }}')
                        texts = {
                            "main.rs": '#[cfg(test)] mod hidden; ' + directive,
                            "hidden.rs": 'const LIVE: &str = "/api/real-production";',
                        }
                        for name, text in texts.items():
                            (root / name).write_text(text)
                        self.assertNotIn("hidden.rs", c.rust_test_sources(root, texts))

    def test_raw_backslash_source_path_keeps_its_literal_filename_semantics(self):
        name = r"\x68idden.rs"
        for literal in [f'r"{name}"', f'r##"{name}"##']:
            for form in ["path", "include"]:
                with self.subTest(literal=literal, form=form):
                    with tempfile.TemporaryDirectory(dir=BUILD) as directory:
                        root = Path(directory)
                        directive = (f'#[cfg(feature="prod")] #[path={literal}] mod live;'
                                     if form == "path" else
                                     f'fn live() {{ include!({literal}); }}')
                        texts = {
                            "main.rs": '#[cfg(test)] mod hidden; ' + directive,
                            "hidden.rs": 'const TEST: &str = "/api/test-only";',
                            name: 'const LIVE: &str = "/api/raw-backslash-production";',
                        }
                        for filename, text in texts.items():
                            (root / filename).write_text(text)
                        self.assertEqual(c.rust_path_literal(literal), name)
                        excluded = c.rust_test_sources(root, texts)
                        self.assertIn("hidden.rs", excluded)
                        self.assertNotIn(name, excluded)

    def test_rust_path_decoder_rejects_byte_literals_escapes_and_dynamic_paths(self):
        for literal in [r'"\x68idden.rs"', r'"\u{68}idden.rs"', r'"hidden\\name.rs"',
                        r'"hidden\nname.rs"', r'"hidden\q.rs"', '"h\\\n    idden.rs"',
                        'b"hidden.rs"', 'br"hidden.rs"', 'br##"hidden.rs"##',
                        'concat!("hidden", ".rs")', '""']:
            for form in ["path", "include"]:
                with self.subTest(literal=literal, form=form):
                    directive = (f'#[path={literal}] mod hidden;' if form == "path" else
                                 f'fn live() {{ include!({literal}); }}')
                    with self.assertRaises(c.ContractError):
                        c.rust_test_sources(self.root, {"main.rs": directive})

    def test_unreviewed_include_delimiters_fail_closed_with_real_shared_test_edge(self):
        for left, right in [("{", "}"), ("[", "]")]:
            with self.subTest(left=left):
                with tempfile.TemporaryDirectory(dir=BUILD) as directory:
                    root = Path(directory)
                    texts = {
                        "main.rs": f'#[cfg(test)] mod shared; include!{left}"shared.rs"{right}',
                        "shared.rs": 'const LIVE: &str = "/api/live-include";',
                    }
                    for name, text in texts.items():
                        (root / name).write_text(text)
                    actual = {name: (root / name).read_text() for name in texts}
                    with self.assertRaisesRegex(c.ContractError, "delimiter needs scanner review"):
                        c.rust_test_sources(root, actual)

    def test_reviewed_production_predicates_do_not_relax_path_resolution(self):
        for suffix in [
            '#[cfg(feature = "codec")] #[path = concat!("x", ".rs")] mod codec;',
            '#[cfg(any(test, feature = "codec"))] #[path = "../escape.rs"] mod codec;',
            '#[cfg(target_os = "macos")] mod absent;',
            '#[cfg(feature = "codec")] mod inline { mod nested; }',
            '#[cfg(feature = "codec")] #[cfg_attr(test, path = "other.rs")] mod codec;',
        ]:
            with self.subTest(suffix=suffix):
                with self.assertRaises(c.ContractError):
                    self.put_policy(suffix=suffix)
                    self.build()

    def test_external_module_conditions_dynamic_paths_and_missing_files_fail_closed(self):
        for suffix in [
            '#[cfg(any(test, target_os = "macos"))] mod fixtures;',
            '#[cfg(feature = feature_name!())] mod fixtures;',
            '#[cfg(target_os = "")] mod fixtures;',
            '#[cfg(target_arch = "aarch64")] mod fixtures;',
            '#[cfg(not(test))] mod fixtures;',
            '#[cfg(any(test, feature = "fixtures", unix))] mod fixtures;',
            '#[cfg_attr(test, path = "else.rs")] mod inline {}',
            '#[cfg(all(not(test), feature = "fixtures"))] mod fixtures;',
            '#[cfg(all(test, any(feature = "a", feature = "b")))] mod fixtures;',
            '#[cfg(all(test, feature = ""))] mod fixtures;',
            '#[cfg_attr(test, path = "else.rs")] mod fixtures;',
            '#[cfg(test)] #[path = concat!("x", ".rs")] mod fixtures;',
            '#[cfg(test)] #[path = "../escape.rs"] mod fixtures;',
            '#[cfg(test)] mod missing;',
            '#[cfg(test)] mod inline { mod fixtures; }',
            '#[cfg(test)] mod inline { include!(concat!("x", ".rs")); }'
        ]:
            with self.subTest(suffix=suffix):
                with self.assertRaises(c.ContractError):
                    self.put_policy(suffix=suffix)
                    self.build()

    def test_test_only_dynamic_route_cannot_be_registered_as_production(self):
        self.put_policy(suffix="#[cfg(test)] mod fixtures;")
        file = c.RUST + "public_http/fixtures.rs"
        self.put(file, 'fn dispatch() { let URL = "/api/fixture"; }')
        self.review["dynamic_routes"] = [{"file": file, "function": "dispatch", "method": "POST", "path": "/api/fixture", "note": "invalid fixture evidence"}]
        inventory, errors = self.build()
        self.assertTrue(any("dynamic production route points to a test-only source" in error for error in errors))
        self.assertNotIn("POST /api/fixture", {route["id"] for route in inventory["routes"]})


if __name__ == "__main__":
    unittest.main(verbosity=2)
