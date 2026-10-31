#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
MODE=source
SOFTWARE_EVIDENCE=
case "$#:${1:-}" in
  0:) ;;
  1:--final-release) MODE=final ;;
  2:--software-release-evidence)
    MODE=software
    SOFTWARE_EVIDENCE=$2
    ;;
  *)
    echo "usage: $0 [--final-release | --software-release-evidence REPORT]" >&2
    exit 2
    ;;
esac
python3 scripts/check-version-contract.py
python3 scripts/check-rust-backend-exit.py --require-go-free
python3 scripts/check-rust-migration-contracts.py --require-rust-only
python3 scripts/verify-carrier-source-local.py
case "$MODE" in
  final)
    python3 scripts/check-acceptance-ledger.py --require-product-complete
    ;;
  software)
    python3 scripts/check-acceptance-ledger.py
    python3 scripts/check-no-device-software-acceptance.py verify "$SOFTWARE_EVIDENCE"
    ;;
esac
