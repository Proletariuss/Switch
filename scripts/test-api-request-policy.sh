#!/bin/sh
# Exercise the production API client with an in-memory URLProtocol, then the
# real Unix bridge against a private temporary mock socket. Never connects to
# the running backend, sends a card command, or accesses user credentials.
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
API_POLICY_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
API_POLICY_TMP=$(mktemp -d "${TMPDIR}/switch-api-policy.XXXXXX")
trap 'rm -rf "$API_POLICY_TMP"' EXIT INT TERM
if grep -Eq 'mutationTimeout|longWrite|isLongWrite' "$API_POLICY_ROOT/app/Sources/APIClient.swift"; then
  echo "FAIL: Swift API client still classifies mutation safety by route" >&2
  exit 1
fi
xcrun swiftc -parse-as-library \
  "$API_POLICY_ROOT/app/Sources/APIClient.swift" \
  "$API_POLICY_ROOT/app/Sources/CallModels.swift" \
  "$API_POLICY_ROOT/app/Sources/CallRuntimeStore.swift" \
  "$API_POLICY_ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$API_POLICY_ROOT/app/Tests/APIRequestPolicy/main.swift" \
  -o "$API_POLICY_TMP/api-policy-tests"
"$API_POLICY_TMP/api-policy-tests"
xcrun swiftc -parse-as-library \
  "$API_POLICY_ROOT/app/Sources/APIClient.swift" \
  "$API_POLICY_ROOT/app/Sources/ESIMRefreshModels.swift" \
  "$API_POLICY_ROOT/app/Sources/UnixSocketURLProtocol.swift" \
  "$API_POLICY_ROOT/app/Tests/APIRequestPolicy/UnixSocket.swift" \
  -o "$API_POLICY_TMP/api-unix-tests"
"$API_POLICY_TMP/api-unix-tests"
