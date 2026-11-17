#!/usr/bin/env bash
# Builds a 60 s, 48 kHz mono test signal: speech, a pause, steady fan-like noise alone and under
# speech, a pause, a quiet talker, and a trailing pause. Writes signal.wav and signal.s16.
set -euo pipefail
cd "$(dirname "$0")"
src=dl/speech.mp3

ffmpeg -hide_banner -loglevel error -y \
	-ss 60 -t 15 -i "$src" \
	-ss 120 -t 10 -i "$src" \
	-ss 200 -t 15 -i "$src" \
	-f lavfi -t 60 -i "anoisesrc=color=pink:amplitude=0.005:seed=1:sample_rate=48000" \
	-f lavfi -t 15 -i "anoisesrc=color=brown:amplitude=0.016:seed=2:sample_rate=48000" \
	-filter_complex "
		[0:a]aresample=48000,pan=mono|c0=c0,adelay=0:all=1[s1];
		[1:a]aresample=48000,pan=mono|c0=c0,adelay=25000:all=1[s2];
		[2:a]aresample=48000,pan=mono|c0=c0,volume=-20dB,adelay=40000:all=1[s3];
		[4:a]highpass=f=80,lowpass=f=4000,adelay=20000:all=1[fan];
		[s1][s2][s3][3:a][fan]amix=inputs=5:normalize=0:duration=longest,atrim=0:60[out]" \
	-map "[out]" -ar 48000 -ac 1 signal.wav

ffmpeg -hide_banner -loglevel error -y -i signal.wav -f s16le -ar 48000 -ac 1 signal.s16
