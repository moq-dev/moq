#!/usr/bin/env bash
# Prove the route key model, then check each variant breaks exactly the lemmas its
# `Breaks:` line names. Tamarin exits 0 whatever it finds, so this reads its summary.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

spec=moq-net/verify/route-key/RouteKey.spthy
log=$(mktemp -d)
trap 'rm -rf "$log"' EXIT

failed=()
base_names=""
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
    lemmas=$(grep -E '^  [A-Za-z0-9_]+ \((all|exists)-traces?\): ' "$log/$name" || true)
    echo "== $name"
    echo "$lemmas"
    [ -n "$lemmas" ] || failed+=("$name: no lemma results")
    local names
    names=$(awk '{print $1}' <<<"$lemmas" | sort | tr '\n' ' ')
    if [ -z "$base_names" ]; then
        base_names=$names
    elif [ "$names" != "$base_names" ]; then
        failed+=("$name: lemmas are [$names], want the base's [$base_names]")
    fi
    local expected
    for expected in $breaks; do
        [[ " $names" == *" $expected "* ]] || failed+=("$name: breaks $expected, which has no result")
    done
    while read -r lemma _ result _; do
        [ -n "$lemma" ] || continue
        local want=verified
        [[ " $breaks " != *" $lemma "* ]] || want=falsified
        [ "$result" = "$want" ] || failed+=("$name: $lemma is $result, want $want")
    done <<<"$lemmas"
}

check base ""
variants=$(sed -nE 's/^ \* ([A-Za-z0-9_]+) +Breaks: (.*)$/\1 \2/p' "rs/$spec")
# Every #ifdef is a variant, so a header that stops parsing cannot retire its negative control.
while read -r variant; do
    grep -q "^$variant " <<<"$variants" || failed+=("$variant: no Breaks: line")
done < <(sed -nE 's/^#ifn?def ([A-Za-z0-9_]+).*/\1/p' "rs/$spec" | sort -u)
while read -r variant breaks; do
    [ -n "$variant" ] || continue
    check "$variant" "$breaks" "-D=$variant"
done <<<"$variants"

if [ ${#failed[@]} -ne 0 ]; then
    printf 'error: %s\n' "${failed[@]}" >&2
    exit 1
fi
