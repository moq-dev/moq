#!/usr/bin/env bash
# Model-check every TLA+ config under the given paths (default: all of them)
# through the `tla` recipes. A config with a `\* Open finding:` line must break
# a rule (a deadlock, invariant, property, or assertion), so a fix that should
# promote its rule is noticed; every other config must pass. A config with a
# `\* Simulate:` line is too large to exhaust: TLC samples it at random with the
# arguments that line gives. A `trace/` directory holds a trace spec, which
# checks recorded runs rather than exploring (`sh/tla-trace.py`).
set -euo pipefail
shopt -s nullglob
cd "$(git rev-parse --show-toplevel)"

[ $# -ne 0 ] || set -- rs js
mapfile -t configs < <(git ls-files --cached --others --exclude-standard -- "$@" | grep -E '^(rs|js)/.*/verify/.*\.cfg$' | grep -v '/trace/' || true)
if [ ${#configs[@]} -eq 0 ]; then
    echo "error: no TLA+ config under: $*" >&2
    exit 1
fi

failed=()
for cfg in "${configs[@]}"; do
    # The index still lists a config deleted but not yet staged.
    [ -f "$cfg" ] || continue
    # The directory's spec, or where it holds several, the one the config's name
    # starts with. TLC's trace-exploration output (`*_TTrace_*.tla`) is no spec.
    dir=$(dirname "$cfg")
    name=$(basename "$cfg")
    specs=()
    for spec in "$dir"/*.tla; do
        [[ "$spec" == *_TTrace_* ]] || specs+=("$spec")
    done
    if [ ${#specs[@]} -gt 1 ]; then
        all=("${specs[@]}")
        specs=()
        for spec in "${all[@]}"; do
            [[ "$name" != "$(basename "$spec" .tla)"* ]] || specs+=("$spec")
        done
    fi
    if [ ${#specs[@]} -ne 1 ]; then
        failed+=("$cfg: no single spec in $dir")
        continue
    fi
    lang=${cfg%%/*}
    finding=false
    ! grep -q '^\\\* Open finding:' "$cfg" || finding=true
    read -ra simulate < <(sed -n 's/^\\\* Simulate: //p' "$cfg") || true

    echo "::group::$cfg"
    start=$SECONDS
    status=0
    just "$lang" tla "${specs[0]#"$lang"/}" -config "$cfg" "${simulate[@]}" || status=$?
    echo "::endgroup::"
    echo "$cfg: exit $status in $((SECONDS - start))s"

    # TLC exits 11 to 14 for a deadlock, a violated invariant, a violated
    # property, and a failed assertion; anything else nonzero is an error.
    if $finding; then
        case $status in
            11 | 12 | 13 | 14) ;;
            0) failed+=("$cfg: passes, so its finding is fixed: promote its rule as the config says") ;;
            *) failed+=("$cfg: exit $status, not a broken rule") ;;
        esac
    elif [ "$status" -ne 0 ]; then
        failed+=("$cfg: exit $status")
    fi
done

if [ ${#failed[@]} -ne 0 ]; then
    printf 'error: %s\n' "${failed[@]}" >&2
    exit 1
fi
