#!/bin/bash
set -euo pipefail
export SWITCH_SOURCE_ROOT="$PWD"
export SWITCH_TEST_TMPDIR="$RUNNER_TEMP/switch-swift-tests/"
. scripts/workspace-env.sh
case "${1:-}" in
  models)
    python3 scripts/ci/run-report.py device-sms-send bash scripts/test-device-sms-send.sh
    python3 scripts/ci/run-report.py module-network-recovery bash scripts/test-module-network-recovery.sh
    python3 scripts/ci/run-report.py api-request-policy bash scripts/test-api-request-policy.sh
    ;;
  handoff)
    cargo +1.98.0 fetch --locked --manifest-path rust/Cargo.toml
    # These two fixtures share the fault-injection Rust static library build.
    python3 scripts/ci/run-report.py sms-forwarding-client bash scripts/test-sms-forwarding-client.sh
    python3 scripts/ci/run-report.py release-initialization bash scripts/test-release-initialization.sh
    ;;
  *) echo 'usage: swift-gates.sh models|handoff' >&2; exit 2 ;;
esac
