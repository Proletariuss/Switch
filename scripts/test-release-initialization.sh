#!/bin/sh
# No real Security/Keychain imports, network, provider delivery or existing data.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
[ "$#" -eq 0 ] || { echo 'usage: test-release-initialization.sh' >&2; exit 2; }
"$SWITCH_SOURCE_ROOT/scripts/build-handoff-core.sh" --test-faults
INIT_TEST_DIR=$(mktemp -d "${TMPDIR}/switch-release-initialization.XXXXXX")
trap 'rm -rf "$INIT_TEST_DIR"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
umask 077
mkdir -p "$INIT_TEST_DIR/home"
SWITCH_RELEASE_INIT_TEST_ROOT="$INIT_TEST_DIR" python3 "$SWITCH_SOURCE_ROOT/scripts/test-release-initialization-bundle.py"
SWITCH_RELEASE_INIT_TEST_ROOT="$INIT_TEST_DIR" python3 "$SWITCH_SOURCE_ROOT/app/Tests/ReleaseInitialization/test_license_pin.py"
ruby "$SWITCH_SOURCE_ROOT/app/Tests/ReleaseInitialization/prepare.rb" "$SWITCH_SOURCE_ROOT" "$INIT_TEST_DIR"
xcrun swiftc -parse-as-library -D SWITCH_FORWARDING_TESTS \
  "$INIT_TEST_DIR/SMSForwardingStore.swift" "$INIT_TEST_DIR/SMSForwardingCheckpoint.swift" \
  "$INIT_TEST_DIR/MockBoundaries.swift" "$SWITCH_SOURCE_ROOT/app/Tests/SMSForwardingClient/UnixHandoff.swift" \
  "$SWITCH_SOURCE_ROOT/app/Tests/ReleaseInitialization/main.swift" \
  -Xlinker -force_load -Xlinker "$SWITCH_BUILD_DIR/handoff-test-target/release/libproletarius_handoff_core.a" \
  -Xlinker -framework -Xlinker SystemConfiguration -o "$INIT_TEST_DIR/tests"
env -i PATH="$PATH" SWITCH_BUILD_DIR="$SWITCH_BUILD_DIR" \
  CFFIXED_USER_HOME="$INIT_TEST_DIR/home" SWITCH_FORWARDING_CHECKPOINT_DIR="$INIT_TEST_DIR/fresh" \
  "$INIT_TEST_DIR/tests" fresh
env -i PATH="$PATH" SWITCH_BUILD_DIR="$SWITCH_BUILD_DIR" \
  CFFIXED_USER_HOME="$INIT_TEST_DIR/home" SWITCH_FORWARDING_CHECKPOINT_DIR="$INIT_TEST_DIR/upgrade" \
  "$INIT_TEST_DIR/tests" upgrade "$SWITCH_SOURCE_ROOT/rust/crates/proletarius-forwarding/tests/fixtures/go-state.swf1.hex"
env -i PATH="$PATH" SWITCH_BUILD_DIR="$SWITCH_BUILD_DIR" \
  CFFIXED_USER_HOME="$INIT_TEST_DIR/home" SWITCH_FORWARDING_CHECKPOINT_DIR="$INIT_TEST_DIR/upgrade" \
  "$INIT_TEST_DIR/tests" upgrade-reopen
