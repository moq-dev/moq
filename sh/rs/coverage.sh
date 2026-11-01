#!/usr/bin/env bash
set -euo pipefail

crate=${1:-workspace}
report=()
if [[ $crate == workspace ]]; then
    # Fuzz targets require nightly.
    packages=(--workspace --exclude moq-net-fuzz --exclude moq-quic-fuzz)
elif [[ $crate =~ ^[a-zA-Z0-9_-]+$ ]]; then
    packages=(-p "$crate")
    report=(-p "$crate")
else
    echo "usage: just rs coverage [crate]" >&2
    exit 2
fi

out="target/coverage/$crate"
# mbx's cache remaps source paths to `${workspace}`, which llvm-cov cannot read back.
export MBX_DISABLE=1
rm -rf "$out"
# `--no-report` skips cargo-llvm-cov's own cleanup, so stale profiles would merge in.
cargo llvm-cov clean --workspace
bash sh/rs/nextest.sh cargo llvm-cov nextest --locked "${packages[@]}" --tests --no-report --test-threads "${NEXTEST_TEST_THREADS:-4}"
mkdir -p "$out"
cargo llvm-cov report "${report[@]}" | tee "$out/summary.txt"
cargo llvm-cov report "${report[@]}" --json --summary-only --output-path "$out/summary.json"
cargo llvm-cov report "${report[@]}" --lcov --output-path "$out/lcov.info"
cargo llvm-cov report "${report[@]}" --html --output-dir "$out"

if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
    {
        printf '\n## Rust unit coverage\n\n```text\n'
        cat "$out/summary.txt"
        printf '\n```\n'
    } >>"$GITHUB_STEP_SUMMARY"
fi
