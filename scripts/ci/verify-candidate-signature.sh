#!/bin/bash
set -euo pipefail
[[ "$#" == 1 ]]
codesign --verify --deep --strict "$1"
details="$(codesign -dv --verbose=4 "$1" 2>&1)"
printf '%s\n' "$details"
grep -Fxq 'Signature=adhoc' <<< "$details"
