#!/bin/sh
# Source this file from a script after setting SWITCH_SOURCE_ROOT.
# Keep generated state beside src; do not reuse the original application's data.
: "${SWITCH_SOURCE_ROOT:=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}"
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$SWITCH_SOURCE_ROOT" && pwd -P)
SWITCH_WORKSPACE=$(CDPATH= cd -- "$SWITCH_SOURCE_ROOT/.." && pwd -P)
export SWITCH_SOURCE_ROOT SWITCH_WORKSPACE
export SWITCH_BUILD_DIR="$SWITCH_WORKSPACE/build"
export SWITCH_DIST_DIR="$SWITCH_WORKSPACE/dist"
export SWITCH_DATA_DIR="$SWITCH_BUILD_DIR/runtime"
# Installed toolchains are read from their existing location. Set this before
# isolating Foundation's home directory, which Rust also uses on macOS.
export RUSTUP_HOME="${RUSTUP_HOME:-${HOME}/.rustup}"
export RUSTUP_TOOLCHAIN=1.98.0
export CFFIXED_USER_HOME="$SWITCH_BUILD_DIR/test-home"
# The isolated clean-build auditor supplies a short private temp root so
# Darwin AF_UNIX fixtures stay below sun_path's 104-byte limit. Normal local
# builds retain the workspace-local temp directory.
export TMPDIR="${SWITCH_TEST_TMPDIR:-$SWITCH_BUILD_DIR/tmp/}"
export CARGO_TARGET_DIR="$SWITCH_BUILD_DIR/rust-target"
export CLANG_MODULE_CACHE_PATH="$SWITCH_BUILD_DIR/swift-cache"
export SWIFT_MODULECACHE_PATH="$SWITCH_BUILD_DIR/swift-cache"
export XDG_CACHE_HOME="$SWITCH_BUILD_DIR/cache"
# Locate the installed compiler before isolating its writable package cache.
if [ -z "${SWITCH_CARGO_BIN:-}" ]; then
  SWITCH_CARGO_BIN=$(command -v cargo 2>/dev/null || true)
fi
export SWITCH_CARGO_BIN
export CARGO_HOME="$SWITCH_BUILD_DIR/cargo-home"
mkdir -p "$SWITCH_DIST_DIR" "$SWITCH_DATA_DIR" "$CFFIXED_USER_HOME" "$TMPDIR" \
  "$CARGO_TARGET_DIR" "$CLANG_MODULE_CACHE_PATH" \
  "$XDG_CACHE_HOME" "$CARGO_HOME"
if [ -z "${DEVELOPER_DIR:-}" ] && [ -d /Applications/Xcode.app/Contents/Developer ]; then
  export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
fi
