#!/bin/bash
set -euo pipefail
export SWITCH_SOURCE_ROOT="$PWD"
export SWITCH_TEST_TMPDIR="$GITHUB_WORKSPACE/build/ci-swift-tmp/"
. scripts/workspace-env.sh
case "${1:-}" in
  models)
    python3 scripts/ci/run-report.py device-sms-send bash scripts/test-device-sms-send.sh
    python3 scripts/ci/run-report.py module-network-recovery bash scripts/test-module-network-recovery.sh
    python3 scripts/ci/run-report.py api-request-policy bash scripts/ci/swift-gates.sh model-regressions
    ;;
  handoff)
    cargo +1.98.0 fetch --locked --manifest-path rust/Cargo.toml
    # Online fetch is complete; offline native fixtures require real toolchain binaries.
    unset SWITCH_CARGO_BIN
    . scripts/offline-rust-toolchain.sh
    # These two fixtures share the fault-injection Rust static library build.
    python3 scripts/ci/run-report.py sms-forwarding-client bash scripts/ci/swift-gates.sh handoff-regressions
    python3 scripts/ci/run-report.py release-initialization bash scripts/test-release-initialization.sh
    ;;
  model-regressions)
    # The complete group must pass before its existing report becomes green.
    bash scripts/test-api-request-policy.sh
    bash scripts/test-unix-socket-transport.sh
    bash scripts/test-device-call-correctness.sh
    bash scripts/test-diagnostic-events.sh
    bash scripts/test-support-diagnostics.sh
    bash scripts/test-bounded-ble-diagnostic-log.sh
    bash scripts/test-reader-ble-selection.sh
    ;;
  handoff-regressions)
    # Includes the original Swift client and fault-injection Rust authority tests.
    bash scripts/test-forwarding-authority.sh
    bash scripts/test-forwarding-lock-interop.sh
    bash scripts/test-ble-manager.sh
    bash scripts/test-ble-phase-deadline.sh
    bash scripts/test-reader-ble-operation-proof.sh
    ;;
  *) echo 'usage: swift-gates.sh models|handoff|model-regressions|handoff-regressions' >&2; exit 2 ;;
esac
