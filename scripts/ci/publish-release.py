#!/usr/bin/env python3
"""Publish only the exact, successful, round-tripped App; never retag or replace assets."""
import base64
import gzip
import hashlib
import io
import json
import os
from pathlib import PurePosixPath
import re
import tarfile
import urllib.error
import urllib.request
import zipfile
from urllib.parse import quote

REPO = "Proletariuss/Switch"
GATES = {"portable", "rust", "device-sms-send", "module-network-recovery", "api-request-policy",
         "sms-forwarding-client", "release-initialization", "app-build", "bundle-audit", "signature"}
LIMIT = 512 * 1024 * 1024

def need(condition, reason):
    if not condition:
        raise RuntimeError(reason)

def digest(data):
    return hashlib.sha256(data).hexdigest()

def backend_version(version):
    need(re.fullmatch(r"[1-9][0-9]*\.[0-9]{2}", version), "invalid marketing version")
    return ".".join(str(int(part)) for part in version.split(".")) + ".0"

def require_release_advance(api, version, build):
    published = [item for item in api.pages("/releases")
                 if re.fullmatch(r"v[1-9][0-9]*\.[0-9]{2}", item["tag_name"]) and not item["draft"]]
    if not published:
        return
    latest = max(published, key=lambda item: tuple(map(int, item["tag_name"][1:].split("."))))
    need(tuple(map(int, version.split("."))) > tuple(map(int, latest["tag_name"][1:].split("."))),
         "release version must increase")
    # Read source metadata from the highest published tag, not a release asset name.
    metadata = api.api("GET", "/contents/BUILD_NUMBER?ref=" + quote(latest["tag_name"], safe=""))
    need(metadata is not None and metadata.get("encoding") == "base64"
         and 0 < metadata.get("size", 0) <= 64, "published build metadata unavailable")
    previous = base64.b64decode(metadata["content"]).decode("ascii").strip()
    need(re.fullmatch(r"[1-9][0-9]*", previous), "invalid published build metadata")
    need(int(build) > int(previous), "release build number must increase")

def checked_zip(raw):
    result = {}
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        need(len(archive.infolist()) <= 200, "too many artifact entries")
        total = 0
        for item in archive.infolist():
            path = PurePosixPath(item.filename)
            need(not path.is_absolute() and ".." not in path.parts and not item.flag_bits & 1, "unsafe ZIP entry")
            if item.is_dir():
                continue
            need(item.filename not in result, "duplicate ZIP entry")
            total += item.file_size
            need(total <= LIMIT, "expanded artifact exceeds limit")
            result[item.filename] = archive.read(item)
    return result

def gate(report, name, commit, run_id, max_attempt, exact=False):
    need(report["gate"] == name and report["status"] == "passed" and report["exit_code"] == 0, "gate failed: " + name)
    need(report["commit"] == commit and str(report["run_id"]) == str(run_id), "gate source/run mismatch")
    attempt = int(report["run_attempt"])
    need(attempt == max_attempt if exact else 1 <= attempt <= max_attempt, "gate attempt mismatch")

def validate_payload(files, reports, commit, tree, version, build, run_id, attempt):
    manifest_raw = files["candidate-manifest.json"]
    manifest = json.loads(manifest_raw)
    need(manifest["schema_version"] == 1 and manifest["kind"] == "ci-app-candidate", "manifest kind mismatch")
    need(manifest["commit"] == commit and manifest["tree"] == tree, "manifest source mismatch")
    need(manifest["version"] == version and manifest["build"] == build, "manifest version/build mismatch")
    need(str(manifest["run"]["id"]) == str(run_id) and int(manifest["run"]["attempt"]) == attempt, "manifest run mismatch")
    need(re.fullmatch(r"[0-9a-f]{64}", manifest["source_digest"]), "missing source digest")
    identity = manifest["identity_validation"]
    need(identity == {"app_version": version, "build_number": build, "backend_version": backend_version(version),
                      "source_digest": manifest["source_digest"], "scope": "plist-and-embedded-identity"}, "App identity mismatch")
    acceptance = manifest["acceptance"]
    need(acceptance["product_acceptance_complete"] is False and acceptance["device_acceptance"] == "UNVERIFIED"
         and acceptance["macos27_runtime"] == "UNVERIFIED" and acceptance["signature"] == "ad-hoc"
         and acceptance["notarized"] is False and acceptance["original_v3_51_release"] is False, "acceptance scope mismatch")
    name = manifest["artifact"]["name"]
    need(name == "Switch-" + version + "-candidate-" + commit + "-macOS27-arm64.app.tar.gz", "archive name mismatch")
    app = files[name]
    need(len(app) == manifest["artifact"]["size"] and digest(app) == manifest["artifact"]["sha256"], "App hash/size mismatch")
    sums = (digest(app) + "  " + name + "\n" + digest(manifest_raw) + "  candidate-manifest.json\n").encode()
    need(files["SHA256SUMS"] == sums, "SHA256SUMS mismatch")
    expected_files = {name, "candidate-manifest.json", "SHA256SUMS"} | {
        "test-reports/" + key for key in manifest["reports_sha256"]}
    need(set(files) == expected_files, "unexpected or missing candidate files")
    test_files = {}
    for report_name, expected in manifest["reports_sha256"].items():
        need(PurePosixPath(report_name).name == report_name, "unsafe report name")
        raw = files["test-reports/" + report_name]
        need(digest(raw) == expected, "report hash mismatch")
        test_files[report_name] = raw
    for name in GATES:
        gate(json.loads(test_files[name + ".json"]), name, commit, run_id, attempt)
    gate(json.loads(reports["upload-roundtrip.json"]), "upload-roundtrip", commit, run_id, attempt, exact=True)
    test_files["candidate-SHA256SUMS"] = files["SHA256SUMS"]
    test_files["upload-roundtrip.json"] = reports["upload-roundtrip.json"]
    test_files["upload-roundtrip.log"] = reports["upload-roundtrip.log"]
    inventory = {row["path"]: row for row in manifest["app_inventory"]}
    need(len(inventory) == len(manifest["app_inventory"]), "duplicate App inventory")
    need(digest(json.dumps(manifest["app_inventory"], sort_keys=True, separators=(",", ":")).encode())
         == manifest["app_tree_sha256"], "App inventory hash mismatch")
    seen = set()
    with tarfile.open(fileobj=io.BytesIO(app), mode="r:gz") as archive:
        for entry in archive:
            path = entry.name.rstrip("/")
            need(path in inventory and path not in seen, "unexpected or duplicate App entry")
            need(not PurePosixPath(path).is_absolute() and ".." not in PurePosixPath(path).parts, "unsafe App path")
            seen.add(path)
            expected = inventory[path]
            need(entry.mode == expected["mode"], "App permission mismatch")
            if expected["kind"] == "file":
                need(entry.isfile() and entry.size == expected["size"] and entry.size <= LIMIT, "App file type/size mismatch")
                with archive.extractfile(entry) as stream:
                    actual = hashlib.sha256()
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                        actual.update(chunk)
                need(actual.hexdigest() == expected["sha256"], "App file hash mismatch")
            elif expected["kind"] == "directory":
                need(entry.isdir(), "App directory mismatch")
            else:
                need(expected["kind"] == "symlink" and entry.issym() and entry.linkname == expected["target"], "App link mismatch")
    need(seen == set(inventory), "missing App entry")
    # Deterministic evidence archive makes publish-only retries byte-identical.
    evidence = io.BytesIO()
    with gzip.GzipFile(fileobj=evidence, mode="wb", mtime=0, filename="") as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name, raw in sorted(test_files.items()):
                entry = tarfile.TarInfo("test-reports/" + name)
                entry.size, entry.mode, entry.mtime = len(raw), 0o644, 0
                archive.addfile(entry, io.BytesIO(raw))
    assets = {manifest["artifact"]["name"]: app, "candidate-manifest.json": manifest_raw,
              "test-reports.tar.gz": evidence.getvalue()}
    assets["SHA256SUMS"] = "".join(digest(raw) + "  " + name + "\n" for name, raw in sorted(assets.items())).encode()
    return assets, manifest

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, url):
        return None

class GitHub:
    def __init__(self):
        self.token = os.environ["GH_TOKEN"]
        self.opener = urllib.request.build_opener(NoRedirect)

    def request(self, method, path, value=None, binary=False, upload=False):
        host = "https://uploads.github.com" if upload else "https://api.github.com"
        need(path.startswith("/repos/" + REPO + "/"), "API repository mismatch")
        data = value if binary else None if value is None else json.dumps(value).encode()
        # Actions ZIP downloads negotiate the GitHub API media type to obtain
        # their redirect; only Release asset downloads request octet-stream.
        release_asset = binary and method == "GET" and re.fullmatch(
            r"/repos/" + re.escape(REPO) + r"/releases/assets/[0-9]+", path) is not None
        headers = {"Authorization": "Bearer " + self.token, "Accept": "application/octet-stream" if release_asset else "application/vnd.github+json",
                   "User-Agent": "switch-reviewed-release", "X-GitHub-Api-Version": "2022-11-28",
                   "Content-Type": "application/octet-stream" if binary else "application/json"}
        request = urllib.request.Request(host + path, data=data, method=method, headers=headers)
        try:
            response = self.opener.open(request, timeout=120)
        except urllib.error.HTTPError as error:
            if method == "GET" and error.code == 404:
                return None
            if method != "GET" or not binary or error.code != 302:
                raise
            location = error.headers["Location"]
            need(location.startswith("https://"), "unsafe download redirect")
            # No token is forwarded to signed artifact/asset storage.
            response = urllib.request.urlopen(location, timeout=120)
        with response:
            maximum = LIMIT if binary and method == "GET" else 8 * 1024 * 1024
            need(int(response.headers.get("Content-Length", "0")) <= maximum, "response size exceeds limit")
            raw = response.read(maximum + 1)
            need(len(raw) <= maximum, "response size exceeds limit")
        return raw if binary and method == "GET" else None if not raw else json.loads(raw)

    def api(self, method, suffix, value=None, **kwargs):
        return self.request(method, "/repos/" + REPO + suffix, value, **kwargs)

    def pages(self, suffix, key=None):
        rows = []
        for page in range(1, 6):
            data = self.api("GET", suffix + ("&" if "?" in suffix else "?") + "per_page=100&page=" + str(page))
            need(data is not None, "missing paginated API resource")
            batch = data if key is None else data[key]
            rows.extend(batch)
            if len(batch) < 100:
                return rows
        raise RuntimeError("API pagination bound exceeded")

    def artifact(self, artifacts, name, run_id, commit):
        matches = [item for item in artifacts if item["name"] == name]
        need(len(matches) == 1 and not matches[0]["expired"], "artifact missing, expired or ambiguous")
        item = matches[0]
        need(item["workflow_run"]["id"] == run_id and item["workflow_run"]["head_sha"] == commit
             and 0 < item["size_in_bytes"] <= LIMIT, "artifact provenance/size mismatch")
        raw = self.api("GET", "/actions/artifacts/" + str(item["id"]) + "/zip", binary=True)
        need(raw is not None, "artifact download missing")
        return checked_zip(raw)

def same_asset(existing, expected):
    need(existing is not None and digest(existing) == digest(expected) and len(existing) == len(expected),
         "existing/uploaded release asset differs; replacement is forbidden")

def main():
    need(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ["GITHUB_REPOSITORY"] == REPO, "GitHub repository runner required")
    need(os.environ["GITHUB_EVENT_NAME"] == "workflow_dispatch", "only explicit release dispatch is allowed")
    commit, tree = os.environ["EXPECTED_COMMIT"], os.environ["EXPECTED_TREE"]
    version, build = os.environ["EXPECTED_VERSION"], os.environ["EXPECTED_BUILD"]
    need(re.fullmatch(r"[0-9a-f]{40}", commit) and commit == os.environ["GITHUB_SHA"], "release SHA mismatch")
    need(re.fullmatch(r"[1-9][0-9]*\.[0-9]{2}", version) and re.fullmatch(r"[1-9][0-9]*", build), "invalid release version")
    tag = "v" + version
    need(tag != "v3.51", "the published v3.51 release is immutable")
    api = GitHub()
    run_id, current_attempt = int(os.environ["GITHUB_RUN_ID"]), int(os.environ["GITHUB_RUN_ATTEMPT"])
    need(1 <= current_attempt <= 50, "run attempt bound exceeded")
    run = api.api("GET", "/actions/runs/" + str(run_id))
    need(run["head_sha"] == commit and run["path"] == ".github/workflows/release.yml"
         and run["event"] == "workflow_dispatch" and run["repository"]["full_name"] == REPO, "release run mismatch")
    actual_commit = api.api("GET", "/git/commits/" + commit)
    need(actual_commit["tree"]["sha"] == tree, "source tree mismatch")
    # A publish-only rerun must use the original successful candidate attempt.
    attempt = None
    for candidate_attempt in range(current_attempt, 0, -1):
        jobs = api.pages("/actions/runs/" + str(run_id) + "/attempts/" + str(candidate_attempt) + "/jobs", "jobs")
        candidates = [job for job in jobs if job["name"] == "candidate / candidate" and job.get("steps")]
        need(len(candidates) <= 1, "ambiguous candidate job")
        if candidates:
            job = candidates[0]
            need(job["conclusion"] == "success" and any(step["name"] == "Verify uploaded bytes and executable permissions"
                 and step["conclusion"] == "success" for step in job["steps"]), "latest candidate/roundtrip did not succeed")
            attempt = candidate_attempt
            break
    need(attempt is not None, "no successful source-bound candidate job")
    artifacts = api.pages("/actions/runs/" + str(run_id) + "/artifacts", "artifacts")
    files = api.artifact(artifacts, "Switch-candidate-" + commit + "-attempt-" + str(attempt), run_id, commit)
    reports = api.artifact(artifacts, "software-report-" + str(attempt) + "-candidate-and-upload", run_id, commit)
    assets, manifest = validate_payload(files, reports, commit, tree, version, build, run_id, attempt)
    marker = "<!-- switch-release:v1 source=" + commit + " manifest=" + digest(assets["candidate-manifest.json"]) + " -->"
    existing_tag = api.api("GET", "/git/ref/tags/" + tag)
    if existing_tag:
        need(existing_tag["object"]["type"] == "commit" and existing_tag["object"]["sha"] == commit, "tag conflict; retagging is forbidden")
    release = api.api("GET", "/releases/tags/" + tag)
    if release is None:
        # Draft releases are not always returned by the tag lookup.
        drafts = [item for item in api.pages("/releases") if item["tag_name"] == tag]
        need(len(drafts) <= 1, "ambiguous release")
        release = drafts[0] if drafts else None
    if release:
        need(marker in release.get("body", "") and release["tag_name"] == tag, "existing release identity conflict")
    if release is None or release["draft"]:
        require_release_advance(api, version, build)
    if release is None:
        body = (marker + "\n## Switch " + version + "\n\n"
            + "Softwarestand aus Commit `" + commit + "`, Build " + build + ".\n\n"
            + "Alle Software-Prüfungen, Bundle-Prüfung und der Upload-Rücklesetest gehören zum selben GitHub-Lauf: "
            + run["html_url"] + ". Die veröffentlichten App-Bytes wurden unverändert übernommen.\n\n"
            + "Das App-Archiv erhält Dateirechte und symbolische Links. SHA256SUMS bindet App und Manifest; "
            + "test-reports.tar.gz enthält die Testberichte und den Rücklesebericht. Kein DMG.\n\n"
            + "Ad-hoc signiert, nicht von Apple notarisiert. Hardware, Mobilfunkanbieter, reale Ausführung unter macOS 27 "
            + "und Langzeitstabilität sind nicht abgenommen. Dies ist keine vollständige Produktabnahme.\n\n"
            + "GitHub-Actions-Zwischenartefakte bleiben 7 Tage verfügbar; diese Release-Anlagen werden nicht automatisch nach 7 Tagen gelöscht.")
        release = api.api("POST", "/releases", {"tag_name": tag, "target_commitish": commit, "name": "Switch " + version + " — Softwarestand",
                                              "body": body, "draft": True, "prerelease": False})
    remote = api.pages("/releases/" + str(release["id"]) + "/assets")
    need(len({item["name"] for item in remote}) == len(remote) and {item["name"] for item in remote} <= set(assets), "unexpected release assets")
    checked_ids = {}
    for name, raw in assets.items():
        found = next((item for item in remote if item["name"] == name), None)
        if found is None:
            need(release["draft"], "published release is incomplete; mutation refused")
            found = api.api("POST", "/releases/" + str(release["id"]) + "/assets?name=" + quote(name, safe=""), raw, binary=True, upload=True)
        need(found["size"] == len(raw) and found["state"] == "uploaded", "release asset size/state mismatch")
        same_asset(api.api("GET", "/releases/assets/" + str(found["id"]), binary=True), raw)
        checked_ids[name] = found["id"]
    final_assets = api.pages("/releases/" + str(release["id"]) + "/assets")
    need(len(final_assets) == len(assets) == 4
         and {item["name"]: item["id"] for item in final_assets} == checked_ids,
         "final release asset whitelist/identity changed")
    if release["draft"]:
        require_release_advance(api, version, build)
    # Tag creation is delayed until the entire verified draft is present.
    existing_tag = api.api("GET", "/git/ref/tags/" + tag)
    if existing_tag is None:
        need(release["draft"], "published release tag missing; mutation refused")
        api.api("POST", "/git/refs", {"ref": "refs/tags/" + tag, "sha": commit})
    final_tag = api.api("GET", "/git/ref/tags/" + tag)
    need(final_tag["object"]["type"] == "commit" and final_tag["object"]["sha"] == commit, "tag changed before publication")
    if release["draft"]:
        require_release_advance(api, version, build)
        release = api.api("PATCH", "/releases/" + str(release["id"]), {"draft": False})
    need(release["draft"] is False, "release did not become public")
    print("Published verified release: " + release["html_url"])
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
        summary.write("Release: " + release["html_url"] + "\n\nSource: `" + commit + "`\n")

if __name__ == "__main__":
    main()
