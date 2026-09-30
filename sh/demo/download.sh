#!/usr/bin/env bash
# Download URL to FILE unless FILE already exists. The download lands in a
# temporary file first, so an interrupted one is retried instead of cached.
#
# Usage: sh/demo/download.sh URL FILE
set -euo pipefail

usage="usage: sh/demo/download.sh URL FILE"
url=${1:?$usage}
file=${2:?$usage}

[ -f "$file" ] && exit 0
mkdir -p "$(dirname "$file")"
curl -fsSL -o "$file.tmp" "$url"
mv "$file.tmp" "$file"
