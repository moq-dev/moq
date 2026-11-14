#!/usr/bin/env bash
# Measure the merge gate's build and test cost under two fixed scenarios for the
# nightly benchmark history (see bench/README.md): `cold`, a `just ci --all`
# from an empty target directory without a compiler cache, then `moq-net`, the
# gate an agent runs after editing moq-net, the crate most diffs touch, on what
# `cold` built. Modules run one at a time, so each one's cost is its own rather
# than whichever concurrent lane reached a shared build first.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
out=${BENCH_RESULTS:-$root/target/bench/nightly/gate-cost}
target=${CARGO_TARGET_DIR:-$root/target}
edit=rs/moq-net/src/lib.rs

# Instructions are the low-noise series for compiles. Counting user-space ones
# needs a hardware PMU, kernel.perf_event_paranoid at 2 or below, and
# perf_event_open allowed by any seccomp filter, which systemd-nspawn's denies.
if ! perf stat -x, -e instructions:u -o /dev/null -- true; then
    echo "gate-cost: perf cannot count user-space instructions; this host needs a PMU, kernel.perf_event_paranoid<=2, and perf_event_open past any seccomp filter" >&2
    exit 1
fi
# A compiler cache such as mbx, through a rustc wrapper in the environment or a
# Cargo config, or a Cargo shim on PATH, would replay compilations rather than
# measure them. The gate sets its own clippy-driver workspace wrapper later.
configs=(/dev/null)
for config in "$root"/.cargo/config{,.toml} "${CARGO_HOME:-$HOME/.cargo}"/config{,.toml}; do
    [[ -f "$config" ]] && configs+=("$config")
done
if [[ -n "${RUSTC_WRAPPER:-}${RUSTC_WORKSPACE_WRAPPER:-}${CARGO_BUILD_RUSTC_WRAPPER:-}${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" ]] ||
    grep -qE '^\s*rustc-(workspace-)?wrapper\s*=' "${configs[@]}" ||
    [[ "$(dirname "$(command -v cargo)")" != "$(dirname "$(command -v rustc)")" ]]; then
    echo "gate-cost: cargo runs through a rustc wrapper or shim ($(command -v cargo)); measure without the Rust cache" >&2
    exit 1
fi
# The moq-net scenario's diff must be its edit alone.
clean() {
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "gate-cost: the working tree has changes $1:" >&2
        git status --short >&2
        exit 1
    fi
}
clean "before the cold scenario"
if [[ -d "$target" && -n "$(find "$target" -mindepth 1 -maxdepth 1 ! -name bench -print -quit)" ]]; then
    echo "gate-cost: the cold scenario needs an empty target directory, but $target holds builds" >&2
    exit 1
fi

rm -rf "$out"
mkdir -p "$out/raw"
{
    rustc -V
    cargo -V
    cargo nextest --version | sed -n 1p
    echo "bun $(bun --version)"
    echo "$(nproc) x $(sed -n '/^model name/{s/.*: //p;q}' /proc/cpuinfo)"
    uname -sr
} >"$out/raw/toolchains.txt"

# The gate's nextest runs share one JUnit path, under the workspace's target/
# whatever CARGO_TARGET_DIR says; its last run is the workspace test.
junit=$root/target/nextest/ci/junit.xml

scenario() {
    local name=$1 base=$2 raw=$out/raw/$1
    mkdir -p "$raw"
    cp /proc/loadavg "$raw/loadavg"
    rm -f "$junit"
    echo "gate-cost: $name scenario"
    CHECK_MODULES='' CHECK_JOBS=1 CHECK_TIMES="$raw/modules.txt" JS_TEST_REPORTS="$raw/js" \
        RUSTC_WRAPPER="$root/sh/gh/rustc-cost.sh" RUSTC_COST="$raw/rustc.tsv" \
        just ci "$base"
    cp "$junit" "$raw/nextest.xml"
}

scenario cold --all
clean "after the cold scenario"
# A comment is enough: Cargo rebuilds a crate's dependents whenever it rebuilds the crate.
trap 'git checkout -- "$edit"' EXIT
echo "// gate-cost: an edit to moq-net" >>"$edit"
scenario moq-net HEAD
git checkout -- "$edit"
trap - EXIT

just gh benchmark-report cost "$out" "$out/report.json"
