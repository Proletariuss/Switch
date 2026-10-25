#!/bin/sh
# Source before workspace-env.sh: never execute a rustup proxy to find a toolchain.
# SWITCH_SOURCE_ROOT must already name the source checkout.
SWITCH_RUST_CHANNEL=$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' \
  "$SWITCH_SOURCE_ROOT/rust/rust-toolchain.toml")
case "$SWITCH_RUST_CHANNEL" in
  ''|*[!0-9.]*) echo "error: offline build requires an exact installed Rust channel" >&2; exit 1 ;;
esac
case "$(uname -s):$(uname -m)" in
  Darwin:arm64) SWITCH_RUST_HOST=aarch64-apple-darwin ;;
  *) echo "error: offline Rust core requires Apple Silicon macOS" >&2; exit 1 ;;
esac
export RUSTUP_HOME="${RUSTUP_HOME:-${HOME}/.rustup}"
SWITCH_RUST_BIN="$RUSTUP_HOME/toolchains/$SWITCH_RUST_CHANNEL-$SWITCH_RUST_HOST/bin"
export RUSTC="$SWITCH_RUST_BIN/rustc"
if [ "${SWITCH_CARGO_BIN+x}" != x ]; then
  SWITCH_CARGO_BIN="$SWITCH_RUST_BIN/cargo"
fi
for SWITCH_RUST_TOOL in "$SWITCH_CARGO_BIN" "$RUSTC"; do
  [ -n "$SWITCH_RUST_TOOL" ] && [ -x "$SWITCH_RUST_TOOL" ] || {
    echo "error: offline Rust tool is not installed: $SWITCH_RUST_TOOL" >&2
    exit 1
  }
  # rustup's cargo/rustc proxies are hard links or symlinks to rustup.
  for SWITCH_RUST_PROXY in "$(command -v rustup 2>/dev/null || true)" \
    "${SWITCH_RUST_TOOL%/*}/rustup"; do
    if [ -n "$SWITCH_RUST_PROXY" ] && [ "$SWITCH_RUST_TOOL" -ef "$SWITCH_RUST_PROXY" ]; then
      echo "error: offline Rust build refuses a rustup proxy: $SWITCH_RUST_TOOL" >&2
      exit 1
    fi
  done
done
export CARGO_NET_OFFLINE=true SWITCH_CARGO_BIN
case "$("$SWITCH_CARGO_BIN" --version)" in
  "cargo $SWITCH_RUST_CHANNEL "*) ;;
  *) echo "error: installed cargo does not match $SWITCH_RUST_CHANNEL" >&2; exit 1 ;;
esac
case "$("$RUSTC" --version)" in
  "rustc $SWITCH_RUST_CHANNEL "*) ;;
  *) echo "error: installed rustc does not match $SWITCH_RUST_CHANNEL" >&2; exit 1 ;;
esac
