#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
"$SWITCH_SOURCE_ROOT/scripts/build-handoff-core.sh" --test-faults
HANDOFF_ARCHIVE="$SWITCH_BUILD_DIR/handoff-test-target/release/libproletarius_handoff_core.a"
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-forwarding-client.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
ruby "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/prepare.rb" "$SWITCH_SOURCE_ROOT/app/Sources" "$TEST_DIR"
xcrun swiftc -parse-as-library -D SWITCH_FORWARDING_TESTS "$TEST_DIR/SMSForwardingStore.swift" "$TEST_DIR/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/UnixHandoff.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/main.swift" -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$TEST_DIR/tests"
"$TEST_DIR/tests"

xcrun swiftc -parse-as-library -D SWITCH_CHECKPOINT_TESTS \
  "$SWITCH_SOURCE_ROOT/app/Sources/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingCheckpoint/main.swift" -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$TEST_DIR/checkpoint-tests"
"$TEST_DIR/checkpoint-tests"

xcrun swiftc -parse-as-library -D SWITCH_CHECKPOINT_TESTS -D SWITCH_FORWARDING_TESTS \
  "$TEST_DIR/SMSForwardingStore.swift" "$TEST_DIR/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/UnixHandoff.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/main.swift" -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$TEST_DIR/commit-failure-tests"
SWITCH_FORWARDING_HANDOFF_COMMIT_FAILURE_ONLY=1 "$TEST_DIR/commit-failure-tests"
