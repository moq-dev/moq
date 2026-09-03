#!/usr/bin/env bash
set -euo pipefail

GENERATED=$1
WRAPPER=$2
TEST=$3

generated=$(sed -n 's/^var \(ErrMoqError[[:alnum:]]*\) =.*/\1/p' "$GENERATED" | sort)
wrapped=$(sed -n 's/^[[:space:]]*Err[[:alnum:]]* = ffi\.\(ErrMoqError[[:alnum:]]*\)$/\1/p' "$WRAPPER" | sort)
tested=$(sed -n 's/.*ffi\.New\(MoqError[[:alnum:]]*\)().*/Err\1/p' "$TEST" | sort)

if [[ -z "$generated" || -z "$wrapped" || -z "$tested" ]]; then
    echo "go check: failed to discover MoqError sentinels" >&2
    exit 1
fi

compare() {
    local label=$1
    local actual=$2

    if diff -u <(printf '%s\n' "$generated") <(printf '%s\n' "$actual"); then
        return
    fi

    echo "go check: generated MoqError variants and $label are out of sync" >&2
    exit 1
}

compare "wrapper sentinels" "$wrapped"
compare "sentinel tests" "$tested"
