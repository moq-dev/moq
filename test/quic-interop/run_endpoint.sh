#!/bin/bash
# The runner's entry point: route through the simulator, then run the endpoint,
# which reads ROLE, TESTCASE, REQUESTS, and SSLKEYLOGFILE itself.
set -euo pipefail

/setup.sh
if [[ "$ROLE" == client ]]; then
    # The simulator starts with the client; dialing before it forwards loses the handshake.
    /wait-for-it.sh sim:57832 -s -t 30
fi
# The runner keeps /logs beside a failed case's pcaps.
exec /usr/local/bin/quic-interop 2>/logs/log.txt
