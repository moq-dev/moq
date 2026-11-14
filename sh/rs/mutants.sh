#!/usr/bin/env bash
# Run cargo-mutants over the files .cargo/mutants.toml names, failing on any mutant the tests miss.
# A mutant that hangs the tests counts as caught, so only a missed one fails.
#
#   mutants.sh [cargo-mutants args]   one run, such as `--file rs/moq-net/src/route/view.rs`
#   mutants.sh nightly SHARD SHARDS   one Nightly shard, mutating the checkout in place
#
# The full set costs about 27 hosted runner-hours, too much for one night. Each
# Nightly shard runs its share of the mutants in code changed since the
# previous scheduled Nightly, so untested logic is caught the next morning, and
# then one slice of a rotation that covers every mutant every ROTATION nights,
# which catches a test that stops killing what it used to.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

ROTATION=8
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

mkdir -p "$out"
: >"$out/missed.txt"
if [[ ${1:-} == nightly ]]; then
    if [[ $# != 3 ]]; then
        echo 'usage: mutants.sh nightly SHARD SHARDS' >&2
        exit 2
    fi
    shard=$2 shards=$3
    base=${MUTANTS_BASE:-$(bash sh/gh/previous-nightly.sh)}
    # Nightly checks out shallowly, and the diff needs only the base's tree.
    if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
        git fetch --no-tags --depth=1 origin "$base"
    fi
    git diff "$base" HEAD -- rs/moq-net/src >"$out/changed.diff"
    mutate changed --in-place --in-diff "$out/changed.diff" --shard "$shard/$shards"
    night=$(($(date -u +%s) / 86400 % ROTATION))
    mutate rotation --in-place --shard "$((night * shards + shard))/$((ROTATION * shards))"
else
    mutate run "$@"
fi
if [[ -s $out/missed.txt ]]; then
    echo "mutants: missed $(sort -u "$out/missed.txt" | wc -l):" >&2
    sort -u "$out/missed.txt" >&2
    exit 1
fi
