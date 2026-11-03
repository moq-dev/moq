#!/usr/bin/env bash
# The bufferbloated push-to-talk acceptance run; --reproduction is diagnostic only.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source test/lib/harness.sh
harness_begin push-to-talk "just test push-to-talk$(harness_argv "$@")"

case "${1:-}" in
    "") shape=(--delay 50ms --rate 64000 --queue 1s) ;;
    --reproduction) shape=(--delay 50ms --loss 0.02) ;;
    *) echo 'usage: just test push-to-talk [--reproduction]' >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { echo 'too many arguments' >&2; exit 2; }

bun install --frozen-lockfile
cargo build --locked -p moq-relay -p moq-shaper
TARGET_BASE="${CARGO_TARGET_DIR:-$ROOT/target}"
harness_port relay
relay_port=$HARNESS_PORT
harness_port shaper
shaper_port=$HARNESS_PORT
sed "s/4443/$relay_port/g" test/audio-quality/relay.toml >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" "$TARGET_BASE/debug/moq-relay" "$HARNESS_RUN/relay.toml"
harness_ready "http://127.0.0.1:$relay_port/certificate.sha256" 30 "$HARNESS_PID"
harness_spawn shaper "$HARNESS_RUN/shaper.log" "$TARGET_BASE/debug/moq-shaper" \
    --listen "127.0.0.1:$shaper_port" --target "127.0.0.1:$relay_port" --seed 7 \
    --report "$HARNESS_RUN/shaper.json" "${shape[@]}"
shaper_pid=$HARNESS_PID
harness_spawn driver "$HARNESS_RUN/driver.log" bun test/audio-quality/clients/js/ptt.ts \
    --publisher "http://127.0.0.1:$relay_port" --viewer "http://127.0.0.1:$shaper_port" --out "$HARNESS_RUN"
status=0
harness_wait "$HARNESS_PID" || status=$?
kill -TERM -- -"$shaper_pid" 2>/dev/null || true
harness_wait "$shaper_pid" || status=$?
analysis=()
[[ "${1:-}" != --reproduction ]] || analysis=(--diagnostic)
bun test/audio-quality/clients/js/ptt-analyze.ts "$HARNESS_RUN" "${analysis[@]}" >"$HARNESS_RUN/summary.log" || status=$?
exit "$status"
