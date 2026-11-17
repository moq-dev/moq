#!/usr/bin/env bash
# Records each input through a browser with its audio processing on and off.
# Usage: runs.sh <chromium|firefox> [inputs...]
set -euo pipefail
cd "$(dirname "$0")"
engine=$1
shift
for input in "${@:-signal fan}"; do
	for processing in on off; do
		out="$engine-$input-$processing"
		bun drive.ts "$input.wav" 62 "$out" "$processing" "$engine" 2>&1 | grep -v PLAYWRIGHT_SKIP | tail -1
		uv run --no-project --with numpy --with soundfile python analyze.py "$input.wav" "$out" > "$out.txt"
	done
done
