#!/bin/sh
# Real production URLProtocol against private local fixtures. No installed app,
# running backend, hardware, card commands, user data or network connections.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
TRANSPORT_TEST_TMP=$(mktemp -d "${TMPDIR}/switch-unix-transport.XXXXXX")
trap 'rm -rf "$TRANSPORT_TEST_TMP"' EXIT INT TERM
compile_and_run_transport() {
  TRANSPORT_TEST_MODE=$1
  shift
  xcrun swiftc -parse-as-library "$@" \
    "$SWITCH_SOURCE_ROOT/app/Sources/UnixSocketURLProtocol.swift" \
    "$SWITCH_SOURCE_ROOT/app/Tests/UnixSocketTransport/main.swift" \
    -o "$TRANSPORT_TEST_TMP/unix-transport-tests-$TRANSPORT_TEST_MODE"
  "$TRANSPORT_TEST_TMP/unix-transport-tests-$TRANSPORT_TEST_MODE"
}
# No arguments keeps all original cases and now validates both compilation
# boundaries. The normal binary contains no test-hook fields or override path.
case "${1:-all}" in
  normal) compile_and_run_transport normal ;;
  hooks) compile_and_run_transport hooks -D UNIX_SOCKET_TRANSPORT_TEST_HOOKS ;;
  all)
    compile_and_run_transport normal
    compile_and_run_transport hooks -D UNIX_SOCKET_TRANSPORT_TEST_HOOKS
    ;;
  *) echo "usage: $0 [normal|hooks|all]" >&2; exit 2 ;;
esac
