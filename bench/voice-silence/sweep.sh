#!/usr/bin/env bash
# The port against libopus 1.6.1 on the same inputs (run build.sh, make-signal.sh, make-fan.sh first):
# the analysis's activity probability at each rate and frame size, the encoder's DTX decision, and
# the frames the Gate sends against the frames the encoder sends.
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD
js() { (cd ../../js/publish && bun "$here/$1" "${@:2}"); }

for rate in 16000 24000; do
	ffmpeg -hide_banner -loglevel error -y -f s16le -ar 48000 -ac 1 -i signal.s16 -f s16le -ar $rate -ac 1 signal-$rate.s16
done
cp signal.s16 signal-48000.s16
cp fan.s16 fan-48000.s16

echo "## Activity probability"
for input in signal-16000 signal-24000 signal-48000 fan-48000; do
	rate=${input#*-}
	for ms in 2.5 5 10 20 40 60; do
		frame=$(python3 -c "print(int($rate * $ms / 1000))")
		./ref_analysis "$rate" "$frame" < "$input.s16" > ref.txt
		js prob.ts "$here/$input.s16" "$rate" "$frame" 2> /dev/null > port.txt
		echo "$input, $ms ms: $(python3 cmp.py port.txt ref.txt | tail -1)"
	done
done

echo "## DTX decision (complexity 9, 32 kbps)"
for input in signal-16000 signal-24000 signal-48000 fan-48000; do
	rate=${input#*-}
	for ms in 2.5 5 10 20 40 60; do
		frame=$(python3 -c "print(int($rate * $ms / 1000))")
		./dtx "$rate" "$frame" 9 < "$input.s16" > ref.txt
		js decide.ts "$here/$input.s16" "$rate" "$frame" > port.txt
		echo "$input, $ms ms: $(python3 cmpdtx.py port.txt ref.txt)"
		js gatevs.ts "$here/$input.s16" "$rate" "$frame" "$here/ref.txt"
	done
done
