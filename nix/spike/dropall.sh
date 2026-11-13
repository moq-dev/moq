#!/usr/bin/env bash
# dropall.sh DRV...: delete the Rust outputs of each DRV from the local store.
# Not tikv-jemalloc-sys: Nix 2.35 reports its outputs (a "+" in the name) alive
# with no root it can name, and only root may --ignore-liveness. Unchanged in
# every scenario, it stays local, so subst runs neither build nor fetch it.
set -euo pipefail
cd "$(dirname "$0")"
rm -f runs/*/result runs/*/result-*
paths=$(for d in "$@"; do ./rustpaths.sh "$d"; done | sort -u | grep -v tikv-jemalloc-sys | while read -r p; do [ -e "$p" ] && echo "$p"; done || true)
[ -z "$paths" ] && { echo "nothing to delete"; exit 0; }
echo "deleting $(echo "$paths" | wc -l) paths"
if ! out=$(echo "$paths" | nix store delete --stdin 2>&1); then
  echo "$out" | grep -v "^deleting '" | tail -3
  alive=$(echo "$out" | grep -o "Cannot delete path '[^']*'" | cut -d"'" -f2)
  [ -n "$alive" ] && nix-store -q --roots "$alive"
  exit 1
fi
echo "$out" | tail -1
left=$(echo "$paths" | while read -r p; do [ -e "$p" ] && echo "$p"; done || true)
[ -z "$left" ] || { echo "still present: $(echo "$left" | wc -l)"; exit 1; }
