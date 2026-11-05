#!/usr/bin/env bash
# Mutation-test the scope in .cargo/mutants.toml and list the mutants no test
# caught. Arguments pass to cargo-mutants, e.g. `--shard 0/8` or `--in-place`.
set -euo pipefail

out=target/mutants
rm -rf "$out"
# Not `cargo mutants`: cargo would point `$CARGO` back at itself.
export CARGO="$PWD/sh/rs/mutants-cargo.sh"
status=0
bash sh/rs/nextest.sh cargo-mutants mutants --cargo-arg=--locked --output "$out" "$@" || status=$?

# 2 means a mutant was missed and 3 that one timed out, which outranks a miss.
# A timeout is caught (the mutant hung a test), so missed.txt alone decides.
case $status in
    0 | 2 | 3) ;;
    *) exit "$status" ;;
esac
missed="$out/mutants.out/missed.txt"
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
    {
        printf '\n## Surviving mutants %s\n\n' "$*"
        if [[ -s $missed ]]; then
            printf '```text\n'
            cat "$missed"
            printf '```\n'
        else
            printf 'None.\n'
        fi
    } >>"$GITHUB_STEP_SUMMARY"
fi
[[ ! -s $missed ]]
