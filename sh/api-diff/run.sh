#!/usr/bin/env bash
# Diff every published package's public API against its last published release:
# Rust crates through cargo-semver-checks, `@moq/*` packages through API
# Extractor reports, and the FFI interfaces (`moq.h` and moq-ffi's UniFFI
# surface) as headers. Writes target/api-diff/report.md and prints it.
#
# usage: sh/api-diff/run.sh [--deny] [rust|js|ffi]...
#
# --deny exits 1 on any break, for backports into `release`. Without it the run
# only reports, as the cut PR does: release-plz bumps Rust versions after the cut
# lands, so a break there is expected and needs a MIGRATE.md entry instead.
# For JS and FFI, any removed or changed line is a break; additions are not.
set -euo pipefail

usage="usage: sh/api-diff/run.sh [--deny] [rust|js|ffi]..."
deny=
backends=()
for arg in "$@"; do
    case "$arg" in
        --deny) deny=1 ;;
        rust | js | ffi) backends+=("$arg") ;;
        *)
            echo "$usage" >&2
            exit 2
            ;;
    esac
done
((${#backends[@]})) || backends=(rust js ffi)

cd "$(git rev-parse --show-toplevel)"
root=$PWD
out=$root/target/api-diff
rm -rf "$out"
mkdir -p "$out"

summary=$out/summary.md
details=$out/details.md
printf '| Surface | Baseline | Result |\n| --- | --- | --- |\n' >"$summary"
: >"$details"
failed=

# Record one surface's row, and its diff when it changed.
row() {
    local surface=$1 baseline=$2 result=$3 body=${4:-} lang=${5:-diff}
    printf '| %s | %s | %s |\n' "$surface" "$baseline" "$result" >>"$summary"
    [[ -n "$body" ]] || return 0
    printf '\n### %s\n\n%s%s\n%s\n%s\n' "$surface" '```' "$lang" "$body" '```' >>"$details"
}

# Diff a baseline file against its current one. A removed line, which includes a
# changed one, is a break; only added lines are an addition.
compare() {
    local surface=$1 baseline=$2 old=$3 new=$4 body
    if [[ ! -f "$new" ]]; then
        row "$surface" "$baseline" break "(removed)"
        return
    fi
    if [[ ! -f "$old" ]]; then
        row "$surface" "$baseline" new
        return
    fi
    # Exit 1 means the files differ; anything above that is an error.
    body=$(git diff --no-index --no-color --no-prefix -U3 "$old" "$new" | tail -n +5) || [[ $? == 1 ]]
    if [[ -z "$body" ]]; then
        row "$surface" "$baseline" unchanged
    elif grep -q '^-' <<<"$body"; then
        row "$surface" "$baseline" break "$body"
    else
        row "$surface" "$baseline" additions "$body"
    fi
}

# cargo-semver-checks over every published library crate, against the newest
# crates.io release no newer than the crate's own version (its default).
# `--release-type minor` holds every crate to "no breaking change" whatever its
# version says, since the versions are bumped only after a cut.
rust() {
    local name kinds status log baseline
    # Share one target directory across crates: cargo-semver-checks otherwise
    # builds every baseline and current crate's dependencies from scratch.
    export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
    mkdir -p "$out/rust"
    while read -r name kinds; do
        # A binary or a C library has no Rust API; moq-c and moq-ffi are diffed as FFI.
        [[ ",$kinds," == *,lib,* || ",$kinds," == *,rlib,* ]] || continue
        status=$(curl -sS -o /dev/null -w '%{http_code}' "https://index.crates.io/${name:0:2}/${name:2:2}/$name")
        case "$status" in
            200) ;;
            404)
                row "$name" unpublished new
                continue
                ;;
            *)
                echo "crates.io index returned $status for $name" >&2
                exit 1
                ;;
        esac

        echo "api-diff: rust $name" >&2
        log=$out/rust/$name.log
        status=0
        cargo semver-checks check-release --color never --release-type minor -p "$name" </dev/null >"$log" 2>&1 || status=$?
        # "Checking kio v0.6.2 -> v0.6.3 (minor change)" names the baseline first.
        baseline=$(sed -nE 's/^ *Checking [^ ]+ v([^ ]+) -> .*/\1/p' "$log")
        case "$status" in
            0) row "$name" "$baseline" unchanged ;;
            # Keep each failed lint's title and the items it failed on, with
            # paths relative to the checkout or the crates.io source.
            100)
                row "$name" "$baseline" break "$(grep -vE '^ *(Building|Built|Parsing|Parsed|Checked|Finished) |^mbx' "$log" |
                    sed -E "/^Description:$/,/^ +impl: /d; s|$root/||g; s|[^ ]*/registry/src/[^/]+/||g")" text
                ;;
            *)
                cat "$log" >&2
                echo "cargo semver-checks failed on $name with exit code $status" >&2
                exit 1
                ;;
        esac
    done < <(cargo metadata --locked --no-deps --format-version 1 |
        jq -r '.packages[] | select(.publish != []) | "\(.name) \([.targets[].kind[]] | unique | join(","))"')
}

js() {
    # Builds every package's dist/ the way a publish does.
    just js build
    # It writes a failed entrypoint's error as its report, so the diff shows it.
    bun sh/api-diff/js.ts "$out/js" || failed=1
    local name version file entry surface
    while IFS=$'\t' read -r name version; do
        if [[ "$version" == - ]]; then
            row "$name" unpublished new
            continue
        fi
        while read -r file; do
            entry=${file%.api.md}
            surface=$name/$entry
            [[ "$entry" != index ]] || surface=$name
            compare "$surface" "$version" "$out/js/baseline/$name/$file" "$out/js/current/$name/$file"
        done < <(find "$out/js/baseline/$name" "$out/js/current/$name" -name '*.api.md' -printf '%f\n' | sort -u)
    done <"$out/js/versions.tsv"
}

ffi() {
    sh/api-diff/ffi.sh "$out/ffi"
    local surface baseline file
    while IFS=$'\t' read -r surface baseline file; do
        compare "$surface" "$baseline" "$out/ffi/baseline/$file" "$out/ffi/current/$file"
    done <"$out/ffi/surfaces.tsv"
}

for backend in "${backends[@]}"; do
    echo "api-diff: $backend" >&2
    "$backend"
done

{
    echo "# Public API diff"
    echo
    echo "Each surface against its last published release. A break needs a MIGRATE.md entry; a backport must have none."
    echo
    cat "$summary"
    if [[ -s "$details" ]]; then
        echo
        echo "## Changes"
        cat "$details"
    fi
} >"$out/report.md"
cat "$out/report.md"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    # The job summary holds 1 MiB; a cut's full report can exceed it.
    if (($(wc -c <"$out/report.md") < 1000000)); then
        cat "$out/report.md" >>"$GITHUB_STEP_SUMMARY"
    else
        { cat "$summary" && echo && echo "The full report is in the api-diff artifact."; } >>"$GITHUB_STEP_SUMMARY"
    fi
fi

if [[ -n "$failed" ]]; then
    echo "api-diff: a surface could not be extracted; see the report" >&2
    exit 1
fi
if [[ -n "$deny" ]] && grep -q '| break |$' "$summary"; then
    echo "api-diff: a published API changed; backports must keep it" >&2
    exit 1
fi
