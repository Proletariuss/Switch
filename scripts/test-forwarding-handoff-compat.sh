#!/bin/sh
# Independent production checkpoint writer/restart -> exact Rust DTO -> Swift verifier.
# Runtime isolation is supplied by the caller; full daemon durability uses
# check-forwarding-handoff-durability.py with the mandatory OS sandbox.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
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
  "$SWITCH_HANDOFF_TEST_DIR/SMSForwardingStore.swift" \
  "$SWITCH_HANDOFF_TEST_DIR/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/UnixHandoff.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/main.swift" \
  -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$SWITCH_HANDOFF_TEST_DIR/tests"
SWITCH_FORWARDING_HANDOFF_MODE=produce "$SWITCH_HANDOFF_TEST_DIR/tests"
export SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_HANDOFF_FIXTURE_DIR/swift-restarted.json"
SWITCH_FORWARDING_HANDOFF_MODE=restart "$SWITCH_HANDOFF_TEST_DIR/tests"

SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH" \
SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH="$SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH" \
  "$SWITCH_CARGO_BIN" test --locked --offline --manifest-path rust/Cargo.toml \
  -p proletarius-api-contract -- --exact tests::swift_forwarding_handoff_fixture_round_trips --ignored --nocapture

SWITCH_FORWARDING_HANDOFF_MODE=verify \
SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH="$SWITCH_HANDOFF_FIXTURE_DIR/swift-verified.json" \
SWITCH_FORWARDING_HANDOFF_VERIFY_PATH="$SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH" \
  "$SWITCH_HANDOFF_TEST_DIR/tests"
echo "Synthetic handoff exchange artifacts: $SWITCH_HANDOFF_FIXTURE_DIR"
