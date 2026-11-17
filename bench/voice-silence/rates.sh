#!/usr/bin/env bash
# Compares the JS port against libopus's C at every rate with an integer path, for both block sizes.
set -euo pipefail
cd "$(dirname "$0")"
for rate in 8000 12000 16000 24000 32000 48000; do
	ffmpeg -hide_banner -loglevel error -y -i signal.wav -f s16le -ar "$rate" -ac 1 "signal-$rate.s16"
	for block in 10 20; do
		./harness "$rate" "$block" < "signal-$rate.s16" > "c-$rate-$block.txt"
		bun compare.ts "signal-$rate.s16" "$rate" "$block" > "js-$rate-$block.txt"
		blocks=$(wc -l < "c-$rate-$block.txt")
		differ=$(diff "c-$rate-$block.txt" "js-$rate-$block.txt" | grep -c '^<' || true)
		active=$(awk '$1>=13' "c-$rate-$block.txt" | wc -l)
		echo "$rate Hz, $block ms: $blocks blocks, $active active, $differ differ"
	done
done
