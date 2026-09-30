#!/usr/bin/env bash
# Print the first port at/after START that is free on both TCP and UDP.
#
# `lsof` isn't available everywhere (e.g. Git Bash on Windows); without it we
# can't probe, so print START and let the relay surface a bind error if it's
# actually taken.
#
# Usage: sh/demo/port.sh START
set -euo pipefail

port=${1:?usage: sh/demo/port.sh START}
if command -v lsof >/dev/null 2>&1; then
    while lsof -nP -iTCP:"$port" -sTCP:LISTEN -t >/dev/null 2>&1 ||
        lsof -nP -iUDP:"$port" -t >/dev/null 2>&1; do
        port=$((port + 1))
    done
fi
echo "$port"
