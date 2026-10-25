#!/bin/sh
set -eu

PROJECT_DIR=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
SWITCH_SOURCE_ROOT="$PROJECT_DIR"
. "$PROJECT_DIR/scripts/offline-rust-toolchain.sh"
. "$PROJECT_DIR/scripts/workspace-env.sh"

[ "${ARCHS:-arm64}" = "arm64" ] || {
  echo "错误：Rust BLE 协议核心仅构建 Apple Silicon 版本" >&2
  exit 1
}

"$SWITCH_CARGO_BIN" build --locked --release --offline \
  --manifest-path "$PROJECT_DIR/rust/Cargo.toml" \
  -p proletarius-ble-core

ARCHIVE="$CARGO_TARGET_DIR/release/libproletarius_ble_core.a"
[ -s "$ARCHIVE" ] || {
  echo "错误：Rust BLE 静态库不存在或为空：$ARCHIVE" >&2
  exit 1
}
