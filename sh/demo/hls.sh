#!/usr/bin/env bash
# Encode media/NAME.mp4 into a looping HLS stream (1280x720 + 256x144 + audio)
# under media/NAME/, and import it into RELAY as NAME.hang.
#
# Usage: sh/demo/hls.sh NAME RELAY, from demo/pub.
set -euo pipefail

usage="usage: sh/demo/hls.sh NAME RELAY"
name=${1:?$usage}
relay=${2:?$usage}

input="media/$name.mp4"
out="media/$name"

rm -rf "$out"
mkdir -p "$out"

echo ">>> Generating HLS stream to disk (1280x720 + 256x144)..."

ffmpeg -hide_banner -loglevel warning -re -stream_loop -1 -i "$input" \
    -filter_complex "[0:v]split=2[v0][v1]; [v0]scale=-2:720[v720]; [v1]scale=-2:144[v144]" \
    -map "[v720]" -map "[v144]" -map 0:a:0 \
    -r 25 -preset veryfast -g 50 -keyint_min 50 -sc_threshold 0 \
    -c:v:0 libx264 -profile:v:0 high -level:v:0 4.1 -pix_fmt:v:0 yuv420p -tag:v:0 avc1 \
    -b:v:0 4M -maxrate:v:0 4.4M -bufsize:v:0 8M \
    -c:v:1 libx264 -profile:v:1 high -level:v:1 4.1 -pix_fmt:v:1 yuv420p -tag:v:1 avc1 \
    -b:v:1 300k -maxrate:v:1 330k -bufsize:v:1 600k \
    -c:a aac -b:a 128k \
    -f hls -hls_time 2 -hls_list_size 6 \
    -hls_flags independent_segments+delete_segments \
    -hls_segment_type fmp4 \
    -master_pl_name master.m3u8 \
    -var_stream_map "v:0,agroup:audio,name:720 v:1,agroup:audio,name:144 a:0,agroup:audio,name:audio" \
    -hls_segment_filename "$out/v%v/segment_%09d.m4s" \
    "$out/v%v/stream.m3u8" &
ffmpeg=$!

cleanup() {
    echo "Shutting down..."
    kill "$ffmpeg" 2>/dev/null || true
    sleep 0.5
    kill -9 "$ffmpeg" 2>/dev/null || true
}
trap cleanup EXIT
# A signal exits, so the EXIT trap cleans up instead of the script resuming its waits.
trap 'exit 130' INT TERM

echo ">>> Waiting for HLS playlist generation..."
for _ in {1..30}; do
    if [ -f "$out/master.m3u8" ]; then break; fi
    sleep 0.5
done

if [ ! -f "$out/master.m3u8" ]; then
    echo "Error: master.m3u8 not generated in time" >&2
    exit 1
fi

echo ">>> Waiting for variant playlists..."
sleep 2
for _ in {1..20}; do
    if [ -f "$out/v720/stream.m3u8" ] || [ -f "$out/v144/stream.m3u8" ] || [ -f "$out/vaudio/stream.m3u8" ]; then
        break
    fi
    sleep 0.5
done

echo ">>> Importing HLS via moq-cli"
cargo run --bin moq -- --connect "$relay" --broadcast "$name.hang" import hls "$out/master.m3u8"
