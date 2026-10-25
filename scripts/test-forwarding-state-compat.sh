#!/bin/sh
# Validate the committed Go-produced SWF1 golden and Rust forwarding behavior.
# The retired Go executor is not rebuilt; no providers or production state run.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
umask 077
if [ -n "${SWITCH_FORWARDING_CARGO_HOME:-}" ]; then
  CARGO_HOME=$SWITCH_FORWARDING_CARGO_HOME
  export CARGO_HOME
fi
cd "$SWITCH_SOURCE_ROOT"
"$SWITCH_CARGO_BIN" test --locked --offline --manifest-path rust/Cargo.toml -p proletarius-forwarding -- --nocapture
