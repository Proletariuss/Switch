#!/usr/bin/env python3
"""One fixed PR/candidate continuation. Never merge or repair application code."""
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import urllib.parse

from quota_resume_common import (AUTH_BLOB, AUTH_SHA, BRANCH, INITIAL_PREPARE_RUN,
    MAX_CANDIDATE_ZIP, PROOF_ARTIFACT, PR, QUALITY_ID, REPO, START, STATE_MARKER,
    STOP, TERMINAL_MARKER, GitHub, Refusal, emit, iso, member, report_pass, require, sha, utcnow)


class Coordinator:
    def __init__(self):
        self.api = GitHub()
        self.comments = self.api.pages("/issues/" + str(PR) + "/comments", max_pages=10)
        matches = [c for c in self.comments if STATE_MARKER in c.get("body", "")]
        require(len(matches) <= 1, "duplicate coordinator state markers")
        self.comment_id = None
        self.state = {
            "schema_version": 1, "repository": REPO, "pr": PR, "authorization_sha": AUTH_SHA,
            "authorization_blob": AUTH_BLOB, "branch": BRANCH, "phase": "waiting_prepare",
            "prepare_run_id": INITIAL_PREPARE_RUN, "prepared_sha": None, "build_run_id": None,
            "billing_retries": 0, "last_retry_date": None, "build_dispatch_intent": None,
            "proof": None, "message": "Waiting for GitHub-hosted billing availability; no local job is scheduled."
        }
        if matches:
            comment = matches[0]
            require(comment["user"]["login"] == "github-actions[bot]" and comment["user"]["type"] == "Bot",
                    "state marker was not authored by the Actions bot")
            body = comment["body"]
            require(len(body) <= 60_000, "coordinator state comment too large")
            match = re.search(r"<!-- switch-state-json\n(.*?)\n-->", body, re.S)
            require(match is not None, "state JSON missing")
            value = json.loads(match[1])
            for key in ["schema_version", "repository", "pr", "authorization_sha", "authorization_blob", "branch"]:
                require(value.get(key) == self.state[key], "state authorization pin mismatch")
            require(value.get("prepare_run_id") == INITIAL_PREPARE_RUN, "state changed fixed preparation run")
            require(value.get("phase") in {"waiting_prepare", "verify", "dispatching_build", "building", "done", "blocked"},
                    "unknown coordinator state")
            self.state = value
            self.comment_id = comment["id"]

    def store(self):
        value = json.dumps(self.state, sort_keys=True, separators=(",", ":"))
        require(len(value) < 40_000, "coordinator state exceeds bound")
        body = STATE_MARKER + "\nGitHub-only candidate continuation for PR #6.\n\n" + self.state["message"]
        body += "\n\nNo source merge, arbitrary repair, new secrets or local scheduled task.\n<!-- switch-state-json\n" + value + "\n-->"
        if self.comment_id:
            self.api.write("PATCH", "/issues/comments/" + str(self.comment_id), {"body": body})
        else:
            comment = self.api.write("POST", "/issues/" + str(PR) + "/comments", {"body": body})
            require(comment["user"]["login"] == "github-actions[bot]", "unexpected state comment author")
            self.comment_id = comment["id"]

    def receipt(self, message):
        comments = self.api.pages("/issues/" + str(PR) + "/comments", max_pages=10)
        matches = [c for c in comments if TERMINAL_MARKER in c.get("body", "")]
        require(len(matches) <= 1, "duplicate terminal receipt marker")
        if matches:
            require(matches[0]["user"]["login"] == "github-actions[bot]" and matches[0]["user"]["type"] == "Bot",
                    "terminal marker not authored by Actions bot")
            return
        self.api.write("POST", "/issues/" + str(PR) + "/comments", {"body": TERMINAL_MARKER + "\n" + message})

    def disable(self):
        workflow = os.environ.get("COORDINATOR_WORKFLOW")
        if workflow:
            require(re.fullmatch(r"[A-Za-z0-9_.-]+\.ya?ml", workflow), "invalid coordinator workflow name")
            self.api.write("PUT", "/actions/workflows/" + workflow + "/disable")

    def blocked(self, reason):
        self.state["phase"] = "blocked"
        self.state["message"] = "自动续跑已停止：" + reason + "。未合并源码，也未自动修复业务或测试失败。"
        self.store()
        self.receipt(self.state["message"] + "\n\n授权源码：`" + AUTH_SHA + "`。请查看 [协调运行](https://github.com/" + REPO + "/actions/runs/" + os.environ["GITHUB_RUN_ID"] + ")。")
        emit(mode="blocked")
        self.disable()

    def billing_only(self, run):
        jobs = self.api.jobs(run["id"], run["run_attempt"])
        failed = [j for j in jobs if j["conclusion"] not in ("success", "skipped", None)]
        if not failed or any(j["conclusion"] != "failure" or j.get("steps") for j in failed):
            return False
        for job in failed:
            annotations = self.api.pages("/check-runs/" + str(job["id"]) + "/annotations", max_pages=2)
            if not annotations or not all("job was not started" in a.get("message", "").lower()
                and ("payments have failed" in a["message"].lower() or "spending limit" in a["message"].lower())
                for a in annotations):
                return False
        return True

    def retry_billing(self, run):
        today = utcnow().date().isoformat()
        require(int(self.state.get("billing_retries", 0)) < 370, "billing retry bound reached")
        if self.state.get("last_retry_date") == today:
            emit(mode="wait")
            return
        # Persist the one-per-day intent before the API side effect. Unknown API
        # outcomes are reconciled by run_attempt on the next daily wakeup.
        self.state["last_retry_date"] = today
        self.state["billing_retries"] = int(self.state.get("billing_retries", 0)) + 1
        self.state["message"] = "GitHub 尚未允许计费受限的作业启动；已安排本日唯一一次原运行重试，不修改代码。"
        self.store()
        self.api.write("POST", "/actions/runs/" + str(run["id"]) + "/rerun-failed-jobs", {})
        emit(mode="wait", action="dispatchprepare" if run["id"] == INITIAL_PREPARE_RUN else "retry-billing-build")

    def proof(self, value, require_current=False):
        require(value.get("schema_version") == 1 and value.get("kind") == "pinned-preparation-provenance-reproduction"
                and value.get("provenance_reproduced") is True
                and value.get("semantic_equivalence_proven") is False
                and value.get("product_acceptance_complete") is False, "invalid provenance proof scope")
        require(value["authorization_sha"] == AUTH_SHA and value["authorization_blob"] == AUTH_BLOB
                and value["prepare_run_id"] == INITIAL_PREPARE_RUN, "proof authorization mismatch")
        candidate = self.api.prepared_commit(value["prepared_sha"])
        require(candidate["tree"]["sha"] == value["prepared_tree"], "proof prepared tree mismatch")
        run = self.api.quality_run(INITIAL_PREPARE_RUN, AUTH_SHA)
        require(run["conclusion"] == "success" and run["run_attempt"] == value["prepare_attempt"], "proof preparation run changed")
        if require_current:
            require(value["coordinator_run_id"] == int(os.environ["GITHUB_RUN_ID"])
                    and value["coordinator_attempt"] == int(os.environ["GITHUB_RUN_ATTEMPT"]), "proof is not from this coordinator attempt")
        return value

    def stored_proof(self):
        state = self.state.get("proof")
        require(isinstance(state, dict), "build lacks a recorded provenance proof")
        artifact = self.api.artifact(state["run_id"], PROOF_ARTIFACT)
        require(artifact["id"] == state["artifact_id"], "proof artifact identity changed")
        archive, _ = self.api.zip(artifact)
        raw = member(archive, "provenance-proof.json")
        require(sha(raw) == state["sha256"], "proof artifact bytes changed")
        value = self.proof(json.loads(raw))
        require(value["prepared_sha"] == self.state["prepared_sha"] and value["coordinator_run_id"] == state["run_id"],
                "stored proof candidate/run mismatch")
        return value

    def discover_build(self):
        intent = self.state.get("build_dispatch_intent")
        require(isinstance(intent, dict), "build dispatch has no durable intent")
        runs = self.api.pages("/actions/workflows/" + str(QUALITY_ID) + "/runs?event=workflow_dispatch&branch="
                              + urllib.parse.quote(BRANCH, safe="") + "&head_sha=" + self.state["prepared_sha"], "workflow_runs")
        matches = [run for run in runs if run["created_at"] >= intent["created_after"]]
        require(len(matches) <= 1, "ambiguous candidate build dispatch; refusing duplicate or arbitrary selection")
        if matches:
            run = self.api.quality_run(matches[0]["id"], self.state["prepared_sha"])
            self.state["build_run_id"] = run["id"]
            self.state["phase"] = "building"
            self.state["message"] = "来源复现完成；完整软件门禁和 App 构建已在 [GitHub](" + run["html_url"] + ") 运行。复现不代表语义或产品验收。"
            self.store()
            return run
        return None

    def finish(self, run):
        proof = self.stored_proof()
        prepared = self.state["prepared_sha"]
        attempt = run["run_attempt"]
        jobs = self.api.jobs(run["id"], attempt)
        candidates = [j for j in jobs if j["name"].endswith("candidate")]
        require(len(candidates) == 1 and candidates[0]["conclusion"] == "success", "candidate job did not succeed")
        require(any(s["name"] == "Verify uploaded bytes and executable permissions" and s["conclusion"] == "success"
                    for s in candidates[0]["steps"]), "actual upload roundtrip step did not succeed")
        candidate_artifact = self.api.artifact(run["id"], "Switch-candidate-" + prepared + "-attempt-" + str(attempt))
        report_artifact = self.api.artifact(run["id"], "software-report-" + str(attempt) + "-candidate-and-upload")
        report_zip, _ = self.api.zip(report_artifact)
        roundtrip = json.loads(member(report_zip, "upload-roundtrip.json"))
        report_pass(roundtrip, "upload-roundtrip", prepared, run["id"], attempt)
        archive, _ = self.api.zip(candidate_artifact, MAX_CANDIDATE_ZIP)
        manifest_bytes = member(archive, "candidate-manifest.json")
        manifest = json.loads(manifest_bytes)
        require(manifest["kind"] == "ci-app-candidate" and manifest["commit"] == prepared
                and manifest["tree"] == proof["prepared_tree"]
                and str(manifest["run"]["id"]) == str(run["id"])
                and int(manifest["run"]["attempt"]) == attempt, "candidate manifest provenance mismatch")
        require(re.fullmatch(r"[0-9a-f]{64}", manifest["source_digest"]), "candidate source digest missing")
        acceptance = manifest["acceptance"]
        require(acceptance["product_acceptance_complete"] is False and acceptance["device_acceptance"] == "UNVERIFIED"
                and acceptance["macos27_runtime"] == "UNVERIFIED" and acceptance["signature"] == "ad-hoc"
                and acceptance["notarized"] is False and acceptance["original_v3_51_release"] is False,
                "candidate acceptance scope changed")
        name = manifest["artifact"]["name"]
        require(prepared in name and name.endswith(".app.tar.gz") and "/" not in name, "candidate archive name mismatch")
        infos = [i for i in archive.infolist() if i.filename == name]
        require(len(infos) == 1 and infos[0].file_size == manifest["artifact"]["size"], "candidate archive size mismatch")
        digest = hashlib.sha256()
        with archive.open(infos[0]) as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        require(digest.hexdigest() == manifest["artifact"]["sha256"], "candidate archive SHA-256 mismatch")
        sums = member(archive, "SHA256SUMS").decode()
        require(manifest["artifact"]["sha256"] + "  " + name in sums
                and sha(manifest_bytes) + "  candidate-manifest.json" in sums, "candidate checksum manifest mismatch")
        required = {"portable", "rust", "device-sms-send", "module-network-recovery", "api-request-policy",
                    "sms-forwarding-client", "release-initialization", "app-build", "bundle-audit", "signature"}
        for gate in required:
            raw = member(archive, gate + ".json")
            report = json.loads(raw)
            report_pass(report, gate, prepared, run["id"])
            require(1 <= int(report["run_attempt"]) <= attempt, "gate report attempt outside current run")
            require(sha(raw) == manifest["reports_sha256"][gate + ".json"], "candidate gate report hash mismatch")
        require(self.api.pinned_pr()["head"]["sha"] == prepared, "candidate moved before completion receipt")
        artifact_url = "https://github.com/" + REPO + "/actions/runs/" + str(run["id"]) + "/artifacts/" + str(candidate_artifact["id"])
        reports_url = "https://github.com/" + REPO + "/actions/runs/" + str(run["id"]) + "/artifacts/" + str(report_artifact["id"])
        message = ("GitHub 验收与候选 App 构建已完成。\n\n源码：`" + prepared + "`\n\n"
                   + "- [App、manifest、SHA256SUMS 与测试报告](" + artifact_url + ")\n"
                   + "- [上传回读验证报告](" + reports_url + ")\n"
                   + "- [完整运行](" + run["html_url"] + ")\n\n"
                   + "App tar SHA-256：`" + manifest["artifact"]["sha256"] + "`。\n\n"
                   + "产物保留 7 天；GitHub 标示到期时间：" + candidate_artifact["expires_at"] + "。"
                   + "这是包含提交 SHA 的候选 App，不是原 v3.51 发布；没有 DMG、源码自动合并或本机任务。"
                   + "来源复现不等于语义证明；ad-hoc 签名、macOS 27 真机运行、设备/运营商及产品验收仍未确认。")
        self.state["phase"] = "done"
        self.state["message"] = "完成，停止继续调度。候选来源和真实上传回读证据已核对。"
        self.state["result"] = {"run_id": run["id"], "attempt": attempt, "artifact_id": candidate_artifact["id"],
                                "report_artifact_id": report_artifact["id"], "archive_sha256": manifest["artifact"]["sha256"]}
        self.store()
        self.receipt(message)
        emit(mode="done")
        self.disable()

    def plan(self):
        now = utcnow()
        if now < START:
            emit(mode="wait")
            return
        if now >= STOP:
            self.blocked("自动续跑授权期限已于 2027-10-25 UTC 结束")
            return
        pr = self.api.pinned_pr()
        if self.state["phase"] == "blocked":
            emit(mode="blocked")
            self.disable()
            return
        if self.state.get("build_run_id"):
            prepared = self.state["prepared_sha"]
            require(pr["head"]["sha"] == prepared, "candidate moved after build authorization")
            self.api.prepared_commit(prepared)
            run = self.api.quality_run(self.state["build_run_id"], prepared)
            if run["status"] != "completed":
                emit(mode="wait")
            elif run["conclusion"] == "success":
                self.finish(run)
            elif self.billing_only(run):
                self.retry_billing(run)
            else:
                self.blocked("完整门禁/编译/测试失败；仅计费未启动允许自动重试，详见 " + run["html_url"])
            return
        if self.state["phase"] == "dispatching_build":
            self.stored_proof()
            require(pr["head"]["sha"] == self.state["prepared_sha"], "candidate moved during dispatch reconciliation")
            run = self.discover_build()
            if run is None:
                created = dt.datetime.fromisoformat(self.state["build_dispatch_intent"]["created_after"].replace("Z", "+00:00"))
                if now - created > dt.timedelta(hours=24):
                    self.blocked("构建触发结果不明且 24 小时内未找到唯一运行，未盲目重复触发")
                else:
                    emit(mode="wait")
            else:
                emit(mode="wait")
            return
        run = self.api.quality_run(INITIAL_PREPARE_RUN, AUTH_SHA)
        if run["status"] != "completed":
            require(pr["head"]["sha"] == AUTH_SHA or len(self.api.get("/git/commits/" + pr["head"]["sha"])["parents"]) == 1,
                    "candidate changed while preparation is active")
            emit(mode="wait")
            return
        if run["conclusion"] != "success":
            require(pr["head"]["sha"] == AUTH_SHA, "failed preparation advanced source; independent review is required")
            if self.billing_only(run):
                self.retry_billing(run)
            else:
                self.blocked("准备步骤出现实际失败，未自动修改或覆盖源码；详见 " + run["html_url"])
            return
        prepared = pr["head"]["sha"]
        self.api.prepared_commit(prepared)
        self.state["prepared_sha"] = prepared
        self.state["phase"] = "verify"
        self.state["message"] = "准备运行成功，开始第二个只读 job 复现固定授权转换；尚未触发 App 构建。"
        self.store()
        emit(mode="verify", prepared_sha=prepared, prepare_run_id=INITIAL_PREPARE_RUN,
             prepare_attempt=run["run_attempt"])

    def dispatch_build(self, path):
        require(START <= utcnow() < STOP, "outside automatic continuation window")
        proof_path = Path(path)
        require(proof_path.stat().st_size <= 1024 * 1024, "proof file exceeds limit")
        raw = proof_path.read_bytes()
        proof = self.proof(json.loads(raw), require_current=True)
        require(self.api.pinned_pr()["head"]["sha"] == proof["prepared_sha"], "candidate moved before build dispatch")
        require(self.state["phase"] == "verify" and self.state["prepared_sha"] == proof["prepared_sha"], "coordinator is not waiting for this proof")
        artifact = self.api.artifact(int(os.environ["GITHUB_RUN_ID"]), PROOF_ARTIFACT)
        archive, _ = self.api.zip(artifact)
        require(member(archive, "provenance-proof.json") == raw, "uploaded current-run proof differs from dispatch input")
        self.state["proof"] = {"run_id": proof["coordinator_run_id"], "artifact_id": artifact["id"], "sha256": sha(raw)}
        self.state["phase"] = "dispatching_build"
        # REST dispatch may return 204 on older API versions; reconcile by exact
        # workflow/branch/SHA/time rather than repeating an ambiguous request.
        self.state["build_dispatch_intent"] = {"created_after": utcnow().strftime("%Y-%m-%dT%H:%M:%SZ")}
        self.state["message"] = "只读来源复现通过，已保存本次构建触发意图；这不是语义或产品验收。"
        self.store()
        result = self.api.write("POST", "/actions/workflows/" + str(QUALITY_ID) + "/dispatches", {
            "ref": BRANCH, "inputs": {"prepare_reviewed": "false", "candidate_sha": proof["prepared_sha"], "build_app": "true"}})
        if result and isinstance(result.get("workflow_run_id"), int):
            run = self.api.quality_run(result["workflow_run_id"], proof["prepared_sha"])
            self.state["build_run_id"] = run["id"]
            self.state["phase"] = "building"
            self.store()
        emit(mode="wait", action="dispatch-build", prepared_sha=proof["prepared_sha"])


def main():
    coordinator = Coordinator()
    command = sys.argv[1] if len(sys.argv) > 1 else "plan"
    try:
        if command == "plan":
            coordinator.plan()
        elif command == "verification-failed":
            coordinator.api.pinned_pr()
            require(coordinator.state["phase"] == "verify", "no active provenance verification")
            coordinator.blocked("第二个只读 job 无法完整复现准备树/授权/格式化证据，未触发构建")
        elif command == "dispatch-build":
            require(len(sys.argv) == 4 and sys.argv[2] == "--proof", "usage: dispatch-build --proof work/provenance-proof.json")
            coordinator.dispatch_build(sys.argv[3])
        else:
            raise Refusal("unknown coordinator command")
    except Refusal as error:
        coordinator.blocked(str(error)[:500])

if __name__ == "__main__":
    main()
