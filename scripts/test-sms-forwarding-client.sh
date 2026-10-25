#!/bin/sh
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
    TMPDIR="$TMPDIR" SWITCH_BUILD_DIR="$SWITCH_BUILD_DIR" "$@"
}
"$SWITCH_SOURCE_ROOT/scripts/build-handoff-core.sh" --test-faults
HANDOFF_ARCHIVE="$SWITCH_BUILD_DIR/handoff-test-target/release/libproletarius_handoff_core.a"
TEST_DIR=$(mktemp -d "${TMPDIR}/switch-forwarding-client.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT INT TERM
ruby "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/prepare.rb" "$SWITCH_SOURCE_ROOT/app/Sources" "$TEST_DIR"
xcrun swiftc -parse-as-library -D SWITCH_FORWARDING_TESTS "$TEST_DIR/SMSForwardingStore.swift" "$TEST_DIR/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Sources/AcceptanceContext.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/UnixHandoff.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/main.swift" -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$TEST_DIR/tests"
run_offline_fixture "$TEST_DIR/tests"

xcrun swiftc -parse-as-library -D SWITCH_CHECKPOINT_TESTS \
  "$SWITCH_SOURCE_ROOT/app/Sources/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingCheckpoint/main.swift" -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$TEST_DIR/checkpoint-tests"
run_offline_fixture "$TEST_DIR/checkpoint-tests"

xcrun swiftc -parse-as-library -D SWITCH_CHECKPOINT_TESTS -D SWITCH_FORWARDING_TESTS \
  "$SWITCH_SOURCE_ROOT/app/Sources/AcceptanceContext.swift" \
  "$TEST_DIR/SMSForwardingStore.swift" "$TEST_DIR/SMSForwardingCheckpoint.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/UnixHandoff.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/main.swift" -Xlinker -force_load -Xlinker "$HANDOFF_ARCHIVE" -Xlinker -framework -Xlinker SystemConfiguration -o "$TEST_DIR/commit-failure-tests"
run_offline_fixture SWITCH_FORWARDING_HANDOFF_COMMIT_FAILURE_ONLY=1 "$TEST_DIR/commit-failure-tests"
