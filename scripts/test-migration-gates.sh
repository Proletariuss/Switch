#!/bin/sh
set -eu
RECOVERY_PROCESS_MODE=full
if [ "$#" -ne 0 ]; then
  if [ "$#" -eq 1 ] && [ "$1" = --smoke-process ]; then
    RECOVERY_PROCESS_MODE=smoke
  else
    echo "Usage: $0 [--smoke-process]" >&2
    exit 2
  fi
fi
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
ROOT="$SWITCH_SOURCE_ROOT"
export PYTHONPYCACHEPREFIX="$SWITCH_BUILD_DIR/python-cache"

case "${SWITCH_COLD_SOURCE_ONLY:-0}" in
  0) COLD_SOURCE_MODE=0 ;;
  1)
    # Only the audited cold source export may omit historical build/dist
    # receipts. A caller-provided environment flag is not sufficient.
    python3 "$ROOT/scripts/check-cold-source-mode.py" --source-root "$ROOT"
    COLD_SOURCE_MODE=1
    ;;
  *) echo "SWITCH_COLD_SOURCE_ONLY must be 0 or 1" >&2; exit 2 ;;
esac

echo "[checks] Native source/route gates and negative checker fixtures"
python3 "$ROOT/scripts/check-no-device-software-acceptance.py" self-test
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_rust_backend_exit.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_capture_optimization_baseline.py'
python3 "$ROOT/scripts/audit-clean-build.py" self-test
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_rust_migration_contracts.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_acceptance_ledger.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_current_baseline_decision.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_p0_1_current_baseline_candidate.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_p0_1_current_baseline_activation.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_current_baseline_successor.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_cold_source_mode.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_p6_data_roundtrip.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_nonblocking_diagnostics.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_clean_build_dependencies.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_recovery_process_gates.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_reader_vowifi_probe.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_audit_carrier_freshness.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_verify_carrier_source_local.py'
python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_app_identity.py'
sh "$ROOT/scripts/test-release-flow.sh"
python3 "$ROOT/scripts/check-rust-backend-exit.py" --require-go-free
python3 "$ROOT/scripts/check-rust-migration-contracts.py" --require-rust-only
python3 "$ROOT/scripts/check-acceptance-ledger.py"
python3 "$ROOT/scripts/check-current-baseline-decision.py"
python3 "$ROOT/scripts/check-p0-1-current-baseline-candidate.py"
python3 "$ROOT/scripts/check-p0-1-current-baseline-activation.py"
if [ "$COLD_SOURCE_MODE" = 1 ]; then
  # The isolated source export intentionally contains no i677 build/dist
  # receipts. This checks only their frozen source-side scope and pins.
  python3 "$ROOT/scripts/check-current-baseline-successor.py" --source-only
else
  python3 "$ROOT/scripts/check-current-baseline-successor.py"
fi

echo "[native] Build EVS archive before codec and session tests"
python3 "$ROOT/scripts/build-evs.py"
echo "[native] Build the ARM module supervisor before compiling the daemon tests"
python3 "$ROOT/scripts/build-module-supervisor.py"

echo "[rust] Format and workspace tests, including frozen protocol projections"
(cd "$ROOT/rust" && cargo +1.98.0 fmt --all -- --check && cargo +1.98.0 test --locked --workspace)

echo "[rust] Owned crash/restart and finite I/O matrices in hardware/network-denied sandboxes"
if [ "$RECOVERY_PROCESS_MODE" = full ]; then
  python3 "$ROOT/scripts/check-recovery-process-gates.py"
else
  python3 "$ROOT/scripts/check-recovery-process-gates.py" --smoke
fi

echo "[swift] BLE physical completion and manager regressions"
"$ROOT/scripts/test-reader-ble-operation-proof.sh"
"$ROOT/scripts/test-ble-manager.sh"
"$ROOT/scripts/test-bounded-ble-diagnostic-log.sh"
"$ROOT/scripts/test-ble-gatt-selection.sh"
"$ROOT/scripts/test-forwarding-authority.sh"
"$ROOT/scripts/test-sms-forwarding-client.sh"
sh "$ROOT/scripts/test-forwarding-handoff-compat.sh"

echo "[swift] Module recovery isolation presentation"
"$ROOT/scripts/test-module-recovery-status.sh"
"$ROOT/scripts/test-reader-esim-identity.sh"
"$ROOT/scripts/test-api-request-policy.sh"
"$ROOT/scripts/test-unix-socket-transport.sh"

echo "[swift] Rust-owned mutation, recovery and durable UI intent boundaries"
"$ROOT/scripts/test-call-mode-mutation-fence.sh"
sh "$ROOT/scripts/test-device-call-correctness.sh"
sh "$ROOT/scripts/test-module-network-recovery.sh"
"$ROOT/scripts/test-reader-vowifi-input-boundary.sh"
"$ROOT/scripts/test-multistandby-store.sh"

echo "[swift] Backend, BLE deadline and message state-machine regressions"
"$ROOT/scripts/test-backend-readiness.sh"
"$ROOT/scripts/test-ble-phase-deadline.sh"
"$ROOT/scripts/test-reader-ble-selection.sh"
"$ROOT/scripts/test-device-sms-send.sh"
"$ROOT/scripts/test-sms-store.sh"
sh "$ROOT/scripts/test-sms-store-health.sh"
"$ROOT/scripts/test-sms-archive-export.sh"
"$ROOT/scripts/test-sms-attention.sh"
"$ROOT/scripts/test-at-console-policy.sh"
"$ROOT/scripts/test-api-json-decoder.sh"
"$ROOT/scripts/test-diagnostic-events.sh"
"$ROOT/scripts/test-udp-proxy-diagnostic.sh"

echo "[swift] Rust reader auto-selection proposals and stale UI fences"
sh "$ROOT/scripts/test-vowifi-readiness.sh"

echo "[swift] Audio playback and call bridge regressions"
"$ROOT/scripts/test-audio-playback-policy.sh"
"$ROOT/scripts/test-module-audio-pairing.sh"
"$ROOT/scripts/test-reader-audio-recovery.sh"
"$ROOT/scripts/test-call-audio-store.sh"

echo "[parity] Frozen fixture integrity negative tests"
PYTHONDONTWRITEBYTECODE=1 python3 "$ROOT/scripts/tests/test_frozen_protocol_fixtures.py"

echo "[parity] Reviewed frozen protocol fixtures and native examples"
python3 "$ROOT/scripts/test-frozen-protocol-fixtures.py"

if [ "$RECOVERY_PROCESS_MODE" = full ]; then
  echo "migration gates passed"
else
  echo "migration gate smoke checks completed; full software acceptance not awarded"
fi
