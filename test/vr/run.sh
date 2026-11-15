#!/usr/bin/env bash
# Import a VR180 left-right MP4 with ambisonic Opus, publish it through a relay, and check demo/vr
# reads the catalog's projection, stereo, and layout and lays out the picture and sound by them.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin vr 'just test vr'
cd "$WORKSPACE"
cargo build --locked -p moq-relay -p moq-cli
uv run --no-project python sh/js/install.py frozen
(cd demo/vr && bunx vite build --outDir "$HARNESS_RUN/vr-dist" --emptyOutDir)

# Each eye is 480x480 over 180 degrees. Left eye: blue, red straight ahead (u 0.5), lime 45 degrees
# left (u 0.25). Right eye: magenta, so an uncropped frame puts magenta ahead. The sound is a plane
# wave from the left in first-order AmbiX (ACN W, Y, Z, X), so W and Y carry the same tone.
ffmpeg -hide_banner -loglevel error -f lavfi \
    -i "color=c=blue:s=960x480:r=15:d=2,drawbox=x=210:y=210:w=60:h=60:c=red:t=fill,drawbox=x=90:y=210:w=60:h=60:c=lime:t=fill,drawbox=x=480:y=0:w=480:h=480:c=magenta:t=fill" \
    -f lavfi -i "aevalsrc=exprs='0.5*sin(2*PI*1000*t)|0.5*sin(2*PI*1000*t)|0|0':s=48000:d=2" \
    -c:v libx264 -preset ultrafast -profile:v baseline -pix_fmt yuv420p -g 15 \
    -c:a libopus -mapping_family 2 -b:a 128k "$HARNESS_RUN/flat.mp4"
uv run --no-project python -I "$DIR/spherical.py" "$HARNESS_RUN/flat.mp4" "$HARNESS_RUN/vr180.mp4"

harness_port relay
URL="http://127.0.0.1:$HARNESS_PORT"
sed "s/4443/$HARNESS_PORT/g" "$DIR/relay.toml" >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" "$WORKSPACE/target/debug/moq-relay" "$HARNESS_RUN/relay.toml"
harness_ready "$URL/certificate.sha256" 30 "$HARNESS_PID"
# ffmpeg carries st3d and sv3d into each fragmented init segment only past -strict unofficial.
# shellcheck disable=SC2329 # invoked by harness_spawn
publish_vr() {
    ffmpeg -hide_banner -loglevel error -re -stream_loop -1 -i "$HARNESS_RUN/vr180.mp4" -c copy -strict unofficial \
        -movflags cmaf+frag_keyframe+empty_moov+default_base_moof -f mp4 - |
        "$WORKSPACE/target/debug/moq" --connect "$URL" --connect-version moq-lite-07-wip --broadcast vr.hang import fmp4
}
harness_spawn publisher "$HARNESS_RUN/publisher.log" publish_vr
cd test/interop/clients/js
# SwiftShader stands in for a GPU. ANGLE asks its Vulkan for a Wayland surface, which it lacks,
# whenever a Wayland session is visible, so a desktop run would have no WebGL.
env -u WAYLAND_DISPLAY bun vr.ts --url "$URL"
