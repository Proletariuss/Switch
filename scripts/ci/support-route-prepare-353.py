#!/usr/bin/env python3
"""Recover only publication of exact, already-cloud-validated preparation bytes."""
import base64, hashlib, io, json, os, subprocess, urllib.request, urllib.error, zipfile
from pathlib import Path
os.chdir(Path(__file__).resolve().parents[2])
BASE="c90634690fe2c202c35bd5c0d58b163146a7af7a"
PREVIOUS="5a01103759a7af662d246520045b70133c3cb53f"
BRANCH="automation/support-route-prepare-353"
HELPERS={"scripts/ci/support-route-prepare-353.py",".github/workflows/support-route-prepare-353.yml"}
EXPECTED={"docs/optimization/all-rust/api-contract-inventory.json","docs/optimization/all-rust/api-contract-review.json","rust/crates/proletariusd/src/ownership.rs"}
def need(value,message):
    if not value: raise SystemExit(message)
def git(*args): return subprocess.check_output(["git",*args],text=True).strip()
def gh(path): return subprocess.check_output(["gh","api","repos/Proletariuss/Switch"+path])
head=os.environ["GITHUB_SHA"]
need(os.environ.get("GITHUB_ACTIONS")=="true" and os.environ["GITHUB_REPOSITORY"]=="Proletariuss/Switch" and os.environ["GITHUB_REF"]=="refs/heads/"+BRANCH,"wrong runner context")
need(git("rev-parse","HEAD")==head and git("rev-list","--parents","-n","1",head).split()==[head,PREVIOUS],"wrong recovery checkout")
need(set(git("diff","--name-only",BASE,head).splitlines())==HELPERS and not git("status","--porcelain"),"source changed before recovery")
artifact=json.loads(gh("/actions/artifacts/12658859069"))
need(artifact["name"]=="support-route-prepared-evidence" and not artifact["expired"] and artifact["workflow_run"]["id"]==40341222763 and artifact["workflow_run"]["head_sha"]==PREVIOUS,"artifact provenance mismatch")
job=json.loads(gh("/actions/jobs/121636129737"))
need(job["run_id"]==40341222763 and job["head_sha"]==PREVIOUS,"validation job mismatch")
raw=gh("/actions/artifacts/12658859069/zip")
need(hashlib.sha256(raw).hexdigest()=="26cb7a85df5db0a404225fbf2d7ce6ffb9ac8784669e403353b94852bf323517","artifact ZIP bytes changed")
archive=zipfile.ZipFile(io.BytesIO(raw))
need(set(archive.namelist())=={"metadata-and-formatting.patch","rustfmt.patch"} and sum(i.file_size for i in archive.infolist())<1024*1024,"unexpected recovery archive")
patch=archive.read("metadata-and-formatting.patch")
subprocess.run(["git","apply","--check"],input=patch,check=True)
subprocess.run(["git","apply"],input=patch,check=True)
changed=set(git("diff","--name-only").splitlines())
need(changed==EXPECTED,"unexpected generated files")
subprocess.run(["git","diff","--check"],check=True)
need(Path("VERSION").read_text().strip()=="3.53" and Path("BUILD_NUMBER").read_text().strip()=="353","version changed")
evidence=Path(os.environ["GITHUB_WORKSPACE"])/"support-route-prepared-evidence"; evidence.mkdir(exist_ok=True)
for name in archive.namelist(): (evidence/name).write_bytes(archive.read(name))
def api(path,payload):
    request=urllib.request.Request("https://api.github.com/repos/Proletariuss/Switch"+path,data=json.dumps(payload).encode(),headers={"Authorization":"Bearer "+os.environ["GH_TOKEN"],"Accept":"application/vnd.github+json","X-GitHub-Api-Version":"2022-11-28"},method="PATCH" if path.startswith("/git/refs/") else "POST")
    try:
        with urllib.request.urlopen(request,timeout=60) as response: return json.load(response)
    except urllib.error.HTTPError as error:
        print("GitHub API failure",path,error.code,error.read(8192).decode(),flush=True)
        raise
entries=[]
for path in sorted(changed):
    blob=api("/git/blobs",{"content":base64.b64encode(Path(path).read_bytes()).decode(),"encoding":"base64"})
    entries.append({"path":path,"mode":"100644","type":"blob","sha":blob["sha"]})
# The exact approved source tree excludes all one-use helpers by construction.
tree=api("/git/trees",{"base_tree":git("rev-parse",BASE+"^{tree}"),"tree":entries})
commit=api("/git/commits",{"message":"ci: prepare reviewed native support method contract","tree":tree["sha"],"parents":[head]})
report={"approved_source":BASE,"prepared_commit":commit["sha"],"prepared_tree":tree["sha"],"validated_run":40341222763,"validated_job":121636129737,"source_artifact":12658859069,"source_artifact_sha256":hashlib.sha256(raw).hexdigest(),"changed_files":sorted(changed),"formatted_files":["rust/crates/proletariusd/src/ownership.rs"],"retired_helpers":sorted(HELPERS),"version":"3.53","build":"353","functional_acceptance_awarded":False}
(evidence/"prepared.json").write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2),flush=True)
with open(os.environ["GITHUB_STEP_SUMMARY"],"a") as stream: stream.write("Prepared commit: `"+commit["sha"]+"`\nRecovered exact validated artifact bytes; no formatting or metadata regeneration repeated.\n")
api("/git/refs/heads/"+BRANCH,{"sha":commit["sha"],"force":False})
