#!/usr/bin/env bash
# Generate a deterministic H.264 test clip with ffmpeg and split it into a
# language-agnostic asset manifest (clip.h264 + asset.json + frames/).
# Nothing here is committed; the output dir is regenerated each run.
set -euo pipefail

out=${1:?usage: gen-asset.sh <out_dir>}
fps=${SMOKE_FPS:-30}
seconds=${SMOKE_SECONDS:-2}
size=${SMOKE_SIZE:-320x240}

mkdir -p "$out"
clip="$out/clip.h264"

# Baseline profile (no B-frames) keeps one slice per access unit, and
# repeat-headers=1 re-emits SPS/PPS before every keyframe so late subscribers
# and the fmp4 export path can initialize without the very first packet.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=${size}:rate=${fps}:duration=${seconds}" \
    -an -c:v libx264 -profile:v baseline -preset ultrafast -pix_fmt yuv420p \
    -x264-params "keyint=${fps}:min-keyint=${fps}:scenecut=0:repeat-headers=1" \
    -f h264 "$clip"

python3 "$(dirname "$0")/extract_asset.py" "$clip" "$out" "$fps"
