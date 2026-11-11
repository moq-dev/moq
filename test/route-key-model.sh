#!/usr/bin/env bash
# Prove the route key model, then check each variant breaks exactly the lemmas its
# `Breaks:` line names. Tamarin exits 0 whatever it finds, so this reads its summary.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

spec=moq-net/verify/route-key/RouteKey.spthy
log=$(mktemp -d)
trap 'rm -rf "$log"' EXIT

failed=()
check() {
    local name=$1 breaks=$2
    shift 2
    local status=0
    just rs tamarin "$spec" "$@" >"$log/$name" 2>&1 || status=$?
    if [ "$status" -ne 0 ]; then
        cat "$log/$name"
        failed+=("$name: tamarin exited $status")
        return
    fi
    if grep -q 'wellformedness check failed' "$log/$name"; then
        failed+=("$name: not wellformed")
    fi
    local lemmas
    lemmas=$(grep -E '^  [a-z_]+ \((all|exists)-traces?\): ' "$log/$name" || true)
    echo "== $name"
    echo "$lemmas"
    [ -n "$lemmas" ] || failed+=("$name: no lemma results")
    while read -r lemma _ result _; do
        [ -n "$lemma" ] || continue
        local want=verified
        [[ " $breaks " != *" $lemma "* ]] || want=falsified
        [ "$result" = "$want" ] || failed+=("$name: $lemma is $result, want $want")
    done <<<"$lemmas"
}

check base ""
while read -r variant _ breaks; do
    check "$variant" "$breaks" "-D=$variant"
done < <(sed -nE 's/^ \* ([A-Z_]+) +Breaks: (.*)$/\1 Breaks: \2/p' "rs/$spec")

if [ ${#failed[@]} -ne 0 ]; then
    printf 'error: %s\n' "${failed[@]}" >&2
    exit 1
fi
