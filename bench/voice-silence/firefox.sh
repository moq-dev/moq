#!/usr/bin/env bash
# Inside a private PulseAudio server (test/lib/pulse.py): plays the input into the null sink, makes
# its monitor the default microphone, and records it through Firefox.
# Usage: firefox.sh <input> <processing on|off>
set -euo pipefail
cd "$(dirname "$0")"
pactl set-default-source moq-null.monitor
ffmpeg -hide_banner -loglevel error -y -stream_loop 2 -i "$1.wav" -ac 2 -ar 48000 "loop-$1.wav"
paplay --device=moq-null "loop-$1.wav" &
player=$!
trap 'kill $player 2>/dev/null || true' EXIT
bun drive.ts "$1.wav" 62 "firefox-$1-$2" "$2" firefox 2>&1 | grep -v PLAYWRIGHT_SKIP | tail -1
uv run --no-project --with numpy --with soundfile python analyze.py "$1.wav" "firefox-$1-$2" > "firefox-$1-$2.txt"
