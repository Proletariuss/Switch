#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
OUTPUT=${1:-"$SWITCH_DIST_DIR/release-manifest.json"}
ARTIFACT_ARG=${2:-}
set -- $("$ROOT/scripts/version.sh")
VERSION=$1
BUILD_NUMBER=$2
BACKEND_VERSION=$(awk '
  /^\[workspace\.package\]$/ { in_workspace=1; next }
  /^\[/ { in_workspace=0 }
  in_workspace && /^version[[:space:]]*=/ {
    value=$0; sub(/^[^=]*=[[:space:]]*/, "", value); gsub(/[\"]/, "", value); print value; exit
  }
' "$ROOT/rust/Cargo.toml")
case "$BACKEND_VERSION" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "错误：Rust backend workspace version 无效。" >&2; exit 1 ;;
esac
SOURCE_DIGEST=$(python3 "$ROOT/scripts/source-identity.py")
if [ -n "${SWITCH_EXPECTED_SOURCE_DIGEST:-}" ] &&
   [ "$SOURCE_DIGEST" != "$SWITCH_EXPECTED_SOURCE_DIGEST" ]; then
  echo "错误：当前源码摘要与已验收 App 后台摘要不一致。" >&2
  exit 1
fi
MODERN="$SWITCH_DIST_DIR/Switch-$VERSION-macOS27.dmg"
ARTIFACT=${ARTIFACT_ARG:-$MODERN}
[ -f "$ARTIFACT" ] || { echo "错误：缺少发行文件 $ARTIFACT" >&2; exit 1; }
ACCEPTANCE_SCOPE=${SWITCH_RELEASE_ACCEPTANCE_SCOPE:-unscoped-build}
EVIDENCE_SHA256=${SWITCH_RELEASE_EVIDENCE_SHA256:-}
APP_TREE_SHA256=${SWITCH_RELEASE_APP_TREE_SHA256:-}
MAIN_EXECUTABLE_SHA256=${SWITCH_RELEASE_MAIN_EXECUTABLE_SHA256:-}
RELEASE_BACKEND_SHA256=${SWITCH_RELEASE_BACKEND_SHA256:-}
VERIFIED_DMG_SHA256=${SWITCH_RELEASE_VERIFIED_DMG_SHA256:-}
PRODUCT_ACCEPTANCE_COMPLETE=${SWITCH_PRODUCT_ACCEPTANCE_COMPLETE:-false}
case "$ACCEPTANCE_SCOPE" in
  unscoped-build)
    [ -z "$EVIDENCE_SHA256$APP_TREE_SHA256$MAIN_EXECUTABLE_SHA256$RELEASE_BACKEND_SHA256$VERIFIED_DMG_SHA256" ] || {
      echo "错误：未验收构建不应携带发行凭证或产物绑定。" >&2
      exit 1
    }
    [ "$PRODUCT_ACCEPTANCE_COMPLETE" = false ] || {
      echo "错误：未验收构建不得声明完整产品验收。" >&2
      exit 1
    }
    PRODUCT_ACCEPTANCE=false
    DEVICE_ACCEPTANCE=UNVERIFIED
    ;;
  product-final)
    [ -z "$EVIDENCE_SHA256$APP_TREE_SHA256$MAIN_EXECUTABLE_SHA256$RELEASE_BACKEND_SHA256$VERIFIED_DMG_SHA256" ] || {
      echo "错误：最终产品发行不应携带无设备软件凭证或产物绑定。" >&2
      exit 1
    }
    [ "$PRODUCT_ACCEPTANCE_COMPLETE" = true ] || {
      echo "错误：最终产品发行缺少明确的完整产品验收状态。" >&2
      exit 1
    }
    PRODUCT_ACCEPTANCE=true
    DEVICE_ACCEPTANCE=required-by-product-ledger
    ;;
  no-device-software-release)
    [ "$PRODUCT_ACCEPTANCE_COMPLETE" = false ] || {
      echo "错误：无设备软件发行不得声明完整产品验收。" >&2
      exit 1
    }
    echo "$EVIDENCE_SHA256" | grep -Eq '^[0-9a-f]{64}$' || {
      echo "错误：无设备软件发行缺少有效验收凭证摘要。" >&2
      exit 1
    }
    echo "$APP_TREE_SHA256" | grep -Eq '^[0-9a-f]{64}$' || {
      echo "错误：无设备软件发行缺少有效 App 树摘要。" >&2
      exit 1
    }
    echo "$MAIN_EXECUTABLE_SHA256" | grep -Eq '^[0-9a-f]{64}$' || {
      echo "错误：无设备软件发行缺少有效 App 主程序摘要。" >&2
      exit 1
    }
    echo "$RELEASE_BACKEND_SHA256" | grep -Eq '^[0-9a-f]{64}$' || {
      echo "错误：无设备软件发行缺少有效 Rust 后端摘要。" >&2
      exit 1
    }
    echo "$VERIFIED_DMG_SHA256" | grep -Eq '^[0-9a-f]{64}$' || {
      echo "错误：无设备软件发行缺少已核对 DMG 摘要。" >&2
      exit 1
    }
    [ "$VERIFIED_DMG_SHA256" = "$(shasum -a 256 "$ARTIFACT" | awk '{print $1}')" ] || {
      echo "错误：DMG 在只读挂载核对后发生变化。" >&2
      exit 1
    }
    PRODUCT_ACCEPTANCE=false
    DEVICE_ACCEPTANCE=UNVERIFIED_EXCLUDED_BY_USER
    ;;
  *) echo "错误：未知发行验收范围。" >&2; exit 1 ;;
esac

artifact_json() {
  file=$1
  minimum=$2
  name=$(basename "$MODERN")
  sha=$(shasum -a 256 "$file" | awk '{print $1}')
  size=$(stat -f '%z' "$file")
  printf '    {"name":"%s","architecture":"arm64","minimum_macos":"%s","size":%s,"sha256":"%s"}' \
    "$name" "$minimum" "$size" "$sha"
}

temporary=$(mktemp "${TMPDIR}/switch-release-manifest.XXXXXX")
trap 'rm -f "$temporary"' EXIT INT TERM
{
  printf '{\n'
  printf '  "schema_version":1,\n'
  printf '  "product":"Switch",\n'
  printf '  "version":"%s",\n' "$VERSION"
  printf '  "build":"%s",\n' "$BUILD_NUMBER"
  printf '  "backend_version":"%s",\n' "$BACKEND_VERSION"
  printf '  "source_digest":"%s",\n' "$SOURCE_DIGEST"
  printf '  "acceptance":{\n'
  printf '    "scope":"%s",\n' "$ACCEPTANCE_SCOPE"
  printf '    "product_acceptance_complete":%s,\n' "$PRODUCT_ACCEPTANCE"
  printf '    "device_acceptance":"%s",\n' "$DEVICE_ACCEPTANCE"
  if [ -n "$EVIDENCE_SHA256" ]; then
    printf '    "evidence_sha256":"%s",\n' "$EVIDENCE_SHA256"
    printf '    "attested_app_tree_sha256":"%s",\n' "$APP_TREE_SHA256"
    printf '    "attested_main_executable_sha256":"%s",\n' "$MAIN_EXECUTABLE_SHA256"
    printf '    "attested_backend_sha256":"%s",\n' "$RELEASE_BACKEND_SHA256"
    printf '    "verified_dmg_sha256":"%s"\n' "$VERIFIED_DMG_SHA256"
  else
    printf '    "evidence_sha256":null,\n'
    printf '    "attested_app_tree_sha256":null,\n'
    printf '    "attested_main_executable_sha256":null,\n'
    printf '    "attested_backend_sha256":null,\n'
    printf '    "verified_dmg_sha256":null\n'
  fi
  printf '  },\n'
  printf '  "generated_at":"%s",\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf '  "artifacts":[\n'
  artifact_json "$ARTIFACT" "27.0"
  printf '\n  ]\n}\n'
} > "$temporary"

mkdir -p "$(dirname "$OUTPUT")"
mv "$temporary" "$OUTPUT"
trap - EXIT INT TERM
echo "created: $OUTPUT"
