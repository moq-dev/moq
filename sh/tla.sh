#!/usr/bin/env bash
# Model-check every TLA+ config under the given paths (default: all of them)
# through the `tla` recipes. A config with a `\* Open finding:` line must break
# a rule (a deadlock, invariant, property, or assertion), so a fix that should
# promote its rule is noticed; every other config must pass.
set -euo pipefail
shopt -s nullglob
cd "$(git rev-parse --show-toplevel)"

[ $# -ne 0 ] || set -- rs js
mapfile -t configs < <(git ls-files --cached --others --exclude-standard -- "$@" | grep -E '^(rs|js)/.*/verify/.*\.cfg$' || true)
if [ ${#configs[@]} -eq 0 ]; then
    echo "error: no TLA+ config under: $*" >&2
    exit 1
fi

failed=()
for cfg in "${configs[@]}"; do
    # The directory's spec, or where it holds several, the one the config's name starts with.
    dir=$(dirname "$cfg")
    specs=("$dir"/*.tla)
    if [ ${#specs[@]} -gt 1 ]; then
        name=$(basename "$cfg")
        specs=()
        for spec in "$dir"/*.tla; do
            [[ "$name" != "$(basename "$spec" .tla)"* ]] || specs+=("$spec")
        done
    fi
    if [ ${#specs[@]} -ne 1 ]; then
        echo "error: no single spec in $dir for $cfg" >&2
        exit 1
    fi
    lang=${cfg%%/*}
    finding=false
    ! grep -q '^\\\* Open finding:' "$cfg" || finding=true

    echo "::group::$cfg"
    start=$SECONDS
    status=0
    just "$lang" tla "${specs[0]#"$lang"/}" -config "$cfg" || status=$?
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
