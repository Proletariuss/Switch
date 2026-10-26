#!/usr/bin/env python3
"""One-use, exact-source review-metadata refresh on GitHub only."""
import hashlib, importlib.util, json, os, re, subprocess, urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
os.chdir(ROOT)
BASE="c90634690fe2c202c35bd5c0d58b163146a7af7a"
BRANCH="automation/support-route-prepare-353"
SOURCE="rust/crates/proletariusd/src/public_http.rs"
REVIEW="docs/optimization/all-rust/api-contract-review.json"
INVENTORY="docs/optimization/all-rust/api-contract-inventory.json"
HELPERS=["scripts/ci/support-route-prepare-353.py", ".github/workflows/support-route-prepare-353.yml"]
PINS={'rust/crates/proletariusd/src/public_http.rs': '8a330079b49db2bdaf5c6d439355b0b1d9f96619', 'rust/crates/proletariusd/src/ownership.rs': '00b8d3811655037198396c0cbf852409cd05ff76', 'rust/crates/proletariusd/src/public_http/tests.rs': 'aad7fb6ae65052b5441cc078c27cc2705f7246d8', 'docs/optimization/all-rust/api-contract-review.json': '63fb547029ecf8d50a5555dd28f8c817842a6f16', 'docs/optimization/all-rust/api-contract-inventory.json': 'f33f99f38fa83ef36bb9bd2a9b0b5221c81e162c'}
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
FORMAT_FILES={path for path in PINS if path.endswith(".rs")}
def tokens(path): return [token for token,_,_ in checker.lex(Path(path).read_text())]
before_tokens={path:tokens(path) for path in FORMAT_FILES}
for manifest in ["rust/Cargo.toml", "scripts/acceptance/daemon-capsule-fixture/Cargo.toml"]:
    run("cargo", "+1.98.0", "fmt", "--manifest-path", manifest, "--all")
formatted=set(git("diff","--name-only").splitlines())
need(formatted <= FORMAT_FILES,"rustfmt changed an unapproved file")
format_token_changes=[]
for path,original in before_tokens.items():
    current=tokens(path)
    need([t for t in original if t!=","]==[t for t in current if t!=","],"rustfmt changed a non-comma token: "+path)
    if current!=original: format_token_changes.append(path)
format_patch=subprocess.check_output(["git","diff","--",*sorted(FORMAT_FILES)],text=True)
review=json.loads(Path(REVIEW).read_text())
updates=[]
for row in review["scopes"]:
    text=checker.production(Path(row["file"]).read_text(),"rust" if row["file"].endswith(".rs") else "swift")
    scope=checker.function_scope(text,row["function"])[0] if row.get("function") else text
    actual=hashlib.sha256(scope.encode()).hexdigest() if row.get("fingerprint")=="sha256-raw" else checker.digest(scope)
    if actual==row["sha256"]: continue
    need(row["file"]==SOURCE and not row.get("function"),"unapproved review drift: "+row["file"])
    need(row["sha256"]=="039df984877b2704b999595ffe7a28c7ed677b0339370512fc65f43a748a3a98","wrong old fingerprint")
    updates.append({"file":SOURCE,"old_sha256":row["sha256"],"new_sha256":actual})
    row["sha256"]=actual
    row["review"] += "\n\nUser-authorized support diagnostics route consistency review at "+BASE+": /api/diagnostics/support is a new native route and is removed only from the independently frozen historical Go method corpus. NATIVE_METHOD_PATHS preserves authenticated Rust-owned 405 rejection with Allow GET, HEAD; GET and HEAD remain read-only. Root reviewed the three-file diff and the bounded real-wire regression for authentication, no-store, unsupported methods, unavailable GET/HEAD behavior, and absence of Go/runtime-data side effects. The frozen Go parity fixture remains unchanged. This preparation only applies pinned Rust formatting and refreshes this source fingerprint and derived census; complete final-source cloud tests remain required. No physical-device, carrier or product acceptance is awarded."
need(len(updates)==1,"expected exactly one changed review scope")
Path(REVIEW).write_text(json.dumps(review,indent=2,ensure_ascii=False)+"\n")
run("python3","scripts/check-rust-migration-contracts.py","--write-inventory","--require-rust-only")
run("scripts/preflight-source.sh")
run("git","diff","--check")
changed=set(git("diff","--name-only").splitlines())
need(REVIEW in changed and changed<={REVIEW,INVENTORY}|FORMAT_FILES,"refresh changed nonmetadata files")
need(not git("ls-files","--others","--exclude-standard"),"unexpected untracked files")
# Complete diff allowlist protects all source outside the three approved Rust files.
run("git","diff","--exit-code","--","app","VERSION","BUILD_NUMBER","project.yml","Switch.xcodeproj","scripts","rust/Cargo.toml","rust/Cargo.lock")
for manifest in ["rust/Cargo.toml", "scripts/acceptance/daemon-capsule-fixture/Cargo.toml"]:
    run("cargo", "+1.98.0", "fmt", "--manifest-path", manifest, "--all", "--", "--check")
patch=subprocess.check_output(["git","diff","--",*sorted(changed)],text=True)
for path in HELPERS: Path(path).unlink()
need(set(git("diff","--name-only").splitlines())==changed|set(HELPERS),"self-retirement changed other files")
evidence=Path(os.environ["GITHUB_WORKSPACE"])/"support-route-prepared-evidence"; evidence.mkdir(exist_ok=True)
(evidence/"metadata-and-formatting.patch").write_text(patch)
(evidence/"rustfmt.patch").write_text(format_patch)
def api(path,payload):
    request=urllib.request.Request("https://api.github.com/repos/Proletariuss/Switch"+path,data=json.dumps(payload).encode(),headers={"Authorization":"Bearer "+os.environ["GH_TOKEN"],"Accept":"application/vnd.github+json","X-GitHub-Api-Version":"2022-11-28"},method="PATCH" if path.startswith("/git/refs/") else "POST")
    with urllib.request.urlopen(request,timeout=60) as response: return json.load(response)
entries=[{"path":path,"mode":"100644","type":"blob","content":Path(path).read_text()} for path in sorted(changed)]
entries += [{"path":path,"mode":"100644","type":"blob","sha":None} for path in HELPERS]
tree=api("/git/trees",{"base_tree":git("rev-parse","HEAD^{tree}"),"tree":entries})
commit=api("/git/commits",{"message":"ci: prepare reviewed native support method contract","tree":tree["sha"],"parents":[head]})
report={"approved_source":BASE,"input_commit":head,"prepared_commit":commit["sha"],"prepared_tree":tree["sha"],"changed_files":sorted(changed),"formatted_files":sorted(formatted),"format_token_changes":format_token_changes,"retired_helpers":HELPERS,"updated_scopes":updates,"production_changes_limited_to_reviewed_rustfmt":True,"version":"3.53","build":"353","functional_acceptance_awarded":False}
(evidence/"prepared.json").write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2),flush=True)
with open(os.environ["GITHUB_STEP_SUMMARY"],"a") as stream: stream.write("Prepared commit: `"+commit["sha"]+"`\n\nOnly the three approved Rust files may receive format-only edits; all other production and version/lock/project blobs are unchanged.\n")
api("/git/refs/heads/"+BRANCH,{"sha":commit["sha"],"force":False})
