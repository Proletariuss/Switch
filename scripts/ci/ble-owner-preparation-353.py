#!/usr/bin/env python3
"""One-use, exact-source review-metadata refresh on GitHub only."""
import base64, hashlib, importlib.util, json, os, re, subprocess, urllib.request, urllib.error
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
os.chdir(ROOT)
BASE="aa428c0948df23a48337d5ac6f8e2f1bdc480ad3"
BRANCH="automation/ble-owner-preparation-353"
SOURCE="app/Sources/ReaderBLEBridge.swift"
REVIEW="docs/optimization/all-rust/api-contract-review.json"
INVENTORY="docs/optimization/all-rust/api-contract-inventory.json"
HELPERS=["scripts/ci/ble-owner-preparation-353.py", ".github/workflows/ble-owner-preparation-353.yml"]
PINS={'app/Sources/ReaderBLEBridge.swift': '69ce0829fad08065d904ee99e8dbf16448b83f1e', 'README.md': '61714bd67a50cde7566a475950b55c8f1c07d6c6', 'docs/optimization/all-rust/api-contract-review.json': '25654edc7ea1c5c46b7774606c193d4a4f15de5f', 'docs/optimization/all-rust/api-contract-inventory.json': '3b6fb315508568fe3fe6c81724e492cb33048268'}
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
    need(row["sha256"]=="25e00298b727ec1afccc7dae4283b2b93259eff5e385c0cf09f2d62af58d5bdd","wrong old fingerprint")
    updates.append({"file":SOURCE,"old_sha256":row["sha256"],"new_sha256":actual})
    row["sha256"]=actual
    row["review"] += "\n\nUser-authorized BLE listener ownership review at "+BASE+": the queued accept worker is the sole descriptor-close owner, including the weak-self-nil path. stop invalidates the generation, attempts shutdown and unlinks only its owned inode without closing the listener. Nonblocking listener acceptance uses bounded poll and generation checks; worker retirement clears matching published state under the lifecycle lock before close, and accepted sockets restore blocking mode for the existing request timeouts. Secure pathname, authentication and operation-ledger semantics remain unchanged. Explicit startup observation and held mock completion remove timing assumptions from tests. The one-shot EIO injection exists only under SWITCH_BLE_BRIDGE_TESTING, enabled by the proof harness and absent from production build inputs. Three independent static reviews approved this source; the combined probe and final integrated-source cloud gates remain pending at preparation time. This fingerprint refresh and README note award no software, physical-device, carrier or product acceptance."
need(len(updates)==1,"expected exactly one changed review scope")
Path(REVIEW).write_text(json.dumps(review,indent=2,ensure_ascii=False)+"\n")
readme=Path("README.md").read_text()
anchor='Bei der BLE-Brücke berücksichtigt die Berechnung temporärer Socket-Namen nun auch die strikte Pfadlängengrenze. Lange, zulässige Laufzeitverzeichnisse können damit einen Socket binden; die Mindestgröße des zufälligen Namens und die Prüfungen vorhandener Dateien bleiben erhalten.'
addition='Listener-Deskriptoren werden erst vom tatsächlich beendeten Empfangs-Worker geschlossen. Stop und Neustart können dadurch keine Verbindung eines nachfolgenden Listeners über eine wiederverwendete Deskriptornummer übernehmen; unerwartete Worker-Ausgänge bereinigen den veröffentlichten Zustand ebenfalls.'
need(readme.count(anchor)==1 and addition not in readme,"approved README insertion point changed")
Path("README.md").write_text(readme.replace(anchor,anchor+" "+addition))
run("python3","scripts/check-rust-migration-contracts.py","--write-inventory","--require-rust-only")
run("scripts/preflight-source.sh")
run("git","diff","--check")
changed=set(git("diff","--name-only").splitlines())
need(REVIEW in changed and changed=={REVIEW,INVENTORY,"README.md"},"refresh changed nonmetadata files")
need(not git("ls-files","--others","--exclude-standard"),"unexpected untracked files")
# git's complete tracked-tree diff proves all production, version, lock and project blobs unchanged.
run("git","diff","--exit-code","--","app","rust","VERSION","BUILD_NUMBER","project.yml","Switch.xcodeproj","scripts")
patch=subprocess.check_output(["git","diff","--",*sorted(changed)],text=True)
for path in HELPERS: Path(path).unlink()
need(set(git("diff","--name-only").splitlines())==changed|set(HELPERS),"self-retirement changed other files")
evidence=Path(os.environ["GITHUB_WORKSPACE"])/"ble-owner-prepared-evidence"; evidence.mkdir(exist_ok=True)
(evidence/"metadata.patch").write_text(patch)
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
tree=api("/git/trees",{"base_tree":git("rev-parse",BASE+"^{tree}"),"tree":entries})
commit=api("/git/commits",{"message":"ci: document reviewed BLE listener ownership and refresh source fingerprint","tree":tree["sha"],"parents":[head]})
report={"approved_source":BASE,"input_commit":head,"prepared_commit":commit["sha"],"prepared_tree":tree["sha"],"changed_files":sorted(changed),"probe_status_at_preparation":"pending","readme_addition":addition,"retired_helpers":HELPERS,"updated_scopes":updates,"production_blobs_unchanged":True,"version":"3.53","build":"353","functional_acceptance_awarded":False}
(evidence/"prepared.json").write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2),flush=True)
with open(os.environ["GITHUB_STEP_SUMMARY"],"a") as stream: stream.write("Prepared commit: `"+commit["sha"]+"`\n\nProduction/test/version/lock/project blobs unchanged; only the approved README addition, reviewed BLE fingerprint and derived inventory changed. Combined probe and final PR gates remain pending.\n")
api("/git/refs/heads/"+BRANCH,{"sha":commit["sha"],"force":False})
