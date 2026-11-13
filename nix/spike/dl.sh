#!/usr/bin/env bash
# dl.sh APPROACH RUN: bytes RUN substituted from the file cache (what CI would download).
set -euo pipefail
cd "$(dirname "$0")"
a=$1; run=$2; n=0; bytes=0
while read -r p src; do
  [[ "$src" == file://* ]] || continue
  h=$(basename "$p" | cut -c1-32)
  fs=$(awk '/^FileSize/{print $2}' "cache-$a/$h.narinfo")
  n=$((n+1)); bytes=$((bytes+fs))
done < "runs/$run/substituted"
echo "$run download: $n paths, $((bytes/1048576)) MiB zstd" | tee "runs/$run/download"
