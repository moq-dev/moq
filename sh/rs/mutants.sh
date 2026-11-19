#!/usr/bin/env bash
# Run cargo-mutants over the files .cargo/mutants.toml names, failing on any mutant the tests miss.
# A mutant that hangs the tests counts as caught, so only a missed one fails.
#
#   mutants.sh [cargo-mutants args]     one run, such as `--file rs/moq-net/src/route/relay.rs`
#   mutants.sh plan                     the Nightly jobs, as `base=` and a `matrix=` of the shards below
#   mutants.sh changed SHARD SHARDS     one shard of the mutants changed since the previous Nightly
#   mutants.sh rotation SHARD SHARDS    one shard of tonight's rotation slice
#
# The Nightly shards mutate the checkout in place. The full set costs about 27
# hosted runner-hours, too much for one night, so Nightly runs every mutant in
# code changed since the previous scheduled Nightly, catching untested logic the
# next morning, and one slice of a rotation that covers every mutant every
# ROTATION nights, catching a test that stops killing what it used to. A heavy
# day can change most of the code, so `plan` gives the changed mutants as many
# shards as keep each to CHANGED_PER_SHARD, instead of a fixed count whose jobs
# then outgrow their timeout.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

ROTATION=8
ROTATION_SHARDS=6
# A caught mutant costs about two minutes on a hosted runner and a missed one four.
CHANGED_PER_SHARD=40
out=${MUTANTS_OUT:-target/mutants}

# Runs cargo-mutants into $out/$1, keeping its missed list for the verdict.
mutate() {
    local dir=$out/$1 status=0
    shift
    rm -rf "$dir"
    mkdir -p "$dir"
    cargo mutants --package moq-net --output "$dir" "$@" || status=$?
    # 2 is a missed mutant and 3 a timeout; either may hide the other, so missed.txt decides.
    case $status in
        0 | 2 | 3) ;;
        *) return "$status" ;;
    esac
    if [[ -s $dir/mutants.out/missed.txt ]]; then
        cat "$dir/mutants.out/missed.txt" >>"$out/missed.txt"
    fi
}

# Writes the diff since the previous scheduled Nightly (or $MUTANTS_BASE) to $out/changed.diff.
changed_diff() {
    base=${MUTANTS_BASE:-$(bash sh/gh/previous-nightly.sh)}
    # Nightly checks out shallowly, and the diff needs only the base's tree.
    if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
        git fetch --no-tags --depth=1 origin "$base"
    fi
    git diff "$base" HEAD -- rs/moq-net/src >"$out/changed.diff"
}

# Reads SHARD and SHARDS from the arguments after the mode.
shard_args() {
    if [[ $# != 2 ]]; then
        echo "usage: mutants.sh $mode SHARD SHARDS" >&2
        exit 2
    fi
    shard=$1 shards=$2
}

mkdir -p "$out"
: >"$out/missed.txt"
mode=${1:-}
case $mode in
    plan)
        changed_diff
        # An empty diff makes cargo-mutants list nothing, as it should.
        count=$(cargo mutants --package moq-net --list --in-diff "$out/changed.diff" | wc -l)
        n=$(((count + CHANGED_PER_SHARD - 1) / CHANGED_PER_SHARD))
        echo "mutants: $count changed, $n shards" >&2
        jobs=()
        for ((i = 0; i < ROTATION_SHARDS; i++)); do
            jobs+=("{\"mode\":\"rotation\",\"shard\":$i,\"shards\":$ROTATION_SHARDS}")
        done
        for ((i = 0; i < n; i++)); do
            jobs+=("{\"mode\":\"changed\",\"shard\":$i,\"shards\":$n}")
        done
        {
            echo "base=$base"
            echo "matrix={\"include\":[$(IFS=,; echo "${jobs[*]}")]}"
        } >>"${GITHUB_OUTPUT:-/dev/stdout}"
        exit 0
        ;;
    changed)
        shift
        shard_args "$@"
        changed_diff
        mutate changed --in-place --in-diff "$out/changed.diff" --shard "$shard/$shards"
        ;;
    rotation)
        shift
        shard_args "$@"
        night=$(($(date -u +%s) / 86400 % ROTATION))
        mutate rotation --in-place --shard "$((night * shards + shard))/$((ROTATION * shards))"
        ;;
    *)
        mutate run "$@"
        ;;
esac
if [[ -s $out/missed.txt ]]; then
    echo "mutants: missed $(sort -u "$out/missed.txt" | wc -l):" >&2
    sort -u "$out/missed.txt" >&2
    exit 1
fi
