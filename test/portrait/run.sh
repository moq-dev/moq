#!/usr/bin/env bash
# Publish a coded portrait, transcode a short-side rung, and paint it in Chromium.
set -euo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin portrait 'just test portrait'
cd "$WORKSPACE"
cargo build --locked -p moq-relay -p moq-cli --features moq-cli/transcode,moq-cli/openh264
bun install --frozen-lockfile
(cd test/interop/clients/js && bunx vite build --outDir "$HARNESS_RUN/js-dist")
harness_port relay
URL="http://127.0.0.1:$HARNESS_PORT"
sed "s/4443/$HARNESS_PORT/g" "$DIR/relay.toml" >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" "$WORKSPACE/target/debug/moq-relay" "$HARNESS_RUN/relay.toml"
harness_ready "$URL/certificate.sha256" 30 "$HARNESS_PID"
# shellcheck disable=SC2329 # invoked by harness_spawn
publish_portrait() {
    ffmpeg -hide_banner -loglevel error -re -f lavfi -i testsrc2=size=720x1280:rate=10 \
        -c:v libx264 -preset ultrafast -tune zerolatency -profile:v baseline -pix_fmt yuv420p -g 10 -f h264 - |
        "$WORKSPACE/target/debug/moq" --connect "$URL" --connect-version moq-lite-07-wip --broadcast portrait.hang import avc3
}
harness_spawn publisher "$HARNESS_RUN/publisher.log" publish_portrait
harness_spawn transcode "$HARNESS_RUN/transcode.log" "$WORKSPACE/target/debug/moq" \
    --connect "$URL" --connect-version moq-lite-07-wip --broadcast portrait.hang transcode --rung 360:600000 --encoder software --decoder software
cd test/interop/clients/js
bun portrait.ts --url "$URL"
