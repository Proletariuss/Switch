#!/bin/sh
set -eu

# Run the copied release scripts in an isolated fake repository workspace. The
# hdiutil shim writes plain text; this test never creates a real app or DMG.
case "$(basename "$0")" in
  xcodegen) exit 0 ;;
  xcodebuild)
    derived_data=
    version=
    build=
    bundle_id=
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -derivedDataPath) shift; derived_data=$1 ;;
        MARKETING_VERSION=*) version=${1#*=} ;;
        CURRENT_PROJECT_VERSION=*) build=${1#*=} ;;
        PRODUCT_BUNDLE_IDENTIFIER=*) bundle_id=${1#*=} ;;
      esac
      shift
    done
    [ -n "$derived_data" ] && [ -n "$version" ] && [ -n "$build" ] &&
      [ "$bundle_id" = com.proletarius.switch ] || exit 2
    printf 'xcodebuild\n' >> "$MOCK_WORKSPACE/build/events"
    app="$derived_data/Build/Products/Release/Switch.app"
    mkdir -p "$app/Contents/Resources/backend"
    printf 'mock app\n' > "$app/Contents/mock.txt"
    if [ "$MOCK_FAILURE" = app_identity ]; then version=3.00; fi
    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$build</string>
<key>CFBundleIdentifier</key><string>$bundle_id</string>
</dict></plist>
PLIST
    source_digest=$(python3 "$MOCK_REPO/scripts/source-identity.py")
    backend_version=$(awk '/^\[workspace\.package\]$/ { section=1; next } /^\[/ { section=0 } section && /^version[[:space:]]*=/ { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/[\"]/, ""); print; exit }' "$MOCK_REPO/rust/Cargo.toml")
    printf '%s\n%s\n%s\n%s\n' "$backend_version" "$version" "$build" "$source_digest" > "$app/Contents/Resources/backend/switchd-rs"
    # Keep fixture-only evidence under the source identity helper's excluded
    # build tree so writing it cannot change the digest being tested.
    mkdir -p "$MOCK_WORKSPACE/build"
    printf '%s\n' "$source_digest" > "$MOCK_WORKSPACE/build/backend.digest"
    if [ "$MOCK_FAILURE" = source_mutation ]; then printf '\nmutation\n' >> "$MOCK_REPO/VERSION"; fi
    if [ "$MOCK_FAILURE" = evidence_mutation ]; then
      printf '\nmutation\n' >> "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
    fi
    exit 0
    ;;
  codesign)
    for last_argument do :; done
    [ -f "$last_argument/Contents/mock.txt" ]
    if [ "$MOCK_FAILURE" = evidence_mutation_after_identity ]; then
      printf '\nlate mutation\n' >> "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
    fi
    if [ "$MOCK_FAILURE" = manifest_source_mutation ]; then
      printf '\nmutation after app identity check\n' >> "$MOCK_REPO/VERSION"
    fi
    exit
    ;;
  ditto)
    printf 'ditto %s %s\n' "$1" "$2" >> "$MOCK_WORKSPACE/build/events"
    /bin/cp -R "$1" "$2"
    if [ "$1" = "${MOCK_COLD_APP:-}" ]; then
      if [ "$MOCK_FAILURE" = cold_app_tamper ]; then
        printf 'tampered\n' >> "$2/Contents/mock.txt"
      fi
      if [ "$MOCK_FAILURE" = evidence_mutation ]; then
        printf '\nmutation\n' >> "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
      fi
    fi
    exit
    ;;
  hdiutil)
    printf 'hdiutil-%s\n' "$1" >> "$MOCK_WORKSPACE/build/events"
    action=$1
    shift
    case "$action" in
      create)
        for output do :; done
        printf 'mock image\n' > "$output"
        if [ "$MOCK_FAILURE" = evidence_mutation_after_identity ]; then
          printf '\nlate mutation\n' >> "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
        fi
        [ "$MOCK_FAILURE" != create ]
        ;;
      verify)
        [ "$MOCK_FAILURE" != verify ] && grep -qx 'mock image' "$1"
        ;;
      *) exit 2 ;;
    esac
    exit
    ;;
  cp)
    if [ "${1:-}" = -R ]; then
      [ "$1 $2" = '-R -X' ] && [ "${3:-}" != -p ] || exit 2
      printf 'safe-copy\n' >> "$MOCK_WORKSPACE/build/events"
    fi
    /bin/cp "$@"
    if [ "$MOCK_FAILURE" = archive_copy ] && [ "${1:-}" = -p ]; then
      for output do :; done
      case "$output" in
        "$MOCK_WORKSPACE"/dist/archive/.Switch-*.dmg.*)
          printf 'corruption\n' >> "$output"
          ;;
      esac
    fi
    if [ "$MOCK_FAILURE" = manifest_archive_copy ] && [ "${1:-}" = -p ]; then
      for output do :; done
      case "$output" in
        "$MOCK_WORKSPACE"/dist/archive/.Switch-*-manifest.json.*)
          printf 'corruption\n' >> "$output"
          ;;
      esac
    fi
    exit
    ;;
  ln)
    if [ "$MOCK_FAILURE" = manifest_archive_publish ]; then
      case "${3:-}" in
        "$MOCK_WORKSPACE"/dist/archive/*-manifest.json) exit 1 ;;
      esac
    fi
    /bin/ln "$@"
    exit
    ;;
  mv)
    printf 'mv %s %s\n' "$MOCK_FAILURE" "${2:-}" >> "$MOCK_WORKSPACE/build/events"
    if [ "$MOCK_FAILURE" = evidence_mutation_during_manifest ] &&
       [ "${2:-}" != "$MOCK_WORKSPACE/dist/release-manifest.json" ] &&
       [ "$(basename "${2:-missing}")" = release-manifest.json ]; then
      printf '\nmanifest-time mutation\n' >> "$SWITCH_SOFTWARE_RELEASE_EVIDENCE"
    fi
    if [ "$MOCK_FAILURE" = manifest_publish ] &&
       [ "${2:-}" = "$MOCK_WORKSPACE/dist/release-manifest.json" ]; then
      exit 1
    fi
    /bin/mv "$@"
    exit
    ;;
esac

SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
SWITCH_SOURCE_ROOT="$SOURCE_ROOT"
. "$SOURCE_ROOT/scripts/workspace-env.sh"
FIXTURE=$(mktemp -d "${TMPDIR:-/tmp}/switch-release-flow-test.XXXXXX")
FIXTURE=$(CDPATH= cd -- "$FIXTURE" && pwd)
trap 'rm -rf "$FIXTURE"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$FIXTURE/bin"
for command_name in xcodegen xcodebuild codesign ditto hdiutil cp ln mv; do
  ln -s "$SOURCE_ROOT/scripts/test-release-flow.sh" "$FIXTURE/bin/$command_name"
done

set -- $(sh "$SOURCE_ROOT/scripts/version.sh")
VERSION=$1
BUILD_NUMBER=$2
DMG_NAME="Switch-$VERSION-macOS27.dmg"

fail() {
  echo "release flow mock test failed: $*" >&2
  exit 1
}

FLOW_CASES=0
new_case() {
  FLOW_CASES=$((FLOW_CASES + 1))
  case_name=$1
  MOCK_WORKSPACE="$FIXTURE/$case_name"
  MOCK_REPO="$MOCK_WORKSPACE/src"
  CASE_TEMP="$MOCK_WORKSPACE/build/tmp"
  mkdir -p "$MOCK_REPO/scripts" "$MOCK_WORKSPACE/dist" \
    "$MOCK_WORKSPACE/dist/archive" "$CASE_TEMP"
  for script_name in release.sh package-dmg.sh sign-release-app.sh \
    generate-release-manifest.sh source-identity.py verify-app-identity.py \
    version.sh workspace-env.sh; do
    /bin/cp "$SOURCE_ROOT/scripts/$script_name" "$MOCK_REPO/scripts/$script_name"
  done
  /bin/cp "$SOURCE_ROOT/VERSION" "$SOURCE_ROOT/BUILD_NUMBER" "$MOCK_REPO/"
  mkdir -p "$MOCK_REPO/rust"
  /bin/cp "$SOURCE_ROOT/rust/Cargo.toml" "$MOCK_REPO/rust/Cargo.toml"
  # This suite validates atomic release publication with fake tools only. The
  # real source preflight is run independently against the actual checkout.
  cat > "$MOCK_REPO/scripts/preflight-source.sh" <<'PREFLIGHT'
#!/bin/sh
case "${1:-}" in
  --final-release)
    [ "$#" -eq 1 ] && [ "${MOCK_PRODUCT_COMPLETE:-false}" = true ] || exit 1
    ;;
  --software-release-evidence)
    [ "$#" -eq 2 ] && [ -f "$2" ] && grep -qx 'valid software evidence' "$2" || exit 1
    ;;
  *) exit 2 ;;
esac
[ "${MOCK_FAILURE:-}" != preflight ]
PREFLIGHT
  /bin/chmod +x "$MOCK_REPO/scripts/preflight-source.sh"
  cat > "$MOCK_REPO/scripts/check-no-device-software-acceptance.py" <<'SOFTWARE_CHECK'
#!/usr/bin/env python3
import hashlib, os, pathlib, sys
def valid(path):
    return pathlib.Path(path).read_text() == "valid software evidence\n"
def tree(root):
    root = pathlib.Path(root)
    return [(str(path.relative_to(root)), hashlib.sha256(path.read_bytes()).hexdigest())
            for path in sorted(root.rglob("*")) if path.is_file() and not path.is_symlink()]
def tree_hash(root):
    return hashlib.sha256(repr(tree(root)).encode()).hexdigest()
if len(sys.argv) < 3 or not valid(sys.argv[2]):
    raise SystemExit(1)
if sys.argv[1] == "verify" and len(sys.argv) == 3:
    print("no-device software acceptance: PASS")
elif sys.argv[1] == "verified-app" and len(sys.argv) == 3:
    print(os.environ["MOCK_COLD_APP"])
elif sys.argv[1] == "verified-bindings" and len(sys.argv) == 3:
    app = pathlib.Path(os.environ["MOCK_COLD_APP"])
    main = app / "Contents/mock.txt"
    backend = app / "Contents/Resources/backend/switchd-rs"
    if os.environ.get("MOCK_FAILURE") == "bad_bindings":
        print("invalid", "invalid", "invalid")
    else:
        print(tree_hash(app), hashlib.sha256(main.read_bytes()).hexdigest(),
              hashlib.sha256(backend.read_bytes()).hexdigest())
elif sys.argv[1] == "verify-app-copy" and len(sys.argv) == 4:
    if tree(os.environ["MOCK_COLD_APP"]) != tree(sys.argv[3]):
        raise SystemExit(1)
    print("attested App copy: PASS")
elif sys.argv[1] == "verify-dmg-app" and len(sys.argv) == 4:
    with open(pathlib.Path(os.environ["MOCK_WORKSPACE"]) / "build/events", "a") as stream:
        stream.write("verify-dmg-app " + sys.argv[3] + "\n")
    if pathlib.Path(sys.argv[3]).read_text() != "mock image\n":
        raise SystemExit(1)
    if os.environ.get("MOCK_FAILURE") == "dmg_payload_tamper":
        raise SystemExit(1)
    if os.environ.get("MOCK_FAILURE") == "dmg_mutation_after_verification":
        pathlib.Path(sys.argv[3]).write_text("mutated image\n")
    print("attested DMG App: PASS")
else:
    raise SystemExit(1)
SOFTWARE_CHECK
  /bin/chmod +x "$MOCK_REPO/scripts/check-no-device-software-acceptance.py"
  # Fixture-only replacements: real bundle/signature and semantic tests execute
  # independently. The delivered release/package entry points have no bypass.
  cat > "$MOCK_REPO/scripts/test-release-initialization.sh" <<'INIT_GATE'
#!/bin/sh
[ "$#" -eq 0 ] || exit 2
printf 'initialization\n' >> "$MOCK_WORKSPACE/build/events"
[ "$MOCK_FAILURE" != initialization ]
INIT_GATE
  /bin/chmod +x "$MOCK_REPO/scripts/test-release-initialization.sh"
  cat > "$MOCK_REPO/scripts/audit-release-bundle.py" <<'BUNDLE_GATE'
import os,pathlib,sys
p=pathlib.Path(os.environ['MOCK_WORKSPACE'])/'build/events'
previous=p.read_text() if p.exists() else ''
with p.open('a') as f:f.write('bundle-audit\n')
n=previous.splitlines().count('bundle-audit')
assert (n==0 and len(sys.argv)==2) or (n==1 and len(sys.argv)==4 and sys.argv[2]=='--stage-root' and pathlib.Path(sys.argv[1]).parent==pathlib.Path(sys.argv[3]))
if (os.environ['MOCK_FAILURE']=='audit_input' and n==0) or (os.environ['MOCK_FAILURE']=='audit_stage' and n==1):sys.exit(1)
BUNDLE_GATE
  MOCK_SOFTWARE_EVIDENCE=
  MOCK_COLD_APP="$MOCK_WORKSPACE/build/cold/Switch.app"
  mkdir -p "$MOCK_COLD_APP/Contents/Resources/backend"
  printf 'mock app\n' > "$MOCK_COLD_APP/Contents/mock.txt"
  cat > "$MOCK_COLD_APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleShortVersionString</key><string>$VERSION</string>
<key>CFBundleVersion</key><string>$BUILD_NUMBER</string>
<key>CFBundleIdentifier</key><string>com.proletarius.switch</string>
</dict></plist>
PLIST
  source_digest=$(python3 "$MOCK_REPO/scripts/source-identity.py")
  backend_version=$(awk '/^\[workspace\.package\]$/ { section=1; next } /^\[/ { section=0 } section && /^version[[:space:]]*=/ { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/[\"]/, ""); print; exit }' "$MOCK_REPO/rust/Cargo.toml")
  printf '%s\n%s\n%s\n%s\n' "$backend_version" "$VERSION" "$BUILD_NUMBER" "$source_digest" > \
    "$MOCK_COLD_APP/Contents/Resources/backend/switchd-rs"
  printf 'mock libusb\n' > "$MOCK_COLD_APP/Contents/Resources/backend/libusb-1.0.0.dylib"
  DIST_DMG="$MOCK_WORKSPACE/dist/$DMG_NAME"
  ARCHIVE_DMG="$MOCK_WORKSPACE/dist/archive/$DMG_NAME"
  ARCHIVE_MANIFEST="$MOCK_WORKSPACE/dist/archive/Switch-$VERSION-macOS27-manifest.json"
}

run_release() {
  scenario=$1
  product_complete=true
  if [ "$scenario" = frozen_denominator_only ]; then product_complete=false; fi
  MOCK_REPO="$MOCK_REPO" MOCK_WORKSPACE="$MOCK_WORKSPACE" MOCK_FAILURE="$scenario" \
    MOCK_PRODUCT_COMPLETE="$product_complete" \
    SWITCH_NOTARY_PROFILE= \
    SWITCH_SOFTWARE_RELEASE_EVIDENCE="$MOCK_SOFTWARE_EVIDENCE" \
    MOCK_COLD_APP="$MOCK_COLD_APP" \
    PATH="$FIXTURE/bin:$PATH" \
    sh "$MOCK_REPO/scripts/release.sh" > "$MOCK_WORKSPACE/build/output.log" 2>&1
}

assert_clean() {
  [ ! -e "$DIST_DMG" ] || fail "dist DMG leaked in $case_name"
  [ ! -e "$ARCHIVE_DMG" ] || fail "archive DMG leaked in $case_name"
  [ ! -e "$ARCHIVE_MANIFEST" ] && [ ! -L "$ARCHIVE_MANIFEST" ] ||
    fail "archive manifest leaked in $case_name"
  [ -z "$(find "$MOCK_WORKSPACE/dist" -maxdepth 1 -name '.Switch-*-release.*' -print)" ] ||
    fail "dist staging leaked in $case_name"
  [ -z "$(find "$MOCK_WORKSPACE/dist/archive" -maxdepth 1 -name '.Switch-*.dmg.*' -print)" ] ||
    fail "archive staging leaked in $case_name"
  [ -z "$(find "$MOCK_WORKSPACE/dist/archive" -maxdepth 1 -name '.Switch-*-manifest.json.*' -print)" ] ||
    fail "archive manifest staging leaked in $case_name"
  [ -z "$(find "$CASE_TEMP" -maxdepth 1 -name 'switch-release.*' -print)" ] ||
    fail "build staging leaked in $case_name"
}

# New mandatory gates must fail before image creation, with no skip switch.
for failure in initialization audit_input audit_stage; do
  new_case "$failure"
  if run_release "$failure"; then fail "$failure was bypassed"; fi
  assert_clean
  if grep -q '^hdiutil-create$' "$MOCK_WORKSPACE/build/events"; then fail "$failure reached image creation"; fi
done

new_case success
run_release none || {
  sed -n '1,80p' "$MOCK_WORKSPACE/build/output.log" >&2
  fail 'successful release returned failure'
}
if ! grep -qx 'mock image' "$DIST_DMG"; then
  sed -n '1,120p' "$MOCK_WORKSPACE/build/output.log" >&2
  find "$MOCK_WORKSPACE" -maxdepth 4 -type f -print >&2
  fail 'dist output absent or invalid'
fi
cmp -s "$DIST_DMG" "$ARCHIVE_DMG" || fail 'desktop archive differs from dist'
python3 - "$MOCK_WORKSPACE/build/events" <<'ORDER'
import pathlib,sys
x=pathlib.Path(sys.argv[1]).read_text().splitlines()
assert x.count('initialization')==1 and x.count('bundle-audit')==2 and x.count('safe-copy')==1
assert x.index('initialization') < x.index('xcodebuild') < x.index('bundle-audit') < x.index('safe-copy')
assert x.index('safe-copy') < max(i for i,v in enumerate(x) if v=='bundle-audit') < x.index('hdiutil-create')
ORDER
[ -f "$MOCK_WORKSPACE/dist/release-manifest.json" ] || fail 'manifest missing'
cmp -s "$MOCK_WORKSPACE/dist/release-manifest.json" "$ARCHIVE_MANIFEST" ||
  fail 'versioned manifest archive differs from published manifest'
[ "$(stat -f '%d:%i' "$MOCK_WORKSPACE/dist/release-manifest.json")" != \
  "$(stat -f '%d:%i' "$ARCHIVE_MANIFEST")" ] ||
  fail 'versioned manifest archive aliases the mutable current manifest'
ruby -rjson -rdigest -e '
  manifest = JSON.parse(File.read(ARGV[0]))
  artifact = manifest.fetch("artifacts").fetch(0)
  abort "incorrect manifest" unless
    manifest.fetch("version") == ARGV[2] &&
    manifest.fetch("build") == ARGV[3] &&
    manifest.fetch("backend_version").match?(/\A[0-9]+\.[0-9]+\.[0-9]+\z/) &&
    manifest.fetch("source_digest") == File.read(ARGV[4]).strip &&
    manifest.fetch("source_digest").match?(/\A[0-9a-f]{64}\z/) &&
    manifest.fetch("acceptance") == {
      "scope" => "product-final",
      "product_acceptance_complete" => true,
      "device_acceptance" => "required-by-product-ledger",
      "evidence_sha256" => nil,
      "attested_app_tree_sha256" => nil,
      "attested_main_executable_sha256" => nil,
      "attested_backend_sha256" => nil,
      "verified_dmg_sha256" => nil,
    } &&
    artifact.fetch("name") == File.basename(ARGV[1]) &&
    artifact.fetch("architecture") == "arm64" &&
    artifact.fetch("minimum_macos") == "27.0" &&
    artifact.fetch("size") == File.size(ARGV[1]) &&
    artifact.fetch("sha256") == Digest::SHA256.file(ARGV[1]).hexdigest
' "$MOCK_WORKSPACE/dist/release-manifest.json" "$DIST_DMG" "$VERSION" "$BUILD_NUMBER" "$MOCK_WORKSPACE/build/backend.digest" ||
  fail 'manifest content mismatch'
[ -z "$(find "$MOCK_WORKSPACE/dist" -maxdepth 1 -name '.Switch-*-release.*' -print)" ] ||
  fail 'successful release left dist staging'
[ -z "$(find "$MOCK_WORKSPACE/dist/archive" -maxdepth 1 -name '.Switch-*.dmg.*' -print)" ] ||
  fail 'successful release left archive staging'

new_case software_success
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
run_release none || {
  sed -n '1,120p' "$MOCK_WORKSPACE/build/output.log" >&2
  fail 'software-scoped release returned failure'
}
cmp -s "$MOCK_WORKSPACE/dist/release-manifest.json" "$ARCHIVE_MANIFEST" ||
  fail 'software release manifest archive differs from published manifest'
if grep -qx 'xcodebuild' "$MOCK_WORKSPACE/build/events"; then
  fail 'software-scoped release rebuilt an unattested App'
fi
grep -F "ditto $MOCK_COLD_APP " "$MOCK_WORKSPACE/build/events" >/dev/null ||
  fail 'software-scoped release did not copy the attested cold App'
grep -F "verify-dmg-app " "$MOCK_WORKSPACE/build/events" >/dev/null ||
  fail 'software-scoped release did not verify the packaged DMG App payload'
ruby -rjson -rdigest -e '
  manifest = JSON.parse(File.read(ARGV[0]))
  acceptance = manifest.fetch("acceptance")
  abort "wrong software acceptance manifest" unless
    acceptance.fetch("scope") == "no-device-software-release" &&
    acceptance.fetch("product_acceptance_complete") == false &&
    acceptance.fetch("device_acceptance") == "UNVERIFIED_EXCLUDED_BY_USER" &&
    acceptance.fetch("evidence_sha256") == Digest::SHA256.file(ARGV[1]).hexdigest &&
    acceptance.fetch("attested_app_tree_sha256") == ARGV[2] &&
    acceptance.fetch("attested_main_executable_sha256") == Digest::SHA256.file(ARGV[3]).hexdigest &&
    acceptance.fetch("attested_backend_sha256") == Digest::SHA256.file(ARGV[4]).hexdigest &&
    acceptance.fetch("verified_dmg_sha256") == Digest::SHA256.file(ARGV[5]).hexdigest
' "$MOCK_WORKSPACE/dist/release-manifest.json" "$MOCK_SOFTWARE_EVIDENCE" \
  "$(MOCK_COLD_APP="$MOCK_COLD_APP" python3 "$MOCK_REPO/scripts/check-no-device-software-acceptance.py" verified-bindings "$MOCK_SOFTWARE_EVIDENCE" | awk '{print $1}')" \
  "$MOCK_COLD_APP/Contents/mock.txt" \
  "$MOCK_COLD_APP/Contents/Resources/backend/switchd-rs" \
  "$DIST_DMG" ||
  fail 'software acceptance manifest content mismatch'

new_case evidence_mutation
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
if run_release evidence_mutation; then
  fail 'mutated software evidence was accepted'
fi
assert_clean

new_case evidence_mutation_after_identity
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
if run_release evidence_mutation_after_identity; then
  fail 'software evidence changed during packaging was accepted'
fi
assert_clean

new_case evidence_mutation_during_manifest
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
if run_release evidence_mutation_during_manifest; then
  fail 'software evidence changed during manifest generation was accepted'
fi
assert_clean

new_case cold_app_tamper
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
if run_release cold_app_tamper; then
  fail 'tampered copy of the attested cold App was accepted'
fi
assert_clean

new_case bad_bindings
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
if run_release bad_bindings; then
  fail 'malformed attested App bindings were accepted'
fi
assert_clean

new_case dmg_payload_tamper
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
if run_release dmg_payload_tamper; then
  fail 'DMG payload verification failure was accepted'
fi
grep -F "verify-dmg-app " "$MOCK_WORKSPACE/build/events" >/dev/null ||
  fail 'DMG payload negative fixture did not reach mounted-payload verification'
assert_clean

new_case dmg_mutation_after_verification
MOCK_SOFTWARE_EVIDENCE="$MOCK_WORKSPACE/build/software-evidence.json"
printf 'valid software evidence\n' > "$MOCK_SOFTWARE_EVIDENCE"
if run_release dmg_mutation_after_verification; then
  fail 'DMG mutation after mounted-payload verification was accepted'
fi
assert_clean

new_case frozen_denominator_only
if run_release frozen_denominator_only; then
  fail 'frozen denominator without explicit product completion was accepted'
fi
assert_clean
[ ! -e "$MOCK_WORKSPACE/build/events" ] ||
  fail 'incomplete product acceptance reached build or publication'

new_case product_manifest_without_explicit_acceptance
MOCK_ARTIFACT="$MOCK_WORKSPACE/build/unpublished.dmg"
printf 'mock image\n' > "$MOCK_ARTIFACT"
if SWITCH_RELEASE_ACCEPTANCE_SCOPE=product-final \
   SWITCH_PRODUCT_ACCEPTANCE_COMPLETE=false \
   sh "$MOCK_REPO/scripts/generate-release-manifest.sh" \
      "$MOCK_WORKSPACE/build/product-manifest.json" "$MOCK_ARTIFACT" >/dev/null 2>&1; then
  fail 'product-final manifest was emitted without explicit product acceptance'
fi
[ ! -e "$MOCK_WORKSPACE/build/product-manifest.json" ] ||
  fail 'failed product-final manifest gate left a manifest'

new_case preflight
if run_release preflight; then
  fail 'failed source preflight was accepted'
fi
assert_clean
[ ! -e "$MOCK_WORKSPACE/build/events" ] || fail 'source preflight failure reached xcodebuild'

for scenario in create verify archive_copy manifest_archive_copy manifest_archive_publish manifest_publish app_identity source_mutation manifest_source_mutation; do
  new_case "$scenario"
  if [ "$scenario" = manifest_publish ]; then
    printf 'previous manifest\n' > "$MOCK_WORKSPACE/dist/release-manifest.json"
  fi
  if run_release "$scenario"; then
    sed -n '1,80p' "$MOCK_WORKSPACE/build/output.log" >&2
    sed -n '1,80p' "$MOCK_WORKSPACE/build/events" >&2
    fail "$scenario failure was accepted"
  fi
  assert_clean
  if [ "$scenario" = manifest_publish ]; then
    grep -qx 'previous manifest' "$MOCK_WORKSPACE/dist/release-manifest.json" ||
      fail 'manifest publish failure changed the prior manifest'
  fi
done

for conflict in dist_file archive_file archive_manifest_file dist_symlink archive_symlink archive_manifest_symlink; do
  new_case "$conflict"
  printf 'existing artifact\n' > "$MOCK_REPO/sentinel"
  case "$conflict" in
    dist_file) /bin/cp "$MOCK_REPO/sentinel" "$DIST_DMG" ;;
    archive_file) /bin/cp "$MOCK_REPO/sentinel" "$ARCHIVE_DMG" ;;
    archive_manifest_file) /bin/cp "$MOCK_REPO/sentinel" "$ARCHIVE_MANIFEST" ;;
    dist_symlink) ln -s "$MOCK_REPO/sentinel" "$DIST_DMG" ;;
    archive_symlink) ln -s "$MOCK_REPO/sentinel" "$ARCHIVE_DMG" ;;
    archive_manifest_symlink) ln -s "$MOCK_REPO/sentinel" "$ARCHIVE_MANIFEST" ;;
  esac
  if run_release none; then
    fail "$conflict was overwritten"
  fi
  [ ! -e "$MOCK_WORKSPACE/build/events" ] || fail "$conflict reached xcodebuild"
  grep -qx 'existing artifact' "$MOCK_REPO/sentinel" ||
    fail "$conflict changed its original target"
  case "$conflict" in
    dist_*) [ -e "$DIST_DMG" ] || fail "$conflict disappeared" ;;
    archive_manifest_*) [ -e "$ARCHIVE_MANIFEST" ] || fail "$conflict disappeared" ;;
    archive_*) [ -e "$ARCHIVE_DMG" ] || fail "$conflict disappeared" ;;
  esac
done

echo "release flow tests passed ($FLOW_CASES cases; mandatory initialization once, two bundle gates, cp-RX ordering, isolated fake app/text image only)"
