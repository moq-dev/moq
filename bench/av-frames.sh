#!/usr/bin/env bash
set -euo pipefail

if (($# < 1 || $# > 2)); then
    printf 'usage: %s BASE [OUTPUT_DIRECTORY]\n' "$0" >&2
    exit 1
fi

ROOT=$(git rev-parse --show-toplevel)
BASE=$(git rev-parse --verify "$1^{commit}")
OUTPUT=${2:-$(mktemp -d "${TMPDIR:-/tmp}/moq-av-results.XXXXXX")}
mkdir -p "$OUTPUT"
OUTPUT=$(cd "$OUTPUT" && pwd)
if compgen -G "$OUTPUT/*.csv" >/dev/null; then
    printf 'refusing to overwrite CSVs in %s\n' "$OUTPUT" >&2
    exit 1
fi
RUN=$(mktemp -d "${TMPDIR:-/tmp}/moq-av-build.XXXXXX")
BASE_TREE=$RUN/base
cleanup() {
    if [[ -f "$BASE_TREE/.git" ]]; then
        git -C "$ROOT" worktree remove --force "$BASE_TREE"
    fi
    rm -rf -- "$RUN"
}
trap cleanup EXIT

# Separate targets prevent a cached build from selecting the other checkout's executable.
git -C "$ROOT" worktree add --detach "$BASE_TREE" "$BASE"
cmp "$ROOT/Cargo.lock" "$BASE_TREE/Cargo.lock"
for file in av-frames.rs support/live_allocation.rs; do
    cp "$ROOT/rs/moq-net/examples/$file" "$BASE_TREE/rs/moq-net/examples/$file"
done
(
    cd "$BASE_TREE"
    CARGO_TARGET_DIR="$RUN/target-before" cargo build --locked --release -p moq-net --example av-frames
)
cp "$RUN/target-before/release/examples/av-frames" "$RUN/before"
(
    cd "$ROOT"
    CARGO_TARGET_DIR="$RUN/target-after" cargo build --locked --release -p moq-net --example av-frames
)
cp "$RUN/target-after/release/examples/av-frames" "$RUN/after"

{
    printf 'base: %s\ncandidate: %s\n' "$BASE" "$(git -C "$ROOT" rev-parse HEAD)"
    printf 'order per case, fresh processes: before-1, after-1, after-2, before-2\n'
    date -u
    uname -a
    rustc -Vv
    if command -v lscpu >/dev/null; then lscpu; fi
} >"$OUTPUT/environment.txt"
sha256sum "$RUN/before" "$RUN/after" >"$OUTPUT/binaries.sha256"
git -C "$ROOT" diff HEAD >"$OUTPUT/candidate.patch"
cp "$ROOT/rs/moq-net/examples/av-frames.rs" "$OUTPUT/av-frames.rs"
cp "$ROOT/rs/moq-net/examples/support/live_allocation.rs" "$OUTPUT/live_allocation.rs"

"$RUN/after" --list >"$OUTPUT/cases.txt"
"$RUN/before" --list >"$RUN/base-cases.txt"
cmp "$OUTPUT/cases.txt" "$RUN/base-cases.txt"
run_case() {
    local side=$1 round=$2 case=$3
    local output=$OUTPUT/$side-$round.csv
    "$RUN/$side" --case "$case" >"$RUN/sample.csv"
    if [[ -f "$output" ]]; then
        tail -n +2 "$RUN/sample.csv" >>"$output"
    else
        cp "$RUN/sample.csv" "$output"
    fi
}
while IFS= read -r case; do
    printf 'Comparing %s\n' "$case" >&2
    run_case before 1 "$case"
    run_case after 1 "$case"
    run_case after 2 "$case"
    run_case before 2 "$case"
done <"$OUTPUT/cases.txt"
printf 'A/V comparison saved in %s\n' "$OUTPUT"
