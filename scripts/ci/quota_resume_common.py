"""Bounded GitHub-only primitives for one explicitly authorized candidate."""
import base64
import datetime as dt
import hashlib
import io
import json
import os
from pathlib import PurePosixPath
import re
import urllib.error
import urllib.parse
import urllib.request
import zipfile

REPO = "Proletariuss/Switch"
PR = 6
BRANCH = "automation/cloud-four-batches"
AUTH_SHA = "bfc29eb42426a7d0a9ceedf80ce947878c9a72de"
AUTH_PATH = "docs/ci/reviewed-prepare-authorization.json"
AUTH_BLOB = "0c68f47d1cc1063b977e3a98ec7a8b2ff5995f3a"
QUALITY_ID = 396067053
INITIAL_PREPARE_RUN = 40276368396
START = dt.datetime(2026, 11, 1, tzinfo=dt.timezone.utc)
STOP = dt.datetime(2027, 10, 26, tzinfo=dt.timezone.utc) # Last allowed date: 2027-10-25 UTC.
STATE_MARKER = "<!-- switch-quota-resume-state:v1:" + AUTH_SHA + " -->"
TERMINAL_MARKER = "<!-- switch-quota-resume-terminal:v1:" + AUTH_SHA + " -->"
PROOF_ARTIFACT = "quota-provenance"
MAX_JSON = 8 * 1024 * 1024
MAX_REPORT_ZIP = 32 * 1024 * 1024
MAX_CANDIDATE_ZIP = 512 * 1024 * 1024

class Refusal(Exception):
    pass

def require(value, message):
    if not value:
        raise Refusal(message)

def sha(data):
    return hashlib.sha256(data).hexdigest()

def utcnow():
    return dt.datetime.now(dt.timezone.utc)

def iso(value=None):
    return (value or utcnow()).isoformat()

def emit(**values):
    destination = os.environ.get("GITHUB_OUTPUT")
    for key, value in values.items():
        if isinstance(value, bool):
            value = str(value).lower()
        require("\n" not in str(value), "multiline workflow output refused")
        if destination:
            with open(destination, "a") as stream:
                stream.write(str(key) + "=" + str(value) + "\n")
        print(str(key) + "=" + str(value))

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, url):
        return None

class GitHub:
    def __init__(self):
        require(os.environ.get("GITHUB_ACTIONS") == "true", "GitHub Actions runner required")
        require(os.environ.get("GITHUB_REPOSITORY") == REPO, "repository mismatch")
        self.token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        require(self.token, "GitHub token missing")
        self.opener = urllib.request.build_opener(NoRedirect)

    def request(self, method, path, payload=None, limit=MAX_JSON):
        require(path.startswith("/repos/" + REPO + "/"), "API path outside the fixed repository")
        headers = {"Accept": "application/vnd.github+json", "Authorization": "Bearer " + self.token,
                   "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "switch-quota-resume"}
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request("https://api.github.com" + path, data=data, method=method, headers=headers)
        with self.opener.open(request, timeout=45) as response:
            length = response.headers.get("Content-Length")
            require(length is None or int(length) <= limit, "API response exceeds limit")
            raw = response.read(limit + 1)
            require(len(raw) <= limit, "API response exceeds limit")
            return None if not raw else json.loads(raw)

    def get(self, suffix):
        return self.request("GET", "/repos/" + REPO + suffix)

    def write(self, method, suffix, payload=None):
        return self.request(method, "/repos/" + REPO + suffix, payload)

    def pages(self, suffix, key=None, max_pages=5):
        result = []
        for page in range(1, max_pages + 1):
            value = self.get(suffix + ("&" if "?" in suffix else "?") + "per_page=100&page=" + str(page))
            rows = value if key is None else value[key]
            require(isinstance(rows, list), "invalid paginated response")
            result.extend(rows)
            if len(rows) < 100:
                return result
        raise Refusal("API pagination bound exceeded")

    def file(self, path, ref):
        value = self.get("/contents/" + path + "?ref=" + urllib.parse.quote(ref, safe=""))
        require(value.get("encoding") == "base64" and value.get("size", MAX_JSON + 1) <= MAX_JSON, "source file exceeds limit")
        return value, base64.b64decode(value["content"])

    def pinned_pr(self):
        pr = self.get("/pulls/" + str(PR))
        require(pr["state"] == "open" and not pr.get("merged"), "authorized PR is no longer open")
        require(pr["base"]["ref"] == "main" and pr["head"]["ref"] == BRANCH,
                "PR branch binding changed")
        require(pr["head"]["repo"]["full_name"] == REPO, "fork PR is not authorized")
        file, raw = self.file(AUTH_PATH, AUTH_SHA)
        require(file["sha"] == AUTH_BLOB, "root authorization blob mismatch")
        authorization = json.loads(raw)
        require(authorization["schema_version"] == 1 and authorization["functional_acceptance_awarded"] is False,
                "authorization scope mismatch")
        return pr

    def prepared_commit(self, prepared):
        require(re.fullmatch(r"[0-9a-f]{40}", prepared or ""), "invalid prepared SHA")
        commit = self.get("/git/commits/" + prepared)
        require([p["sha"] for p in commit["parents"]] == [AUTH_SHA], "prepared commit is not the unique child of authorization")
        file, _ = self.file(AUTH_PATH, prepared)
        require(file["sha"] == AUTH_BLOB, "prepared authorization blob changed")
        return commit

    def quality_run(self, run_id, source_sha):
        require(isinstance(run_id, int) and run_id > 0, "invalid run ID")
        run = self.get("/actions/runs/" + str(run_id))
        require(run["repository"]["full_name"] == REPO and run["workflow_id"] == QUALITY_ID,
                "workflow run belongs to another repository/workflow")
        require(run["event"] == "workflow_dispatch" and run["head_branch"] == BRANCH
                and run["head_sha"] == source_sha and run["path"] == ".github/workflows/quality.yml",
                "workflow run source/event mismatch")
        return run

    def jobs(self, run_id, attempt):
        return self.pages("/actions/runs/" + str(run_id) + "/attempts/" + str(attempt) + "/jobs", "jobs")

    def artifacts(self, run_id):
        return self.pages("/actions/runs/" + str(run_id) + "/artifacts", "artifacts")

    def artifact(self, run_id, name):
        matches = [a for a in self.artifacts(run_id) if a["name"] == name]
        require(len(matches) == 1 and not matches[0]["expired"], "required artifact missing, expired or ambiguous: " + name)
        artifact = matches[0]
        require(artifact.get("workflow_run", {}).get("id") == run_id, "artifact run mismatch")
        return artifact

    def zip(self, artifact, limit=MAX_REPORT_ZIP):
        require(0 < artifact["size_in_bytes"] <= limit and not artifact["expired"], "artifact size/expiry refusal")
        path = "/repos/" + REPO + "/actions/artifacts/" + str(artifact["id"]) + "/zip"
        request = urllib.request.Request("https://api.github.com" + path, headers={
            "Authorization": "Bearer " + self.token, "Accept": "application/vnd.github+json", "User-Agent": "switch-quota-resume"})
        try:
            response = self.opener.open(request, timeout=45)
        except urllib.error.HTTPError as error:
            require(error.code in (301, 302, 303, 307, 308), "artifact download failed")
            location = error.headers.get("Location", "")
            parsed = urllib.parse.urlparse(location)
            require(parsed.scheme == "https" and parsed.hostname and not parsed.username and not parsed.password,
                    "unsafe artifact redirect")
            # Never forward GITHUB_TOKEN to the signed storage URL.
            response = urllib.request.urlopen(urllib.request.Request(location, headers={"User-Agent": "switch-quota-resume"}), timeout=60)
        with response:
            length = response.headers.get("Content-Length")
            require(length is None or int(length) <= limit, "artifact download exceeds limit")
            raw = response.read(limit + 1)
        require(len(raw) <= limit, "artifact download exceeds limit")
        archive = zipfile.ZipFile(io.BytesIO(raw))
        names = archive.namelist()
        require(len(names) <= 200 and len(names) == len(set(names)), "artifact entry limit/duplicate")
        total = 0
        for entry in archive.infolist():
            path = PurePosixPath(entry.filename)
            require(not path.is_absolute() and ".." not in path.parts and not entry.flag_bits & 1,
                    "unsafe artifact member")
            total += entry.file_size
        require(total <= 2 * 1024 * 1024 * 1024, "artifact expanded-size limit")
        return archive, sha(raw)

def member(archive, name, maximum=MAX_JSON):
    matches = [entry for entry in archive.infolist() if PurePosixPath(entry.filename).name == name and not entry.is_dir()]
    require(len(matches) == 1 and matches[0].file_size <= maximum, "missing/ambiguous/oversized artifact member: " + name)
    with archive.open(matches[0]) as stream:
        raw = stream.read(maximum + 1)
    require(len(raw) <= maximum, "artifact member exceeds limit")
    return raw

def report_pass(report, gate, commit, run_id, attempt=None):
    require(report["gate"] == gate and report["status"] == "passed" and report["exit_code"] == 0,
            "gate did not pass: " + gate)
    require(report["commit"] == commit and str(report["run_id"]) == str(run_id), "gate source/run mismatch")
    if attempt is not None:
        require(int(report["run_attempt"]) == attempt, "gate attempt mismatch")
