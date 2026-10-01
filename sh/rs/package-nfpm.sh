#!/usr/bin/env bash
set -euo pipefail

if (($# != 3)); then
    echo "Usage: $0 <config> <packager> <target>" >&2
    exit 2
fi

config=$1
packager=$2
target=$3

mapfile -t variables < <(envsubst --variables "$(<"$config")")
format=
for variable in "${variables[@]}"; do
    if [[ ! -v "$variable" ]]; then
        echo "Missing environment variable in $config: $variable" >&2
        exit 1
    fi
    printf -v format '%s$%s ' "$format" "$variable"
done

rendered=$(mktemp)
trap 'rm -f "$rendered"' EXIT
envsubst "$format" <"$config" >"$rendered"

nfpm pkg \
    --packager "$packager" \
    --config "$rendered" \
    --target "$target"
