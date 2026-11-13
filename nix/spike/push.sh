#!/usr/bin/env bash
# push.sh APPROACH RUN: what a CI `nix copy` of RUN's Rust outputs adds to a
# cache already holding the baseline (cache-APPROACH): new paths and bytes.
set -euo pipefail
cd "$(dirname "$0")"
a=$1; run=$2
tmp=cache-push-$a
rm -rf "$tmp"; cp -al "cache-$a" "$tmp"
before=$(ls "$tmp" | sort)
nix copy --to "file://$PWD/$tmp?compression=zstd&secret-key=$PWD/key.sec" $(./rustpaths.sh "$(cat runs/$run/drv)") 2>/dev/null
new=$(comm -13 <(echo "$before") <(ls "$tmp" | sort) | grep narinfo || true)
n=0; bytes=0; nar=0
for f in $new; do
  fs=$(awk '/^FileSize/{print $2}' "$tmp/$f"); ns=$(awk '/^NarSize/{print $2}' "$tmp/$f")
  n=$((n+1)); bytes=$((bytes+fs)); nar=$((nar+ns))
done
echo "$run push: $n paths ($((2*n)) objects), $((bytes/1048576)) MiB zstd, $((nar/1048576)) MiB NAR" | tee "runs/$run/push"
rm -rf "$tmp"
