#!/usr/bin/env python3
"""Offline synthetic fixtures for the release verifier; never contact GitHub."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("publisher", Path(__file__).with_name("publish-release.py"))
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)
COMMIT, TREE, RUN = "a" * 40, "b" * 40, 12345
VERSION, BUILD = "3.52", "352"

def encode(value):
    return (json.dumps(value, indent=2) + "\n").encode()

def fixture():
    content = b"synthetic offline fixture\n"
    name = "Switch-3.52-candidate-" + COMMIT + "-macOS27-arm64.app.tar.gz"
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        entry = tarfile.TarInfo("Switch.app/Contents/MacOS/Switch")
        entry.mode, entry.size = 0o755, len(content)
        archive.addfile(entry, io.BytesIO(content))
    files = {name: buffer.getvalue()}
    for gate in publisher.GATES:
        files["test-reports/" + gate + ".json"] = encode({
            "gate": gate, "status": "passed", "exit_code": 0, "commit": COMMIT, "run_id": str(RUN), "run_attempt": "1"})
    manifest = {"schema_version": 1, "kind": "ci-app-candidate", "version": VERSION, "build": BUILD,
        "commit": COMMIT, "tree": TREE, "source_digest": "c" * 64, "run": {"id": str(RUN), "attempt": "1"},
        "identity_validation": {"app_version": VERSION, "build_number": BUILD, "backend_version": VERSION + ".0",
                                "source_digest": "c" * 64, "scope": "plist-and-embedded-identity"},
        "acceptance": {"product_acceptance_complete": False, "device_acceptance": "UNVERIFIED",
                       "macos27_runtime": "UNVERIFIED", "signature": "ad-hoc", "notarized": False, "original_v3_51_release": False},
        "artifact": {"name": name, "size": len(files[name]), "sha256": publisher.digest(files[name])},
        "app_inventory": [{"path": "Switch.app/Contents/MacOS/Switch", "kind": "file", "mode": 0o755,
                           "size": len(content), "sha256": publisher.digest(content)}],
        "reports_sha256": {key.split("/", 1)[1]: publisher.digest(value) for key, value in files.items() if key.startswith("test-reports/")}}
    manifest["app_tree_sha256"] = publisher.digest(json.dumps(manifest["app_inventory"], sort_keys=True, separators=(",", ":")).encode())
    seal(files, manifest)
    reports = {"upload-roundtrip.json": encode({"gate": "upload-roundtrip", "status": "passed", "exit_code": 0,
               "commit": COMMIT, "run_id": str(RUN), "run_attempt": "1"}), "upload-roundtrip.log": b"synthetic success\n"}
    return files, manifest, reports

def seal(files, manifest):
    files["candidate-manifest.json"] = encode(manifest)
    name = manifest["artifact"]["name"]
    files["SHA256SUMS"] = (publisher.digest(files[name]) + "  " + name + "\n" +
        publisher.digest(files["candidate-manifest.json"]) + "  candidate-manifest.json\n").encode()

def validate(files, reports, **overrides):
    values = dict(commit=COMMIT, tree=TREE, version=VERSION, build=BUILD, run_id=RUN, attempt=1)
    values.update(overrides)
    return publisher.validate_payload(files, reports, **values)

class ReleaseFixtures(unittest.TestCase):
    def test_valid_four_asset_whitelist_and_deterministic_retry(self):
        files, _, reports = fixture()
        first, _ = validate(files, reports)
        second, _ = validate(files, reports)
        self.assertEqual(first, second)
        self.assertEqual(set(first), {next(name for name in files if name.endswith(".app.tar.gz")),
                                     "candidate-manifest.json", "SHA256SUMS", "test-reports.tar.gz"})
        with tarfile.open(fileobj=io.BytesIO(first["test-reports.tar.gz"])) as archive:
            self.assertIn("test-reports/upload-roundtrip.json", archive.getnames())

    def test_exact_source_tree_version_build_run_attempt(self):
        for field, value in [("commit", "d" * 40), ("tree", "e" * 40), ("version", "3.53"),
                             ("build", "353"), ("run_id", 9), ("attempt", 2)]:
            with self.subTest(field=field):
                files, _, reports = fixture()
                with self.assertRaises(RuntimeError):
                    validate(files, reports, **{field: value})

    def test_app_tamper_is_rejected(self):
        files, manifest, reports = fixture()
        files[manifest["artifact"]["name"]] += b"tampered"
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_archive_mode_is_checked_even_with_consistent_manifest_hashes(self):
        files, manifest, reports = fixture()
        manifest["app_inventory"][0]["mode"] = 0o644
        manifest["app_tree_sha256"] = publisher.digest(json.dumps(manifest["app_inventory"], sort_keys=True, separators=(",", ":")).encode())
        seal(files, manifest)
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_failed_gate_cannot_be_hidden_by_rehashing(self):
        files, manifest, reports = fixture()
        path = "test-reports/rust.json"
        report = json.loads(files[path])
        report.update(status="failed", exit_code=1)
        files[path] = encode(report)
        manifest["reports_sha256"]["rust.json"] = publisher.digest(files[path])
        seal(files, manifest)
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_roundtrip_failure_or_wrong_source_rejected(self):
        for change in [{"status": "failed"}, {"commit": "f" * 40}, {"run_attempt": "2"}]:
            files, _, reports = fixture()
            report = json.loads(reports["upload-roundtrip.json"])
            report.update(change)
            reports["upload-roundtrip.json"] = encode(report)
            with self.assertRaises(RuntimeError):
                validate(files, reports)

    def test_missing_roundtrip_rejected(self):
        files, _, reports = fixture()
        del reports["upload-roundtrip.json"]
        with self.assertRaises(KeyError):
            validate(files, reports)

    def test_unexpected_candidate_file_rejected(self):
        files, _, reports = fixture()
        files["credentials.txt"] = b"synthetic unexpected data"
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_identity_or_acceptance_drift_rejected(self):
        for area, key, value in [("identity_validation", "backend_version", "3.51.0"),
                                 ("acceptance", "product_acceptance_complete", True)]:
            files, manifest, reports = fixture()
            manifest[area][key] = value
            seal(files, manifest)
            with self.assertRaises(RuntimeError):
                validate(files, reports)

    def test_checksum_lines_must_match_exactly(self):
        files, _, reports = fixture()
        files["SHA256SUMS"] += b"extra checksum line\n"
        with self.assertRaises(RuntimeError):
            validate(files, reports)

    def test_same_asset_retry_succeeds_but_conflict_never_replaces(self):
        publisher.same_asset(b"same", b"same")
        for old in [None, b"different", b""]:
            with self.assertRaises(RuntimeError):
                publisher.same_asset(old, b"same")

    def test_zip_traversal_and_duplicates_rejected(self):
        for names in [["../escape"], ["/absolute"], ["same", "same"]]:
            output = io.BytesIO()
            with zipfile.ZipFile(output, "w") as archive:
                for name in names:
                    archive.writestr(name, b"synthetic")
            with self.assertRaises(RuntimeError):
                publisher.checked_zip(output.getvalue())


class FakeGitHub:
    """Only the GitHub boundary is mocked; main and payload validation run unchanged."""
    def __init__(self):
        import base64
        self.files, self.manifest, self.reports = fixture()
        self.release = None
        self.tag = None
        self.published = [{"id": 351, "tag_name": "v3.51", "draft": False}]
        self.builds = {"v3.51": "351"}
        self.remote = {}
        self.asset_bytes = {}
        self.next_id = 10
        self.writes = []
        self.upload_failure_after = None
        self.bad_readback = False
        self.extra_final_asset = False
        self.asset_lists = 0

    def install_draft(self):
        self.release = {"id": 352, "tag_name": "v3.52", "draft": True,
            "html_url": "https://github.com/Proletariuss/Switch/releases/tag/v3.52",
            "body": "<!-- switch-release:v1 source=" + COMMIT + " manifest=" +
                    publisher.digest(self.files["candidate-manifest.json"]) + " -->"}

    def api(self, method, suffix, value=None, **kwargs):
        import base64
        from urllib.parse import parse_qs, urlsplit
        if method != "GET":
            self.writes.append((method, suffix))
        if method == "GET" and suffix == "/actions/runs/" + str(RUN):
            return {"head_sha": COMMIT, "path": ".github/workflows/release.yml",
                    "event": "workflow_dispatch", "repository": {"full_name": publisher.REPO},
                    "status": "in_progress", "conclusion": None, "html_url": "https://example.invalid/run"}
        if method == "GET" and suffix == "/git/commits/" + COMMIT:
            return {"tree": {"sha": TREE}}
        if method == "GET" and suffix == "/git/ref/tags/v3.52":
            return copy.deepcopy(self.tag)
        if method == "GET" and suffix == "/releases/tags/v3.52":
            return copy.deepcopy(self.release)
        if method == "GET" and suffix.startswith("/contents/BUILD_NUMBER?ref="):
            tag = parse_qs(urlsplit(suffix).query)["ref"][0]
            raw = (self.builds[tag] + "\n").encode()
            return {"encoding": "base64", "size": len(raw), "content": base64.b64encode(raw).decode()}
        if method == "POST" and suffix == "/releases":
            self.release = dict(value, id=352, html_url="https://github.com/Proletariuss/Switch/releases/tag/v3.52")
            return copy.deepcopy(self.release)
        if method == "POST" and suffix.startswith("/releases/352/assets?name="):
            if self.upload_failure_after is not None and len(self.remote) >= self.upload_failure_after:
                self.upload_failure_after = None
                raise RuntimeError("synthetic interrupted upload")
            name = parse_qs(urlsplit(suffix).query)["name"][0]
            if name in self.remote:
                raise AssertionError("duplicate upload must not replace an existing asset")
            item = {"id": self.next_id, "name": name, "size": len(value), "state": "uploaded"}
            self.next_id += 1
            self.remote[name] = item
            self.asset_bytes[item["id"]] = value
            return copy.deepcopy(item)
        if method == "GET" and suffix.startswith("/releases/assets/"):
            raw = self.asset_bytes[int(suffix.rsplit("/", 1)[1])]
            return raw + b"corrupted" if self.bad_readback else raw
        if method == "POST" and suffix == "/git/refs":
            if self.tag is not None:
                raise AssertionError("retagging is forbidden")
            self.tag = {"object": {"type": "commit", "sha": value["sha"]}}
            return copy.deepcopy(self.tag)
        if method == "PATCH" and suffix == "/releases/352":
            self.release.update(value)
            return copy.deepcopy(self.release)
        raise AssertionError("unexpected mocked API call: " + method + " " + suffix)

    def pages(self, suffix, key=None):
        if suffix == "/releases":
            return copy.deepcopy(self.published + ([self.release] if self.release else []))
        if suffix == "/releases/352/assets":
            self.asset_lists += 1
            rows = copy.deepcopy(list(self.remote.values()))
            if self.extra_final_asset and len(rows) == 4:
                rows.append({"id": 999, "name": "unexpected.txt", "size": 1, "state": "uploaded"})
            return rows
        if suffix == "/actions/runs/" + str(RUN) + "/artifacts":
            return []
        if suffix.endswith("/attempts/2/jobs"):
            return []  # Publish-only rerun; the successful candidate is from attempt 1.
        if suffix.endswith("/attempts/1/jobs"):
            return [{"name": "candidate / candidate", "conclusion": "success", "steps": [{
                "name": "Verify uploaded bytes and executable permissions", "conclusion": "success"}]}]
        raise AssertionError("unexpected mocked pages: " + suffix)

    def artifact(self, artifacts, name, run_id, commit):
        if run_id != RUN or commit != COMMIT:
            raise AssertionError("wrong source/run requested")
        if name == "Switch-candidate-" + COMMIT + "-attempt-1":
            return copy.deepcopy(self.files)
        if name == "software-report-1-candidate-and-upload":
            return copy.deepcopy(self.reports)
        raise AssertionError("wrong original artifact selected: " + name)

class PublicationStateFixtures(unittest.TestCase):
    def invoke(self, api, attempt=1):
        from unittest import mock
        environment = {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": publisher.REPO,
            "GITHUB_EVENT_NAME": "workflow_dispatch", "EXPECTED_COMMIT": COMMIT, "EXPECTED_TREE": TREE,
            "EXPECTED_VERSION": VERSION, "EXPECTED_BUILD": BUILD, "GITHUB_SHA": COMMIT,
            "GITHUB_RUN_ID": str(RUN), "GITHUB_RUN_ATTEMPT": str(attempt), "GITHUB_STEP_SUMMARY": "synthetic-summary"}
        with mock.patch.dict(publisher.os.environ, environment), mock.patch.object(publisher, "GitHub", return_value=api), \
                mock.patch("builtins.open", mock.mock_open()), mock.patch("sys.stdout", new_callable=io.StringIO):
            publisher.main()

    def test_fresh_publication_and_published_idempotent_readonly_retry(self):
        api = FakeGitHub()
        self.invoke(api)
        self.assertFalse(api.release["draft"])
        self.assertEqual(api.tag["object"]["sha"], COMMIT)
        self.assertEqual(len(api.remote), 4)
        api.published.append({"id": 353, "tag_name": "v3.53", "draft": False})
        api.builds["v3.53"] = "353"
        api.writes.clear()
        self.invoke(api, attempt=2)
        self.assertEqual(api.writes, [])  # Already public: verify bytes, never mutate or reset latest.

    def test_old_draft_after_newer_publication_is_rejected_without_writes(self):
        api = FakeGitHub()
        api.install_draft()
        api.published.append({"id": 353, "tag_name": "v3.53", "draft": False})
        api.builds["v3.53"] = "353"
        with self.assertRaisesRegex(RuntimeError, "version must increase"):
            self.invoke(api, attempt=2)
        self.assertTrue(api.release["draft"])
        self.assertEqual(api.writes, [])

    def test_new_marketing_version_cannot_reuse_published_build(self):
        api = FakeGitHub()
        api.builds["v3.51"] = BUILD
        with self.assertRaisesRegex(RuntimeError, "build number must increase"):
            self.invoke(api)
        self.assertEqual(api.writes, [])

    def test_partial_upload_retry_reuses_identical_assets_and_original_attempt(self):
        api = FakeGitHub()
        api.upload_failure_after = 2
        with self.assertRaisesRegex(RuntimeError, "interrupted upload"):
            self.invoke(api)
        self.assertTrue(api.release["draft"])
        self.assertIsNone(api.tag)
        prior_ids = {name: item["id"] for name, item in api.remote.items()}
        self.invoke(api, attempt=2)
        self.assertFalse(api.release["draft"])
        self.assertEqual(len(api.remote), 4)
        self.assertTrue(all(api.remote[name]["id"] == identity for name, identity in prior_ids.items()))

    def test_tag_or_marker_conflict_never_mutates_release(self):
        for kind in ("tag", "marker"):
            with self.subTest(kind=kind):
                api = FakeGitHub()
                if kind == "tag":
                    api.tag = {"object": {"type": "commit", "sha": "e" * 40}}
                else:
                    api.install_draft()
                    api.release["body"] = "unrelated release"
                with self.assertRaisesRegex(RuntimeError, "conflict"):
                    self.invoke(api)
                self.assertEqual(api.writes, [])

    def test_failed_asset_readback_keeps_draft_and_creates_no_tag(self):
        api = FakeGitHub()
        api.bad_readback = True
        with self.assertRaisesRegex(RuntimeError, "asset differs"):
            self.invoke(api)
        self.assertTrue(api.release["draft"])
        self.assertIsNone(api.tag)
        self.assertFalse(any(method == "PATCH" for method, _ in api.writes))

    def test_final_asset_relist_rejects_unexpected_added_asset(self):
        api = FakeGitHub()
        api.extra_final_asset = True
        with self.assertRaisesRegex(RuntimeError, "final release asset"):
            self.invoke(api)
        self.assertTrue(api.release["draft"])
        self.assertIsNone(api.tag)

    def test_newer_release_appearing_during_upload_blocks_publication(self):
        api = FakeGitHub()
        original_pages = api.pages
        def pages(suffix, key=None):
            if suffix == "/releases" and len(api.remote) == 4:
                api.published = [{"id": 353, "tag_name": "v3.53", "draft": False}]
                api.builds["v3.53"] = "353"
            return original_pages(suffix, key)
        api.pages = pages
        with self.assertRaisesRegex(RuntimeError, "version must increase"):
            self.invoke(api)
        self.assertTrue(api.release["draft"])
        self.assertFalse(any(method == "PATCH" for method, _ in api.writes))

    def test_backend_semver_normalizes_two_digit_marketing_minor(self):
        for marketing, backend in [("3.52", "3.52.0"), ("4.00", "4.0.0"), ("4.09", "4.9.0")]:
            self.assertEqual(publisher.backend_version(marketing), backend)


class RequestTransportFixtures(unittest.TestCase):
    """Exercise real request construction with only HTTP responses mocked."""
    def client(self):
        from unittest import mock
        with mock.patch.dict(publisher.os.environ, {"GH_TOKEN": "synthetic-token"}):
            return publisher.GitHub()

    def response(self, raw, headers=None):
        value = io.BytesIO(raw)
        value.headers = headers or {}
        return value

    def error(self, status, location=None):
        headers = {} if location is None else {"Location": location}
        return publisher.urllib.error.HTTPError("https://api.github.com/synthetic", status, "synthetic", headers, None)

    def test_artifact_zip_200_uses_github_json_accept_and_returns_raw_bytes(self):
        from unittest import mock
        client = self.client()
        raw = b"PK synthetic ZIP bytes"
        with mock.patch.object(client.opener, "open", return_value=self.response(raw)) as opened:
            result = client.api("GET", "/actions/artifacts/123/zip", binary=True)
        request = opened.call_args.args[0]
        self.assertEqual(request.get_header("Accept"), "application/vnd.github+json")
        self.assertEqual(request.get_header("Authorization"), "Bearer synthetic-token")
        self.assertIsNone(request.data)
        self.assertEqual(result, raw)

    def test_release_asset_200_uses_octet_stream_accept(self):
        from unittest import mock
        client = self.client()
        with mock.patch.object(client.opener, "open", return_value=self.response(b"asset bytes")) as opened:
            result = client.api("GET", "/releases/assets/123", binary=True)
        self.assertEqual(opened.call_args.args[0].get_header("Accept"), "application/octet-stream")
        self.assertEqual(result, b"asset bytes")

    def test_artifact_and_release_302_follow_without_token(self):
        from unittest import mock
        for suffix, accept in [("/actions/artifacts/123/zip", "application/vnd.github+json"),
                               ("/releases/assets/123", "application/octet-stream")]:
            with self.subTest(suffix=suffix):
                client = self.client()
                location = "https://storage.example.invalid/signed?synthetic=1"
                with mock.patch.object(client.opener, "open", side_effect=self.error(302, location)) as first, \
                        mock.patch.object(publisher.urllib.request, "urlopen", return_value=self.response(b"download")) as second:
                    self.assertEqual(client.api("GET", suffix, binary=True), b"download")
                self.assertEqual(first.call_args.args[0].get_header("Accept"), accept)
                self.assertEqual(first.call_args.args[0].get_header("Authorization"), "Bearer synthetic-token")
                # The second request is a bare signed URL, not the authenticated Request.
                self.assertEqual(second.call_args.args, (location,))
                self.assertEqual(second.call_args.kwargs, {"timeout": 120})
                self.assertNotIn("synthetic-token", second.call_args.args[0])

    def test_upload_keeps_binary_body_type_and_json_response(self):
        from unittest import mock
        client = self.client()
        payload = b"exact binary asset"
        with mock.patch.object(client.opener, "open", return_value=self.response(b'{"id":123,"state":"uploaded"}')) as opened:
            result = client.api("POST", "/releases/7/assets?name=fixture.tar.gz", payload, binary=True, upload=True)
        request = opened.call_args.args[0]
        self.assertTrue(request.full_url.startswith("https://uploads.github.com/repos/" + publisher.REPO + "/"))
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(request.get_header("Accept"), "application/vnd.github+json")
        self.assertEqual(request.get_header("Content-type"), "application/octet-stream")
        self.assertEqual(request.data, payload)
        self.assertEqual(result, {"id": 123, "state": "uploaded"})

    def test_non_https_redirect_is_rejected_before_second_request(self):
        from unittest import mock
        for location in ["http://storage.example.invalid/asset", "file:///synthetic", "//storage.example.invalid/asset"]:
            client = self.client()
            with self.subTest(location=location), \
                    mock.patch.object(client.opener, "open", side_effect=self.error(302, location)), \
                    mock.patch.object(publisher.urllib.request, "urlopen") as second:
                with self.assertRaisesRegex(RuntimeError, "unsafe download redirect"):
                    client.api("GET", "/actions/artifacts/123/zip", binary=True)
                second.assert_not_called()

    def test_http_errors_are_not_retried_or_reinterpreted_as_bytes(self):
        from unittest import mock
        for status in [403, 410, 415, 500, 307]:
            client = self.client()
            with self.subTest(status=status), \
                    mock.patch.object(client.opener, "open", side_effect=self.error(status)), \
                    mock.patch.object(publisher.urllib.request, "urlopen") as second:
                with self.assertRaises(publisher.urllib.error.HTTPError):
                    client.api("GET", "/actions/artifacts/123/zip", binary=True)
                second.assert_not_called()

    def test_missing_get_is_optional_but_missing_post_fails(self):
        from unittest import mock
        client = self.client()
        with mock.patch.object(client.opener, "open", side_effect=self.error(404)):
            self.assertIsNone(client.api("GET", "/releases/tags/v3.52"))
            with self.assertRaises(publisher.urllib.error.HTTPError):
                client.api("POST", "/releases", {"draft": True})

    def test_write_redirect_never_forwards_token_or_body(self):
        from unittest import mock
        client = self.client()
        with mock.patch.object(client.opener, "open", side_effect=self.error(302, "https://storage.example.invalid")), \
                mock.patch.object(publisher.urllib.request, "urlopen") as second:
            with self.assertRaises(publisher.urllib.error.HTTPError):
                client.api("POST", "/releases/7/assets?name=x", b"asset", binary=True, upload=True)
            second.assert_not_called()

    def test_declared_oversize_binary_response_fails_before_read(self):
        from unittest import mock
        client = self.client()
        response = self.response(b"small", {"Content-Length": str(publisher.LIMIT + 1)})
        with mock.patch.object(client.opener, "open", return_value=response):
            with self.assertRaisesRegex(RuntimeError, "response size exceeds limit"):
                client.api("GET", "/actions/artifacts/123/zip", binary=True)

class DerivativeFixtures(unittest.TestCase):
    """Published retries admit only the exact verified packaging artifact."""
    def fixture(self):
        api = FakeGitHub()
        api.install_draft()
        api.release["draft"] = False
        api.tag = {"object": {"type": "commit", "sha": COMMIT}}
        assets, manifest = validate(api.files, api.reports)
        for name, raw in assets.items():
            item = {"id": api.next_id, "name": name, "size": len(raw), "state": "uploaded"}
            api.next_id += 1
            api.remote[name], api.asset_bytes[item["id"]] = item, raw
        name = "Switch_" + VERSION + ".dmg"
        api.derivative = {name: b"synthetic verified DMG"}
        api.proof = {"schema_version": 1, "kind": "release-dmg-derivative",
            "source": {"commit": COMMIT, "tree": TREE, "version": VERSION, "build": BUILD,
                "source_digest": manifest["source_digest"], "manifest_sha256": publisher.digest(assets["candidate-manifest.json"]),
                "app_archive_sha256": manifest["artifact"]["sha256"], "app_tree_sha256": manifest["app_tree_sha256"],
                "release_id": 352, "tag": "v" + VERSION},
            "packaging": {"commit": COMMIT, "run_id": 54321, "run_attempt": 1,
                "run_url": "https://github.com/" + publisher.REPO + "/actions/runs/54321",
                "workflow": ".github/workflows/package-dmg.yml", "reusable_workflow": ".github/workflows/package-dmg.yml"},
            "dmg": {"name": name, "size": len(api.derivative[name]), "sha256": publisher.digest(api.derivative[name])},
            "validation": {"input_archive": True, "mounted_inventory": True, "signature": True,
                "readonly_mount": True, "app_recompiled": False},
            "acceptance": {"signature": "ad-hoc", "notarized": False,
                "macos27_runtime": "UNVERIFIED", "device_acceptance": "UNVERIFIED"}}
        api.dmg_run = {"head_sha": COMMIT, "path": ".github/workflows/package-dmg.yml",
            "event": "workflow_dispatch", "repository": {"full_name": publisher.REPO},
            "html_url": api.proof["packaging"]["run_url"]}
        api.dmg_jobs = [{"name": name, "conclusion": "success", "steps": [{"name": step, "conclusion": "success"}]}
            for name, step in (("DMG verify", "Verify mounted DMG against accepted App"),
                               ("DMG publish", "Publish verified DMG derivative"))]
        api.controller_blobs = {"sha": "e" * 40, "type": "file"}
        original_api, original_pages, original_artifact = api.api, api.pages, api.artifact
        def request(method, suffix, value=None, **kwargs):
            if method == "GET" and suffix == "/actions/runs/54321":
                return copy.deepcopy(api.dmg_run)
            if method == "GET" and suffix.startswith("/contents/") and "BUILD_NUMBER" not in suffix:
                return copy.deepcopy(api.controller_blobs)
            return original_api(method, suffix, value, **kwargs)
        def pages(suffix, key=None):
            if suffix == "/actions/runs/54321/attempts/1/jobs":
                return copy.deepcopy(api.dmg_jobs)
            if suffix == "/actions/runs/54321/artifacts":
                return []
            return original_pages(suffix, key)
        def artifact(artifacts, name, run_id, commit):
            if run_id == 54321:
                if name != "Switch-dmg-" + VERSION + "-" + COMMIT + "-attempt-1" or commit != api.proof["packaging"]["commit"]:
                    raise AssertionError("wrong DMG evidence provenance requested")
                return copy.deepcopy(api.dmg_evidence)
            return original_artifact(artifacts, name, run_id, commit)
        api.api, api.pages, api.artifact = request, pages, artifact
        self.seal(api)
        return api

    def seal(self, api, evidence=True):
        name = api.proof["dmg"]["name"]
        api.derivative[name + ".sha256"] = (publisher.digest(api.derivative[name]) + "  " + name + "\n").encode()
        api.derivative["dmg-provenance.json"] = encode(api.proof)
        for filename, raw in api.derivative.items():
            if filename not in api.remote:
                api.remote[filename] = {"id": api.next_id, "name": filename, "size": len(raw), "state": "uploaded"}
                api.next_id += 1
            item = api.remote[filename]
            item["size"] = len(raw)
            api.asset_bytes[item["id"]] = raw
        if evidence:
            api.dmg_evidence = copy.deepcopy(api.derivative)

    def invoke(self, api):
        PublicationStateFixtures().invoke(api, attempt=2)

    def test_complete_verified_derivative_retry_is_readonly(self):
        api = self.fixture()
        identities = copy.deepcopy(api.remote)
        self.invoke(api)
        self.assertEqual(api.writes, [])
        self.assertEqual(api.remote, identities)
        self.assertEqual(len(api.remote), 7)

    def test_reusable_same_run_jobs_use_exact_names_without_waiting_for_whole_run(self):
        api = self.fixture()
        api.proof["packaging"]["workflow"] = ".github/workflows/release.yml"
        api.dmg_run.update(path=".github/workflows/release.yml", status="in_progress", conclusion=None)
        for job in api.dmg_jobs:
            job["name"] = "dmg / " + job["name"]
        self.seal(api)
        self.invoke(api)
        self.assertEqual(api.writes, [])

    def test_partial_or_unexpected_derivatives_never_mutate(self):
        for missing in ("Switch_3.52.dmg", "Switch_3.52.dmg.sha256", "dmg-provenance.json"):
            api = self.fixture()
            del api.remote[missing]
            with self.subTest(missing=missing), self.assertRaisesRegex(RuntimeError, "incomplete DMG"):
                self.invoke(api)
            self.assertEqual(api.writes, [])
        api = self.fixture()
        api.remote["other.dmg"] = {"id": 999, "name": "other.dmg", "size": 1, "state": "uploaded"}
        with self.assertRaisesRegex(RuntimeError, "unexpected"):
            self.invoke(api)
        self.assertEqual(api.writes, [])

    def test_replaced_dmg_and_self_consistent_provenance_rejected_by_original_artifact(self):
        api = self.fixture()
        name = api.proof["dmg"]["name"]
        api.derivative[name] = b"replacement DMG with rewritten release checksums"
        api.proof["dmg"].update(size=len(api.derivative[name]), sha256=publisher.digest(api.derivative[name]))
        self.seal(api, evidence=False)
        with self.assertRaisesRegex(RuntimeError, "asset differs"):
            self.invoke(api)
        self.assertEqual(api.writes, [])

    def test_source_or_scope_drift_rejected_even_in_matching_artifact(self):
        changes = [("source", "commit", "f" * 40), ("source", "release_id", 999),
                   ("source", "manifest_sha256", "f" * 64), ("source", "app_archive_sha256", "f" * 64),
                   ("source", "app_tree_sha256", "f" * 64), ("source", "tree", "f" * 40),
                   ("source", "tag", "v3.53"), ("source", "version", "3.53"), ("source", "build", "353"),
                   ("validation", "mounted_inventory", False), ("validation", "app_recompiled", True),
                   ("acceptance", "notarized", True), ("acceptance", "macos27_runtime", "PASSED")]
        for area, key, value in changes:
            api = self.fixture()
            api.proof[area][key] = value
            self.seal(api)
            with self.subTest(area=area, key=key), self.assertRaises(RuntimeError):
                self.invoke(api)
            self.assertEqual(api.writes, [])

    def test_failed_missing_or_duplicate_verification_jobs_or_steps_rejected(self):
        for change in ("failed_job", "missing_job", "duplicate_job", "failed_step", "missing_step", "duplicate_step"):
            api = self.fixture()
            if change == "failed_job":
                api.dmg_jobs[0]["conclusion"] = "failure"
            elif change == "missing_job":
                api.dmg_jobs.pop()
            elif change == "duplicate_job":
                api.dmg_jobs.append(copy.deepcopy(api.dmg_jobs[0]))
            elif change == "failed_step":
                api.dmg_jobs[1]["steps"][0]["conclusion"] = "failure"
            elif change == "missing_step":
                api.dmg_jobs[1]["steps"] = []
            else:
                api.dmg_jobs[1]["steps"].append(copy.deepcopy(api.dmg_jobs[1]["steps"][0]))
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                self.invoke(api)
            self.assertEqual(api.writes, [])

    def test_wrong_run_source_workflow_event_repository_or_url_rejected(self):
        for key, value in [("head_sha", "f" * 40), ("path", ".github/workflows/other.yml"),
                           ("event", "pull_request"), ("repository", {"full_name": "other/repo"}),
                           ("html_url", "https://example.invalid/other")]:
            api = self.fixture()
            api.dmg_run[key] = value
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "run provenance"):
                self.invoke(api)

    def test_evidence_missing_extra_or_tampered_is_rejected(self):
        for change in ("missing", "extra", "tampered"):
            api = self.fixture()
            if change == "missing":
                del api.dmg_evidence["dmg-provenance.json"]
            elif change == "extra":
                api.dmg_evidence["unapproved.txt"] = b"extra"
            else:
                api.dmg_evidence["dmg-provenance.json"] += b" "
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                self.invoke(api)
            self.assertEqual(api.writes, [])

    def test_checksum_bytes_must_be_exact(self):
        api = self.fixture()
        item = api.remote["Switch_3.52.dmg.sha256"]
        api.asset_bytes[item["id"]] += b"extra line\n"
        item["size"] = len(api.asset_bytes[item["id"]])
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            self.invoke(api)

    def test_distinct_controller_requires_identical_reviewed_packaging_code(self):
        api = self.fixture()
        api.proof["packaging"]["commit"] = "f" * 40
        api.dmg_run["head_sha"] = "f" * 40
        self.seal(api)
        self.invoke(api)
        self.assertEqual(api.writes, [])
        original = api.api
        def changed(method, suffix, value=None, **kwargs):
            if suffix.startswith("/contents/") and suffix.endswith("ref=" + COMMIT):
                return {"type": "file", "sha": "d" * 40}
            return original(method, suffix, value, **kwargs)
        api.api = changed
        with self.assertRaisesRegex(RuntimeError, "validation code differs"):
            self.invoke(api)
        self.assertEqual(api.writes, [])

    def test_draft_with_derivatives_is_rejected_without_writes(self):
        api = self.fixture()
        api.release["draft"] = True
        with self.assertRaisesRegex(RuntimeError, "incomplete DMG"):
            self.invoke(api)
        self.assertEqual(api.writes, [])

    def test_final_relist_detects_derivative_id_replacement(self):
        api = self.fixture()
        original = api.pages
        def pages(suffix, key=None):
            rows = original(suffix, key)
            if suffix == "/releases/352/assets" and api.asset_lists == 2:
                for item in rows:
                    if item["name"] == "Switch_3.52.dmg":
                        item["id"] = 999
            return rows
        api.pages = pages
        with self.assertRaisesRegex(RuntimeError, "final release asset"):
            self.invoke(api)
        self.assertEqual(api.writes, [])


if __name__ == "__main__":
    unittest.main()
