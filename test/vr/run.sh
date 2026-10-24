#!/usr/bin/env bash
# Publish a stereo 360 picture with VR catalog flags and check demo/vr paints each eye the right way round.
set -euo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin vr 'just test vr'
cd "$WORKSPACE"
cargo build --locked -p moq-relay -p moq-cli
bun install --frozen-lockfile
(cd demo/vr && bunx vite build --outDir "$HARNESS_RUN/vr-dist" --emptyOutDir)
# The ambisonic decoder alone, so the driver can render it offline without a stream.
bun build demo/vr/src/audio.ts --format esm --outfile "$HARNESS_RUN/vr-dist/audio-check.js"
(cd test/interop/clients/js && bunx playwright install chromium)
harness_port relay
URL="http://127.0.0.1:$HARNESS_PORT"
sed "s/4443/$HARNESS_PORT/g" "$DIR/relay.toml" >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" "$WORKSPACE/target/debug/moq-relay" "$HARNESS_RUN/relay.toml"
harness_ready "$URL/certificate.sha256" 30 "$HARNESS_PID"
# Left eye: blue, red straight ahead (u 0.5), lime 90 degrees left (u 0.25). Right eye: magenta,
# so a frame shown uncropped puts magenta straight ahead.
# shellcheck disable=SC2329 # invoked by harness_spawn
publish_vr() {
    ffmpeg -hide_banner -loglevel error -re -f lavfi \
        -i "color=c=blue:s=960x480:r=15,drawbox=x=210:y=210:w=60:h=60:c=red:t=fill,drawbox=x=90:y=210:w=60:h=60:c=lime:t=fill,drawbox=x=480:y=0:w=480:h=480:c=magenta:t=fill" \
        -c:v libx264 -preset ultrafast -tune zerolatency -profile:v baseline -pix_fmt yuv420p -g 15 -f h264 - |
        "$WORKSPACE/target/debug/moq" --connect "$URL" --connect-version moq-lite-07-wip --broadcast vr.hang \
            import --projection equirectangular --stereo left-right avc3
}
harness_spawn publisher "$HARNESS_RUN/publisher.log" publish_vr
cd test/interop/clients/js
bun vr.ts --url "$URL"
