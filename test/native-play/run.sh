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
harness_spawn player - python3 "$DIR/../lib/pulse.py" --dir "$HARNESS_RUN" -- \
    python3 "$DIR/play.py" --url "$URL" --moq "$WORKSPACE/target/debug/moq"
harness_wait "$HARNESS_PID"
