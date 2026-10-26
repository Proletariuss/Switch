#!/usr/bin/env python3
"""One-use, exact-source review-metadata refresh on GitHub only."""
import hashlib, importlib.util, json, os, re, subprocess, urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
os.chdir(ROOT)
BASE="6c2692c4f0ac667d22bd6e4a446772022d82dbf2"
BRANCH="automation/stability-final-refresh-353"
SOURCE="app/Sources/ReaderBLEBridge.swift"
REVIEW="docs/optimization/all-rust/api-contract-review.json"
INVENTORY="docs/optimization/all-rust/api-contract-inventory.json"
HELPERS=["scripts/ci/final-review-refresh-353.py", ".github/workflows/final-review-refresh-353.yml"]
PINS={SOURCE:"7e97e7b357023ff3a284bc132fdea08590a42565", REVIEW:"8904a6d4948fb1536891f8ad243df9211ee51e4a", INVENTORY:"2e9b822bd9184a6927ce9e669725eb14a7976f4c"}
def need(value, message):
    if not value: raise SystemExit(message)
def git(*args): return subprocess.check_output(["git",*args],text=True).strip()
def run(*args): subprocess.run(args,check=True)
need(os.environ.get("GITHUB_ACTIONS")=="true" and os.environ["GITHUB_REPOSITORY"]=="Proletariuss/Switch", "GitHub repository runner required")
need(os.environ["GITHUB_REF"]=="refs/heads/"+BRANCH,"wrong preparation branch")
head=os.environ["GITHUB_SHA"]
need(re.fullmatch(r"[0-9a-f]{40}",head) and git("rev-parse","HEAD")==head,"wrong checkout")
need(git("rev-list","--parents","-n","1",head).split()==[head,BASE],"helper must directly follow approved source")
need(set(git("diff","--name-only",BASE,head).splitlines())==set(HELPERS),"helper changed source")
need(not git("status","--porcelain"),"dirty checkout")
for path,sha in PINS.items(): need(git("rev-parse",head+":"+path)==sha,"pinned blob mismatch: "+path)
need(Path("VERSION").read_text().strip()=="3.53" and Path("BUILD_NUMBER").read_text().strip()=="353","wrong version")
spec=importlib.util.spec_from_file_location("contracts",ROOT/"scripts/check-rust-migration-contracts.py")
checker=importlib.util.module_from_spec(spec); spec.loader.exec_module(checker)
review=json.loads(Path(REVIEW).read_text())
updates=[]
for row in review["scopes"]:
    text=checker.production(Path(row["file"]).read_text(),"rust" if row["file"].endswith(".rs") else "swift")
    scope=checker.function_scope(text,row["function"])[0] if row.get("function") else text
    actual=hashlib.sha256(scope.encode()).hexdigest() if row.get("fingerprint")=="sha256-raw" else checker.digest(scope)
    if actual==row["sha256"]: continue
    need(row["file"]==SOURCE and not row.get("function"),"unapproved review drift: "+row["file"])
    need(row["sha256"]=="5359b3c44d1ae4d0d98242a2b5fa3aa30503446fd9e7f484b678df8d0afb188f","wrong old fingerprint")
    updates.append({"file":SOURCE,"old_sha256":row["sha256"],"new_sha256":actual})
    row["sha256"]=actual
    row["review"] += "\n\nUser-authorized final 3.53 stability review at "+BASE+": private temporary Unix socket naming reserves one additional byte before the strict sockaddr path limit. The reviewed source change only adjusts nameBudget from maximumPathBytes - directory.utf8.count - 1 to - 2 and explains the boundary. Full UUID, first shortened suffix and minimum 48-bit suffix real-bind boundary tests passed with the full handoff suite in GitHub run 40336101903. No runtime source is modified by this metadata refresh; final integrated-source gates remain required. No physical-device, carrier or product acceptance is awarded."
need(len(updates)==1,"expected exactly one changed review scope")
Path(REVIEW).write_text(json.dumps(review,indent=2,ensure_ascii=False)+"\n")
run("python3","scripts/check-rust-migration-contracts.py","--write-inventory","--require-rust-only")
run("scripts/preflight-source.sh")
run("git","diff","--check")
changed=set(git("diff","--name-only").splitlines())
need(REVIEW in changed and changed<={REVIEW,INVENTORY},"refresh changed nonmetadata files")
need(not git("ls-files","--others","--exclude-standard"),"unexpected untracked files")
# git's complete tracked-tree diff proves all production, version, lock and project blobs unchanged.
run("git","diff","--exit-code","--","app","rust","VERSION","BUILD_NUMBER","project.yml","Switch.xcodeproj","scripts")
patch=subprocess.check_output(["git","diff","--",*sorted(changed)],text=True)
for path in HELPERS: Path(path).unlink()
need(set(git("diff","--name-only").splitlines())==changed|set(HELPERS),"self-retirement changed other files")
evidence=Path(os.environ["GITHUB_WORKSPACE"])/"final-refresh-evidence"; evidence.mkdir(exist_ok=True)
(evidence/"metadata.patch").write_text(patch)
def api(path,payload):
    request=urllib.request.Request("https://api.github.com/repos/Proletariuss/Switch"+path,data=json.dumps(payload).encode(),headers={"Authorization":"Bearer "+os.environ["GH_TOKEN"],"Accept":"application/vnd.github+json","X-GitHub-Api-Version":"2022-11-28"},method="PATCH" if path.startswith("/git/refs/") else "POST")
    with urllib.request.urlopen(request,timeout=60) as response: return json.load(response)
entries=[{"path":path,"mode":"100644","type":"blob","content":Path(path).read_text()} for path in sorted(changed)]
entries += [{"path":path,"mode":"100644","type":"blob","sha":None} for path in HELPERS]
tree=api("/git/trees",{"base_tree":git("rev-parse","HEAD^{tree}"),"tree":entries})
commit=api("/git/commits",{"message":"ci: refresh reviewed BLE socket boundary fingerprint","tree":tree["sha"],"parents":[head]})
report={"approved_source":BASE,"input_commit":head,"prepared_commit":commit["sha"],"prepared_tree":tree["sha"],"changed_metadata":sorted(changed),"retired_helpers":HELPERS,"updated_scopes":updates,"production_blobs_unchanged":True,"version":"3.53","build":"353","functional_acceptance_awarded":False}
(evidence/"prepared.json").write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2),flush=True)
with open(os.environ["GITHUB_STEP_SUMMARY"],"a") as stream: stream.write("Prepared commit: `"+commit["sha"]+"`\n\nProduction/version/lock/project blobs unchanged; only the reviewed BLE fingerprint and derived inventory may change.\n")
api("/git/refs/heads/"+BRANCH,{"sha":commit["sha"],"force":False})
