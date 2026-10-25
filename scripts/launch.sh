#!/bin/sh
set -eu
SWITCH_SOURCE_ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
. "$SWITCH_SOURCE_ROOT/scripts/workspace-env.sh"
APP="$SWITCH_DIST_DIR/Switch.app"
[ -d "$APP" ] || { echo '请先运行 scripts/build.sh。' >&2; exit 1; }
# open --env applies to this launch without changing the user's launchd environment.
open --env "SWITCH_DATA_DIR=$SWITCH_DATA_DIR" --env "CFFIXED_USER_HOME=$CFFIXED_USER_HOME" "$APP"
