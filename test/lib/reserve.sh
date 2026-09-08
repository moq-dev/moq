#!/usr/bin/env bash
# Update one port reservation while the caller holds its advisory lock.

set -euo pipefail

root="$1"
port="$2"
owner="$3"
run="$4"
target="$root/$port"

process_identity() {
    ps -o lstart= -p "$1" 2>/dev/null | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' || true
}

process_exited() {
    local state
    state=$(ps -o state= -p "$1" 2>/dev/null || true)
    if [[ -n "$state" ]]; then
        [[ "$state" == Z* ]]
    else
        if kill -0 "$1" 2>/dev/null; then
            return 1
        fi
        return 0
    fi
}

if [[ -e "$target" && ! -d "$target" ]]; then
    echo "error: malformed port reservation: $target is not a directory" >&2
    exit 2
fi

if [[ -d "$target" ]]; then
    previous=$(cat "$target/pid" 2>/dev/null || true)
    previous_identity=$(cat "$target/identity" 2>/dev/null || true)
    if [[ -n "$previous" ]] && ! process_exited "$previous"; then
        current_identity=$(process_identity "$previous")
        if [[ -z "$previous_identity" || -z "$current_identity" || "$previous_identity" == "$current_identity" ]]; then
            exit 1
        fi
    fi

    aside=$(mktemp -d "$root/.stale-$port-XXXXXXXX")
    rmdir "$aside"
    mv "$target" "$aside"
    rm -rf "${aside:?}"
fi

claim=$(mktemp -d "$root/.claim-$port-XXXXXXXX")
trap 'rm -rf "${claim:?}"' EXIT
printf '%s\n' "$owner" >"$claim/pid"
printf '%s\n' "$run" >"$claim/run"
process_identity "$owner" >"$claim/identity"
mv "$claim" "$target"
trap - EXIT
