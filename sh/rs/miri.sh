#!/usr/bin/env bash
# Run the hand-written in-memory unsafe under Miri: sh/rs/miri.sh [NEXTEST_ARGS...]
#
# Nightly only: Miri interprets every instruction, orders of magnitude slower than a
# native test, so the `miri` nextest profile gives each test longer. A test Miri cannot
# run (foreign calls, real sockets, the wall clock) carries
# `#[cfg_attr(miri, ignore = "...")]` rather than leaving a filter here.
#
# Two passes, by whether a test reaches crossbeam-epoch's global collector, which
# `kio::log` frees segments through:
#
# - strict: Stacked Borrows and the leak check.
# - epoch: Tree Borrows, because the collector's intrusive list breaks Stacked Borrows
#   (https://github.com/crossbeam-rs/crossbeam/issues/545), and no leak check, because
#   the global collector never runs at exit, so whatever it still holds then leaks.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if [[ "$(rustc --version)" == *-nightly* ]]; then
    sysroot=$(rustc --print sysroot)
elif command -v rustup >/dev/null 2>&1; then
    sysroot=$(rustup run nightly rustc --print sysroot)
else
    echo "rs: nightly Rust is missing; select it or run rustup toolchain install nightly --component miri" >&2
    exit 1
fi

# Nix puts its pinned stable compiler first; cargo and rustc must both be nightly.
export PATH="$sysroot/bin:$PATH"
if ! cargo miri --version >/dev/null 2>&1; then
    echo "rs: Miri is missing: rustup component add --toolchain nightly miri" >&2
    exit 1
fi
rustc --version

# Nextest unions repeated filtersets, so a caller's `-E` would add tests to a pass
# rather than narrow it; intersect it with each pass's filter instead.
args=()
filters=()
while (($#)); do
    case $1 in
        --)
            args+=("$@")
            break
            ;;
        -E | --filterset)
            filters+=("${2:?$1 needs a filterset}")
            shift 2
            ;;
        -E*)
            filters+=("${1#-E}")
            shift
            ;;
        --filterset=*)
            filters+=("${1#*=}")
            shift
            ;;
        *)
            args+=("$1")
            shift
            ;;
    esac
done
caller='all()'
if ((${#filters[@]})); then
    caller=$(printf ' | (%s)' "${filters[@]}")
    caller=${caller# | }
fi

# Nextest exits 4 when a pass selects no tests. Without arguments that means a filter
# below went stale; with them, the caller may have narrowed the run to one pass.
ran=0
pass() {
    local status=0
    cargo miri nextest run --locked --profile miri --no-fail-fast -p kio -p moq-net --lib \
        -E "($1) & ($caller)" ${args[@]+"${args[@]}"} || status=$?
    case $status in
        0) ran=1 ;;
        4) ((${#args[@]} + ${#filters[@]})) || exit 4 ;;
        *) exit "$status" ;;
    esac
}

pass '(package(kio) & not test(/^log::/)) | (package(moq-net) & (test(/^model::frame::/) | test(/^path::/)))'

MIRIFLAGS="${MIRIFLAGS:+$MIRIFLAGS }-Zmiri-tree-borrows -Zmiri-ignore-leaks" \
    pass '(package(kio) & test(/^log::/)) | (package(moq-net) & test(/^model::group::/))'

if ((! ran)); then
    echo "rs: no test matched in either Miri pass" >&2
    exit 4
fi
