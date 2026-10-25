#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
if [ "${1:-}" = --test-faults ]; then
  "$SWITCH_CARGO_BIN" build --locked --release --offline --manifest-path "$SWITCH_SOURCE_ROOT/rust/Cargo.toml" \
    -p proletarius-handoff-core --features fault-injection --target-dir "$SWITCH_BUILD_DIR/handoff-test-target"
else
  "$SWITCH_CARGO_BIN" build --locked --release --offline --manifest-path "$SWITCH_SOURCE_ROOT/rust/Cargo.toml" -p proletarius-handoff-core
fi
