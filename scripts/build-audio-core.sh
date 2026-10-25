#!/bin/sh
set -eu

PROJECT_DIR=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
SWITCH_SOURCE_ROOT="$PROJECT_DIR"
. "$PROJECT_DIR/scripts/workspace-env.sh"

[ "${ARCHS:-arm64}" = "arm64" ] || {
  echo "错误：Rust 实时音频核心仅构建 Apple Silicon 版本" >&2
  exit 1
}

CARGO_BIN="${SWITCH_CARGO_BIN:-$(command -v cargo)}"
[ -x "$CARGO_BIN" ] || {
  echo "错误：未找到 Rust cargo (${CARGO_BIN})" >&2
  exit 1
}

"$CARGO_BIN" build --locked --release \
  --manifest-path "$PROJECT_DIR/rust/Cargo.toml" \
  -p proletarius-audio-core

ARCHIVE="$CARGO_TARGET_DIR/release/libproletarius_audio_core.a"
[ -s "$ARCHIVE" ] || {
  echo "错误：Rust 音频静态库不存在或为空：$ARCHIVE" >&2
  exit 1
}
