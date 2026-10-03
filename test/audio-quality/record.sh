#!/usr/bin/env bash
# Records the arrival traces under traces/, which the replay rows grade. See README.md.
#
# Three lanes, each --duration seconds from the first arrival:
#   relay-bbb   the public relay's Big Buck Bunny, the AAC flush-span shape #3477 measured
#   local-aac   an AAC tone through a local relay, the shallow control
#   relay-mic   a browser publishing Chromium's fake capture device through the public relay
#
# Not part of a run: re-record by hand when the path or the publishers change, and re-measure the
# replay budgets after.
set -euo pipefail

AQ_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$AQ_DIR/../.." && pwd)
CLIENT="$AQ_DIR/clients/js"

# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$AQ_DIR/../lib/harness.sh"

RERUN="just test audio-quality-record$(harness_argv "$@")"

ALL_LANES=(relay-bbb local-aac relay-mic)
LANES=("${ALL_LANES[@]}")
DURATION=35
PUBLIC="https://cdn.moq.dev"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lanes)
            IFS=, read -r -a LANES <<<"${2:?--lanes requires a value}"
            shift 2
            ;;
        --duration)
            DURATION="${2:?--duration requires a value}"
            shift 2
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done
for lane in "${LANES[@]}"; do
    [[ " ${ALL_LANES[*]} " == *" $lane "* ]] || {
        echo "error: unknown lane '$lane' (known: ${ALL_LANES[*]})" >&2
        exit 2
    }
done

harness_begin audio-quality-record "$RERUN"

echo "building the pages..."
(
    cd "$CLIENT"
    bun install --frozen-lockfile
    bunx playwright install chromium
    bunx vite build --outDir "$HARNESS_RUN/page" --emptyOutDir
)

DATE=$(date -u +%Y-%m-%d)
record() {
    local lane="$1"
    shift
    echo ""
    echo "── $lane ──"
    bun "$CLIENT/record.ts" --page "$HARNESS_RUN/page" --duration "$DURATION" \
        --out "$AQ_DIR/traces/$lane.json" "$@"
}

for lane in "${LANES[@]}"; do
    case "$lane" in
        relay-bbb)
            record "$lane" --url "$PUBLIC/demo" --broadcast bbb.hang \
                --source "$PUBLIC/demo bbb.hang, the public demo's MPEG-TS import, recorded $DATE" \
                --description "The public relay's Big Buck Bunny: one AAC frame per group, flushed in bursts of about seven, the flush-span shape #3477 measured."
            ;;
        local-aac)
            (cd "$WORKSPACE" && cargo build --locked -p moq-relay -p moq-cli)
            target="${CARGO_TARGET_DIR:-$WORKSPACE/target}/debug"
            harness_port relay
            url="http://127.0.0.1:$HARNESS_PORT"
            sed "s/4443/$HARNESS_PORT/g" "$AQ_DIR/relay.toml" >"$HARNESS_RUN/relay.toml"
            harness_spawn relay "$HARNESS_RUN/relay.log" "$target/moq-relay" "$HARNESS_RUN/relay.toml"
            harness_ready "$url/certificate.sha256" 30 "$HARNESS_PID" || {
                echo "relay never became ready" >&2
                exit 1
            }
            # The harness's own AAC publisher, so the control is the shape its rows play.
            # shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
            publish() {
                ffmpeg -hide_banner -v error -re -f lavfi -i "sine=frequency=440:sample_rate=44100" \
                    -ac 2 -c:a aac -b:a 128k -f mpegts - |
                    "$target/moq" --connect "$url" --broadcast tone-aac.hang import ts
            }
            harness_spawn pub-aac "$HARNESS_RUN/pub-aac.log" publish
            record "$lane" --url "$url" --broadcast tone-aac.hang \
                --source "a local moq-relay on 127.0.0.1, ffmpeg's AAC sine over MPEG-TS through moq import ts, recorded $DATE" \
                --description "The shallow control: the audio quality harness's AAC publisher with nothing but loopback between it and the viewer."
            ;;
        relay-mic)
            broadcast="aq-mic-$RANDOM$RANDOM.hang"
            record "$lane" --mic --url "$PUBLIC/anon" --broadcast "$broadcast" \
                --source "$PUBLIC/anon, <moq-publish> in headless Chromium capturing its fake audio device, recorded $DATE" \
                --description "A browser publisher through the public relay: getUserMedia, the capture worklet, and WebCodecs Opus, with the relay's round trip in between."
            ;;
    esac
done
