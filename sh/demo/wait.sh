#!/usr/bin/env bash
# Wait up to ten minutes for URL to respond, e.g. a relay's certificate
# fingerprint once it is listening.
#
# Usage: sh/demo/wait.sh URL
set -euo pipefail

url=${1:?usage: sh/demo/wait.sh URL}
for _ in $(seq 1 600); do
    curl -sf "$url" >/dev/null 2>&1 && exit 0
    sleep 1
done
echo "timed out waiting for $url" >&2
exit 1
