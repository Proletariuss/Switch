#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)

# Listing is the default. Removal requires explicit, validated cache paths.
exec ruby "$ROOT/scripts/clean-generated.rb" "$@"
