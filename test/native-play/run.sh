#!/usr/bin/env bash
# Hear the shipped headless player through a private, clocked null audio device.
set -euo pipefail
if [[ $(uname -s) != Linux ]]; then
    echo "native-play requires Linux ALSA/cpal and PulseAudio" >&2
    exit 2
fi
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin native-play 'just test native-play'
cd "$WORKSPACE"
cargo build --locked -p moq-relay -p moq-cli --features moq-cli/play
harness_port relay
URL="http://127.0.0.1:$HARNESS_PORT"
sed "s/4443/$HARNESS_PORT/g" "$DIR/relay.toml" >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" target/debug/moq-relay "$HARNESS_RUN/relay.toml"
harness_ready "$URL/certificate.sha256" 30 "$HARNESS_PID"
# shellcheck disable=SC2329 # invoked by harness_spawn
publish_tone() {
    ffmpeg -hide_banner -v error -re -f lavfi -i sine=frequency=440:sample_rate=48000 \
        -ac 2 -c:a libopus -b:a 128k \
        -f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer -frag_duration 1000 - |
        "$WORKSPACE/target/debug/moq" --connect "$URL" --broadcast tone.hang import fmp4
}
harness_spawn publisher "$HARNESS_RUN/publisher.log" publish_tone
harness_spawn player - python3 "$DIR/../lib/pulse.py" --dir "$HARNESS_RUN" -- \
    python3 "$DIR/play.py" --url "$URL" --moq "$WORKSPACE/target/debug/moq"
harness_wait "$HARNESS_PID"
