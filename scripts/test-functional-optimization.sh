#!/bin/sh
# The current functional gate uses only fixtures and disposable local state.
# No production app, card, carrier, external notification service or DMG runs.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
if [ -n "${SWITCH_FORWARDING_CARGO_HOME:-}" ]; then
  CARGO_HOME=$SWITCH_FORWARDING_CARGO_HOME
  export CARGO_HOME
fi
if [ -n "${SWITCH_FORWARDING_TEST_TMPDIR:-}" ]; then
  mkdir -p "$SWITCH_FORWARDING_TEST_TMPDIR"
  SWITCH_FORWARDING_TEST_TMPDIR_REAL=$(CDPATH= cd -- "$SWITCH_FORWARDING_TEST_TMPDIR" && pwd -P)
  case "$SWITCH_FORWARDING_TEST_TMPDIR_REAL" in
    "$SWITCH_BUILD_DIR"/*) ;;
    *) echo 'SWITCH_FORWARDING_TEST_TMPDIR must resolve inside this worktree build directory.' >&2; exit 2 ;;
  esac
  TMPDIR=$SWITCH_FORWARDING_TEST_TMPDIR
  SWITCH_TEST_TMPDIR=$SWITCH_FORWARDING_TEST_TMPDIR
  export TMPDIR SWITCH_TEST_TMPDIR
fi
unset PROLETARIUS_TEST_REAL_PROXY PROLETARIUS_TEST_REAL_SURGE SWITCH_INTEGRATION_FIXTURE
cd "$SWITCH_SOURCE_ROOT"
export PYTHONPYCACHEPREFIX="$SWITCH_BUILD_DIR/python-cache"
python3 scripts/build-evs.py
# The native workspace includes the former selected storage/forwarding suites,
# retained native IMS/module/reader lifecycle tests, and cross-process locking.
"$SWITCH_CARGO_BIN" test --locked --offline --manifest-path rust/Cargo.toml --workspace
python3 scripts/test-frozen-protocol-fixtures.py
for check in device-sms-send sms-archive-export at-console-policy sms-store sms-attention sms-forwarding-client multistandby-store \
  device-call-correctness backend-readiness ble-manager menu-bar-presentation \
  reader-presentation api-request-policy udp-proxy-diagnostic diagnostic-events sms-store-health; do
  sh "scripts/test-$check.sh"
done
python3 scripts/test-multistandby-integration.py
