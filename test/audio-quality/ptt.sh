#!/usr/bin/env bash
# Measure push-to-talk on an ideal local path: no shaping, loss, or queueing.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source test/lib/harness.sh
harness_begin push-to-talk "just test push-to-talk$(harness_argv "$@")"
# The run directory is this measurement's evidence, so keep it even when it passes.
export HARNESS_KEEP="${HARNESS_KEEP:-1}"

[[ $# -eq 0 ]] || {
    echo 'usage: just test push-to-talk' >&2
    exit 2
}

bun install --frozen-lockfile
cargo build --locked -p moq-relay
TARGET_BASE="${CARGO_TARGET_DIR:-$ROOT/target}"
harness_port relay
relay_port=$HARNESS_PORT
sed "s/4443/$relay_port/g" test/audio-quality/relay.toml >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" "$TARGET_BASE/debug/moq-relay" "$HARNESS_RUN/relay.toml"
harness_ready "http://127.0.0.1:$relay_port/certificate.sha256" 30 "$HARNESS_PID"
# A different listener can answer HTTP before our process reports a bind collision.
if ! lsof -nP -a -p "$HARNESS_PID" -iTCP:"$relay_port" -sTCP:LISTEN >/dev/null 2>&1 ||
    ! lsof -nP -a -p "$HARNESS_PID" -iUDP:"$relay_port" >/dev/null 2>&1; then
    cat "$HARNESS_RUN/relay.log" >&2
    echo 'relay does not own both listeners' >&2
    exit 1
fi
harness_spawn driver "$HARNESS_RUN/driver.log" bun test/audio-quality/clients/js/ptt.ts \
    --relay "http://127.0.0.1:$relay_port" --out "$HARNESS_RUN"
driver_status=0
harness_wait "$HARNESS_PID" || driver_status=$?
if [[ $driver_status -ne 0 ]]; then
    cat "$HARNESS_RUN/driver.log" >&2
    exit "$driver_status"
fi
bun test/audio-quality/clients/js/ptt-analyze.ts "$HARNESS_RUN" | tee "$HARNESS_RUN/summary.log"
