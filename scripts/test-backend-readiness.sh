#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"

TEST_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TEST_TMP_ROOT=${SWITCH_TEST_TMPDIR:-$TMPDIR}
mkdir -p "$TEST_TMP_ROOT"
TEST_DIR=$(mktemp -d "${TEST_TMP_ROOT%/}/switch-readiness.XXXXXX")
# Keep compiler output in the shared isolated workspace. Runtime fixtures need
# their own short private root: Darwin sun_path is only 104 bytes, regardless
# of the length of SWITCH_TEST_TMPDIR supplied by the enclosing Swift gate.
FIXTURE_DIR=$(mktemp -d /tmp/swr.XXXXXX)
printf 'Backend readiness fixture directory: %s\n' "$FIXTURE_DIR"
# Only remove this test's generated executable; fixture logs stay available on failure.
trap 'rm -f "$TEST_DIR/backend-readiness"' EXIT INT TERM
xcrun swiftc -parse-as-library \
    "$TEST_ROOT/app/Sources/AcceptanceContext.swift" \
    "$TEST_ROOT/app/Sources/BackendProcess.swift" \
    "$TEST_ROOT/app/Sources/UnixSocketURLProtocol.swift" \
    "$TEST_ROOT/app/Sources/LocalBackendAuthentication.swift" \
    "$TEST_ROOT/app/Tests/BackendReadiness/main.swift" \
    -o "$TEST_DIR/backend-readiness"
"$TEST_DIR/backend-readiness" "$FIXTURE_DIR"
