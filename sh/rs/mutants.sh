#!/usr/bin/env bash
# Run cargo-mutants over the files .cargo/mutants.toml names, failing on any mutant the tests miss.
# A mutant that hangs the tests counts as caught, so only a missed one fails.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

out=${MUTANTS_OUT:-target/mutants}
mkdir -p "$out"
status=0
cargo mutants --package moq-net --output "$out" "$@" || status=$?
# 2 is a missed mutant and 3 a timeout; either may hide the other, so missed.txt decides.
case $status in
    0 | 2 | 3) ;;
    *) exit "$status" ;;
esac
missed="$out/mutants.out/missed.txt"
if [[ -s $missed ]]; then
    echo "mutants: missed $(wc -l <"$missed"):" >&2
    cat "$missed" >&2
    exit 1
fi
