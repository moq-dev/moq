#!/usr/bin/env bash
# Encode media/NAME.mp4 into a looping HLS stream (720p + 144p + audio)
# under media/NAME/ and serve it over HTTP on PORT, for testing HLS import.
#
# Usage: sh/demo/serve-hls.sh NAME PORT, from demo/pub.
set -euo pipefail

usage="usage: sh/demo/serve-hls.sh NAME PORT"
name=${1:?$usage}
port=${2:?$usage}

input="media/$name.mp4"
out="media/$name"

rm -rf "$out"
mkdir -p "$out"

echo ">>> Starting HLS stream generation..."
echo ">>> Master playlist: http://localhost:$port/master.m3u8"

ffmpeg -loglevel warning -re -stream_loop -1 -i "$input" \
    -map 0:v:0 -map 0:v:0 -map 0:a:0 \
    -r 25 -preset veryfast -g 50 -keyint_min 50 -sc_threshold 0 \
    -c:v:0 libx264 -profile:v:0 high -level:v:0 4.1 -pix_fmt:v:0 yuv420p -tag:v:0 avc1 -bsf:v:0 dump_extra -b:v:0 4M -filter:v:0 "scale=-2:720" \
    -c:v:1 libx264 -profile:v:1 high -level:v:1 4.1 -pix_fmt:v:1 yuv420p -tag:v:1 avc1 -bsf:v:1 dump_extra -b:v:1 300k -filter:v:1 "scale=-2:144" \
    -c:a aac -b:a 128k \
    -f hls \
    -hls_time 2 -hls_list_size 12 \
    -hls_flags independent_segments+delete_segments \
    -hls_segment_type fmp4 \
    -master_pl_name master.m3u8 \
    -var_stream_map "v:0,agroup:audio v:1,agroup:audio a:0,agroup:audio" \
    -hls_segment_filename "$out/v%v/segment_%09d.m4s" \
    "$out/v%v/stream.m3u8" &
ffmpeg=$!

trap 'echo "Shutting down..."; exit 0' INT TERM
trap 'kill "$ffmpeg" 2>/dev/null || true' EXIT

sleep 2
echo ">>> HTTP server: http://localhost:$port/"
cd "$out"
python3 -m http.server "$port"
