#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
"$SWITCH_CARGO_BIN" test --locked --offline --manifest-path "$SWITCH_SOURCE_ROOT/rust/Cargo.toml" \
  -p proletarius-handoff-core --features fault-injection --target-dir "$SWITCH_BUILD_DIR/handoff-test-target"
"$SWITCH_SOURCE_ROOT/scripts/test-sms-forwarding-client.sh"
