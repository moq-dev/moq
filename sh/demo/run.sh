#!/usr/bin/env bash
# Run the web demo: a localhost relay, Big Buck Bunny, and the web player.
#
# Picks the first free port at/after 4443 (both UDP for QUIC and TCP for HTTP)
# so multiple worktrees can run `just dev` without colliding. The relay reads
# the port from MOQ_LISTEN/MOQ_WEB_HTTP_LISTEN, overriding localhost.toml.
#
# Usage: sh/demo/run.sh, from demo/ so `just` resolves the demo recipes.
set -euo pipefail

port=$("$(dirname "$0")/port.sh" 4443)
if [ "$port" != "4443" ]; then
    echo ">>> Port 4443 is busy, using $port instead" >&2
fi

base="http://localhost:$port"

# Scope the bind env vars to the relay only. The unified `moq` binary also
# reads MOQ_LISTEN, so exporting it globally would make the `pub` client
# try to start a server (and fail without a TLS cert).
bun run concurrently --kill-others --names rly,bbb,web --prefix-colors auto \
    "MOQ_LISTEN='[::]:$port' MOQ_WEB_HTTP_LISTEN='[::]:$port' just relay" \
    "just wait $base/certificate.sha256 && just pub bbb $base" \
    "just wait $base/certificate.sha256 && just web serve $base"
