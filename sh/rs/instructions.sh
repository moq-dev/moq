#!/usr/bin/env bash
# Count the deterministic Criterion benches' instructions under stock Valgrind with swole,
# as the nightly Instructions workflow does (x86-64 Linux).
#
# Usage: sh/rs/instructions.sh run|compare <base>|fingerprint [swole options]
#
# With `--benchmarks <file>`, Cargo builds only the bench targets its names live in, since each
# name starts with its target's file (`rs/<package>/benches/<target>.rs::`), so a bisect step
# builds its suspects' targets alone. It still selects every package and feature, so Cargo
# resolves the same features, and the targets link the same code, as a full count's.
set -euo pipefail

usage="usage: sh/rs/instructions.sh run|compare <base>|fingerprint [swole options]"
command=${1:?$usage}

# Valgrind runs ADX but hides it from CPUID (https://bugs.kde.org/show_bug.cgi?id=494162), so
# rind is built to skip that one startup check, or it refuses the simulated CPU.
export VALGRIND_BUG_494162=1
bench=(nix run --inputs-from . swole --)

# swole's `sim` counts under Valgrind; `run` counts this commit and `compare <base>` both sides.
shift
case "$command" in
    fingerprint) exec "${bench[@]}" fingerprint sim "$@" ;;
    help | -h | --help)
        echo "$usage"
        echo "Options are swole sim's, below:"
        exec "${bench[@]}" --help
        ;;
    run | compare) ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

# Only single-threaded targets without I/O, and not targets made only of `iter_custom`, which
# CodSpeed's Criterion skips; some still read the wall clock, so their counts move with the host's
# load (quest/m0/repeatable-instruction-counts.md). Nightly still times them all.
targets=(
    "kio tasks" "kio waiter"
    "moq-audio bus"
    "moq-auth jwt"
    "moq-e2ee protect"
    "moq-json codec" "moq-json table"
    "moq-mux catalog" "moq-mux containers" "moq-mux decode_clock" "moq-mux export" "moq-mux jitter"
    "moq-mux parameter_sets" "moq-mux sync" "moq-mux ts_programs" "moq-mux ts_schedule" "moq-mux ts_video"
    "moq-net announce" "moq-net auth" "moq-net bandwidth" "moq-net canonical" "moq-net codec" "moq-net frame"
    "moq-net interests" "moq-net priority" "moq-net retirement" "moq-net schedule" "moq-net stats"
    "moq-net prefixes" "moq-net synced" "moq-net tail" "moq-net tracks" "moq-net varint" "moq-net viewers"
    "moq-quic endpoint" "moq-quic receive" "moq-quic sent_packets" "moq-quic streams"
    "moq-rtmp chunk_interleaving"
)
# The features a package's targets need.
declare -A features=(["moq-json"]=bench ["moq-net"]=fuzz ["moq-quic"]=fuzz)

# The targets the `--benchmarks` file names, when given.
named=()
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    if [[ ${args[i]} == --benchmarks ]]; then
        file=${args[i + 1]:?--benchmarks takes a file}
        while IFS= read -r name || [[ -n $name ]]; do
            if [[ $name =~ ^[[:space:]]*rs/([^/]+)/benches/([^/]+)\.rs:: ]]; then
                named+=("${BASH_REMATCH[1]} ${BASH_REMATCH[2]}")
            fi
        done <"$file"
    fi
done

packages=() benches=()
for target in "${targets[@]}"; do
    packages+=("${target% *}")
    if ((${#named[@]})); then
        wanted=false
        for name in "${named[@]}"; do [[ $name == "$target" ]] && wanted=true; done
        $wanted || continue
    fi
    benches+=("${target#* }")
done
if ((${#benches[@]} == 0)); then
    echo "instructions.sh: --benchmarks names no benchmark in a target this counts" >&2
    exit 1
fi

cargo=(--locked)
features_list=()
for package in $(printf '%s\n' "${packages[@]}" | sort -u); do
    cargo+=(-p "$package")
    [[ -n ${features[$package]:-} ]] && features_list+=("$package/${features[$package]}")
done
((${#features_list[@]})) && cargo+=(--features "$(
    IFS=,
    echo "${features_list[*]}"
)")
for target in $(printf '%s\n' "${benches[@]}" | sort -u); do
    cargo+=(--bench "$target")
done

if [[ $command == compare ]]; then
    base=${1:-}
    if [[ -z $base || $base == -* ]]; then
        echo "instructions.sh: compare takes a base commit first; $usage" >&2
        exit 2
    fi
    shift
    exec "${bench[@]}" sim "$base" "$@" -- "${cargo[@]}"
fi
exec "${bench[@]}" sim "$@" -- "${cargo[@]}"
