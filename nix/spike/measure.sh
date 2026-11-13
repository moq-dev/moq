#!/usr/bin/env bash
# Usage: copy nix/spike/* into .tmp/ and run from there; see the PR for the order.
# measure.sh LABEL ATTR [extra nix build args...]
# Builds ATTR, then reports wall, builder CPU (from each built drv's MEASURE-TIMES),
# derivations built and paths substituted. Run under quest gate.
set -euo pipefail
label=$1; attr=$2; shift 2
tmp=$(cd "$(dirname "$0")" && pwd)
dir=$tmp/runs/$label
mkdir -p "$dir"
cd "$(git -C "$tmp" rev-parse --show-toplevel)"
# Evaluate first so the timed build is only building.
drv=$(nix eval --raw "$attr.drvPath" 2>/dev/null)
echo "$drv" > "$dir/drv"
cat /proc/loadavg > "$dir/load.before"
start=$(date +%s.%N)
set +e
/usr/bin/time -f '%e %U %S' -o "$dir/client.time" \
  nix build "$drv^*" --max-jobs 24 --out-link "$dir/result" "$@" 2> "$dir/build.log"
status=$?
set -e
end=$(date +%s.%N)
cat /proc/loadavg > "$dir/load.after"
echo "$start $end" > "$dir/wall"
grep -o "building '/nix/store/[^']*\.drv'" "$dir/build.log" | sed "s/building '//;s/'$//" | sort -u > "$dir/built" || true
grep -o "copying path '/nix/store/[^']*' from '[^']*'" "$dir/build.log" | sed "s/copying path '//;s/' from '/ /;s/'$//" | sort -u > "$dir/substituted" || true
"$(dirname "$0")/analyze.sh" "$dir"
exit $status
