#!/bin/sh
set -eu
SWITCH_PHASE_A_SOURCE=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
exec /usr/bin/env PYTHONDONTWRITEBYTECODE=1 python3 "$SWITCH_PHASE_A_SOURCE/scripts/acceptance/test-companion-phase-a.py" "$@"
