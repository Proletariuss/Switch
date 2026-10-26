#!/usr/bin/env python3
"""GitHub-only candidate version update; no build or release."""
import json
import os
from pathlib import Path
import subprocess
import tomllib
ROOT = Path(__file__).resolve().parents[2]
os.chdir(ROOT)
BASE = "8f7c7d1ab60614a5b6e4b8a3cd9caf37e216f1cf"
BRANCH = "automation/version-candidate-353"
SELF = "scripts/ci/prepare-version-353.py"
WORKFLOW = ".github/workflows/version-candidate-353.yml"
ALLOWED = {"VERSION","BUILD_NUMBER","project.yml","Switch.xcodeproj/project.pbxproj",
           "rust/Cargo.toml","rust/Cargo.lock","scripts/acceptance/daemon-capsule-fixture/Cargo.lock"}
def require(value, reason):
    if not value:
        raise SystemExit(reason)
def git(*args):
    return subprocess.check_output(["git",*args],text=True).strip()
def run(*args):
    subprocess.run(args,check=True)
def api(path,value=None):
    command=["gh","api","repos/Proletariuss/Switch/"+path]
    if value is not None:
        command += ["-X","PATCH" if path.startswith("git/refs/heads/") else "POST","--input","-"]
    return json.loads(subprocess.check_output(command,input=None if value is None else json.dumps(value),text=True))
def external(path):
    return sorted((row for row in tomllib.loads(Path(path).read_text())["package"] if "source" in row),
                  key=lambda row:(row["name"],row["version"],row["source"]))
expected=os.environ["EXPECTED_COMMIT"]
require(os.environ.get("GITHUB_ACTIONS")=="true" and os.environ["GITHUB_REPOSITORY"]=="Proletariuss/Switch","runner mismatch")
require(os.environ["GITHUB_REF"]=="refs/heads/"+BRANCH,"branch mismatch")
require(git("rev-parse","HEAD")==expected==os.environ["GITHUB_SHA"] and git("rev-parse","HEAD^")==BASE,"source mismatch")
require(not git("status","--porcelain"),"checkout is dirty")
require(Path("VERSION").read_text().strip()=="3.52" and Path("BUILD_NUMBER").read_text().strip()=="352","candidate baseline changed")
locks=["rust/Cargo.lock","scripts/acceptance/daemon-capsule-fixture/Cargo.lock"]
before={path:external(path) for path in locks}
run("bash","scripts/bump-version.sh","3.53")
run("xcodegen","generate","--spec","project.yml")
for path in locks:
    require(external(path)==before[path],"external dependency graph changed: "+path)
run("python3","scripts/check-version-contract.py")
for manifest in ("rust/Cargo.toml","scripts/acceptance/daemon-capsule-fixture/Cargo.toml"):
    run("cargo","+1.98.0","metadata","--manifest-path",manifest,"--locked","--offline","--no-deps","--format-version","1")
run("git","diff","--check")
require(not git("ls-files","--others","--exclude-standard"),"unexpected generated file")
changed=set(git("diff","--name-only").splitlines())
require(changed <= ALLOWED and {"VERSION","BUILD_NUMBER","rust/Cargo.toml"} <= changed,"unexpected version diff")
require(api("git/ref/heads/"+BRANCH)["object"]["sha"]==expected,"branch advanced")
entries=[]
for path in sorted(changed):
    entries.append({"path":path,"mode":git("ls-tree","HEAD","--",path).split()[0],
                    "type":"blob","content":Path(path).read_text()})
for path in (SELF,WORKFLOW):
    entries.append({"path":path,"mode":"100755" if path==SELF else "100644","type":"blob","sha":None})
tree=api("git/trees",{"base_tree":git("rev-parse","HEAD^{tree}"),"tree":entries})["sha"]
commit=api("git/commits",{"message":"chore: prepare Switch 3.53 build 353 candidate","tree":tree,"parents":[expected]})["sha"]
report={"candidate_commit":commit,"tree":tree,"baseline":BASE,"version":"3.53","build":"353",
        "rust_version":"3.53.0","external_dependencies_unchanged":True,"changed_files":sorted(changed),
        "software_acceptance_awarded":False,"published":False}
evidence=Path(os.environ["GITHUB_WORKSPACE"])/"version-evidence"
evidence.mkdir(exist_ok=True)
(evidence/"candidate-version.json").write_text(json.dumps(report,indent=2)+"\n")
(evidence/"version.patch").write_text(subprocess.check_output(["git","diff"],text=True))
print(json.dumps(report,indent=2),flush=True)
api("git/refs/heads/"+BRANCH,{"sha":commit,"force":False})
with open(os.environ["GITHUB_STEP_SUMMARY"],"a") as stream:
    stream.write("Prepared version candidate: "+commit+"\nNot built or published.\n")
