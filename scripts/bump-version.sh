#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
CURRENT=$(tr -d '[:space:]' < "$ROOT/VERSION")
BUILD=$(tr -d '[:space:]' < "$ROOT/BUILD_NUMBER")
NEXT=${1:-}

if [ -z "$NEXT" ]; then
  MAJOR=${CURRENT%%.*}
  MINOR=$((1${CURRENT#*.} - 100 + 1))
  if [ "$MINOR" -ge 100 ]; then
    MAJOR=$((MAJOR + 1))
    MINOR=0
  fi
  NEXT=$(printf '%s.%02d' "$MAJOR" "$MINOR")
fi
case "$NEXT" in
  *[!0-9.]*|*.*.*) echo "版本必须为整数主版本和两位次版本" >&2; exit 1 ;;
  [0-9]*.[0-9][0-9]) ;;
  *) echo "用法：$0 [例如 0.03]" >&2; exit 1 ;;
esac
case "${NEXT%%.*}" in
  0|[1-9]|[1-9][0-9]*) ;;
  *) echo "主版本不能有前导零" >&2; exit 1 ;;
esac
RUST_VERSION="${NEXT%%.*}.$((1${NEXT#*.} - 100)).0"

printf '%s\n' "$NEXT" > "$ROOT/VERSION"
printf '%s\n' "$((BUILD + 1))" > "$ROOT/BUILD_NUMBER"

perl -0pi -e 's/MARKETING_VERSION: "[^"]+"/MARKETING_VERSION: "'"$NEXT"'"/g; s/CURRENT_PROJECT_VERSION: "[^"]+"/CURRENT_PROJECT_VERSION: "'"$((BUILD + 1))"'"/g' "$ROOT/project.yml"
perl -0pi -e 's/MARKETING_VERSION = [^;]+;/MARKETING_VERSION = '"$NEXT"';/g; s/CURRENT_PROJECT_VERSION = [^;]+;/CURRENT_PROJECT_VERSION = '"$((BUILD + 1))"';/g' "$ROOT/Switch.xcodeproj/project.pbxproj"
perl -0pi -e 's/^version = "[^"]+"$/version = "'"$RUST_VERSION"'"/m' "$ROOT/rust/Cargo.toml"
cargo +1.98.0 generate-lockfile --manifest-path "$ROOT/rust/Cargo.toml" --offline

echo "版本已更新：${CURRENT} → ${NEXT}（构建 $((BUILD + 1))）"
