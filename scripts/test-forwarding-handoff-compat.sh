#!/bin/sh
# Independent production checkpoint writer/restart -> exact Rust DTO -> Swift verifier.
# This runner is offline only. Explicit live daemon durability uses
# check-forwarding-handoff-durability.py with its owned daemon and mandatory OS sandbox.
set -eu
if [ "${SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET+x}" = x ] || \
   [ "${SWITCH_FORWARDING_HANDOFF_LIVE_TOKEN+x}" = x ]; then
  echo "error: offline forwarding runner refuses inherited LIVE_SOCKET/LIVE_TOKEN (including empty values)" >&2
  exit 1
fi
unset SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET SWITCH_FORWARDING_HANDOFF_LIVE_TOKEN
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
run_offline_fixture() {
  env -i PATH="$PATH" HOME="$CFFIXED_USER_HOME" CFFIXED_USER_HOME="$CFFIXED_USER_HOME" \
    TMPDIR="$TMPDIR" SWITCH_BUILD_DIR="$SWITCH_BUILD_DIR" \
    SWITCH_FORWARDING_CHECKPOINT_DIR="$SWITCH_FORWARDING_CHECKPOINT_DIR" \
    SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH" \
    SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH="$SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH" "$@"
}
umask 077
SWITCH_HANDOFF_FIXTURE_DIR=$(mktemp -d "$SWITCH_BUILD_DIR/tmp/forwarding-handoff-compat.XXXXXX")
SWITCH_HANDOFF_TEST_DIR=$(mktemp -d "$SWITCH_BUILD_DIR/tmp/forwarding-handoff-build.XXXXXX")
trap 'rm -rf "$SWITCH_HANDOFF_TEST_DIR"' EXIT INT TERM
export SWITCH_BUILD_DIR
export SWITCH_FORWARDING_CHECKPOINT_DIR="$SWITCH_HANDOFF_FIXTURE_DIR/checkpoint"
mkdir -m 700 "$SWITCH_FORWARDING_CHECKPOINT_DIR"
export SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_HANDOFF_FIXTURE_DIR/swift-producer.json"
export SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH="$SWITCH_HANDOFF_FIXTURE_DIR/rust-handoff.json"
cd "$SWITCH_SOURCE_ROOT"
"$SWITCH_SOURCE_ROOT/scripts/build-handoff-core.sh" --test-faults
HANDOFF_ARCHIVE="$SWITCH_BUILD_DIR/handoff-test-target/release/libproletarius_handoff_core.a"

ruby "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/prepare.rb" \
  "$SWITCH_SOURCE_ROOT/app/Sources" "$SWITCH_HANDOFF_TEST_DIR"
xcrun swiftc -parse-as-library -D SWITCH_FORWARDING_TESTS \
  "$SWITCH_SOURCE_ROOT/app/Sources/AcceptanceContext.swift" \
  "$SWITCH_HANDOFF_TEST_DIR/SMSForwardingStore.swift" \
  "$SWITCH_HANDOFF_TEST_DIR/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/UnixHandoff.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/main.swift" \
  -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$SWITCH_HANDOFF_TEST_DIR/tests"
run_offline_fixture SWITCH_FORWARDING_HANDOFF_MODE=produce "$SWITCH_HANDOFF_TEST_DIR/tests"
export SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_HANDOFF_FIXTURE_DIR/swift-restarted.json"
run_offline_fixture SWITCH_FORWARDING_HANDOFF_MODE=restart "$SWITCH_HANDOFF_TEST_DIR/tests"

SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH" \
SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH="$SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH" \
  "$SWITCH_CARGO_BIN" test --locked --offline --manifest-path rust/Cargo.toml \
  -p proletarius-api-contract -- --exact tests::swift_forwarding_handoff_fixture_round_trips --ignored --nocapture

run_offline_fixture SWITCH_FORWARDING_HANDOFF_MODE=verify \
SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_HANDOFF_FIXTURE_DIR/swift-verified.json" \
SWITCH_FORWARDING_HANDOFF_VERIFY_PATH="$SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH" \
  "$SWITCH_HANDOFF_TEST_DIR/tests"
echo "Synthetic handoff exchange artifacts: $SWITCH_HANDOFF_FIXTURE_DIR"
