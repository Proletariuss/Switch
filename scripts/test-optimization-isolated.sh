#!/bin/sh
# Deliberately avoids the full hardware-aware migration gate and real-network opt-ins.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
OPTIMIZATION_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
OPTIMIZATION_OUTPUT=${1:-"$SWITCH_BUILD_DIR/optimization/regression-$(date -u '+%Y%m%dT%H%M%SZ')"}
mkdir -p "$OPTIMIZATION_OUTPUT"
OPTIMIZATION_OUTPUT=$(CDPATH= cd -- "$OPTIMIZATION_OUTPUT" && pwd)
unset PROLETARIUS_TEST_REAL_PROXY PROLETARIUS_TEST_REAL_SURGE
if [ -z "${DEVELOPER_DIR:-}" ] && [ -d /Applications/Xcode.app/Contents/Developer ]; then
  export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
fi
export CC="$(xcrun --find clang)"
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
# Stress is opt-in; the default gate is one deterministic regression pass.
OPTIMIZATION_STRESS_COUNT=${OPTIMIZATION_STRESS_COUNT:-1}
case "$OPTIMIZATION_STRESS_COUNT" in
  ''|*[!0-9]*|0*) echo 'OPTIMIZATION_STRESS_COUNT must be a positive integer' >&2; exit 2 ;;
esac

export PYTHONPYCACHEPREFIX="$SWITCH_BUILD_DIR/python-cache"
python3 "$OPTIMIZATION_ROOT/scripts/build-evs.py"
echo '[rust] offline unit/local-loopback tests; no production service'
(cd "$OPTIMIZATION_ROOT/rust" && cargo +1.98.0 fmt --all -- --check &&
  cargo +1.98.0 test --locked --offline --workspace)
# Retain the opt-in repeat count for the current native IMS actor/session
# tests; the default full workspace pass already executes them once.
OPTIMIZATION_STRESS_PASS=1
while [ "$OPTIMIZATION_STRESS_PASS" -lt "$OPTIMIZATION_STRESS_COUNT" ]; do
  (cd "$OPTIMIZATION_ROOT/rust" && cargo +1.98.0 test --locked --offline -p proletariusd native_ims_)
  OPTIMIZATION_STRESS_PASS=$((OPTIMIZATION_STRESS_PASS + 1))
done

echo '[parity] reviewed frozen protocol fixtures and native examples'
python3 "$OPTIMIZATION_ROOT/scripts/test-frozen-protocol-fixtures.py"

echo '[swift] production response decoder and isolated backend startup'
sh "$OPTIMIZATION_ROOT/scripts/test-api-json-decoder.sh" "$OPTIMIZATION_OUTPUT/sms-decoder"
sh "$OPTIMIZATION_ROOT/scripts/test-backend-readiness.sh"
sh "$OPTIMIZATION_ROOT/scripts/test-ble-phase-deadline.sh"
sh "$OPTIMIZATION_ROOT/scripts/test-ble-manager.sh"
sh "$OPTIMIZATION_ROOT/scripts/test-sms-store.sh"
sh "$OPTIMIZATION_ROOT/scripts/test-api-request-policy.sh"
sh "$OPTIMIZATION_ROOT/scripts/test-reader-esim-identity.sh"
sh "$OPTIMIZATION_ROOT/scripts/test-vowifi-readiness.sh"
ruby "$OPTIMIZATION_ROOT/scripts/test-process-resources.rb"
ruby "$OPTIMIZATION_ROOT/scripts/test-summarize-process-resources.rb"
ruby "$OPTIMIZATION_ROOT/scripts/test-vowifi-log-summary.rb"
ruby "$OPTIMIZATION_ROOT/scripts/test-clean-generated.rb"
echo '[release] isolated text-image packaging and rollback tests'
sh "$OPTIMIZATION_ROOT/scripts/test-release-atomic.sh"
sh "$OPTIMIZATION_ROOT/scripts/test-release-flow.sh"
printf 'Isolated checks passed. SMS samples: %s/sms-decoder\n' "$OPTIMIZATION_OUTPUT"
