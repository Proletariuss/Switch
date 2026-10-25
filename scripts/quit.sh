#!/bin/sh
set -eu
# A normal quit targets only Switch and lets its owned backends shut down.
osascript -e 'if application id "com.proletarius.switch" is running then tell application id "com.proletarius.switch" to quit'
