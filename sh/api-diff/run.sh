#!/usr/bin/env bash
# Diff every published package's public API against its last release: Rust
# crates through cargo-semver-checks against crates.io, `@moq/*` packages
# through their committed API Extractor reports against `release`'s (or
# API_DIFF_BASE's), and the FFI interfaces (`moq.h` and moq-ffi's UniFFI
# surface) as headers against the released archives. Writes
# target/api-diff/report.md and prints it.
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
base=${API_DIFF_BASE:-origin/release}
rm -rf "$out"
mkdir -p "$out"

summary=$out/summary.md
details=$out/details.md
printf '| Surface | Baseline | Result |\n| --- | --- | --- |\n' >"$summary"
: >"$details"

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

# cargo-semver-checks over every published library crate, against its newest
# crates.io release, with every feature on. `--release-type minor` holds every
# crate to "no breaking change" whatever its version says, since the versions
# are bumped only after a cut.
#
# Both sides are documented as rustdoc JSON in one build each, sharing their
# dependencies: cargo-semver-checks would otherwise build each crate, baseline
# and current, in a workspace of its own and compile the dependencies again.
rust() {
    local name kinds lib id path index version features status log target
    local crates=() libs=() versions=() current=() baseline=()
    local -A released=()
    local manifest=$out/rust/baseline/Cargo.toml
    mkdir -p "$out/rust/baseline/src"
    : >"$out/rust/baseline/src/lib.rs"
    # Its own workspace, so the repository's does not claim it.
    printf '[package]\nname = "api-diff-baseline"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n\n[workspace]\n\n[dependencies]\n' >"$manifest"

    while read -r name kinds lib id; do
        # A binary or a C library has no Rust API; moq-c and moq-ffi are diffed as FFI.
        [[ ",$kinds," == *,lib,* || ",$kinds," == *,rlib,* ]] || continue
        # The sparse index shards names by length: 3/k/kio, ha/ng/hang.
        case ${#name} in
            1 | 2) path=${#name}/$name ;;
            3) path=3/${name:0:1}/$name ;;
            *) path=${name:0:2}/${name:2:2}/$name ;;
        esac
        index=$out/rust/$name.index
        status=$(curl -sS -o "$index" -w '%{http_code}' "https://index.crates.io/$path")
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
        # The highest release that is neither yanked nor a pre-release, and its features.
        version=$(jq -rs '[.[] | select((.yanked | not) and (.vers | contains("-") | not))]
            | max_by(.vers | split(".") | map(tonumber)) | .vers' "$index")
        features=$(jq -cs --arg v "$version" '.[] | select(.vers == $v) | (.features // {}) + (.features2 // {}) | keys' "$index")
        printf '%s = { version = "=%s", features = %s }\n' "$name" "$version" "$features" >>"$manifest"
        crates+=("$name")
        libs+=("$lib")
        versions+=("$version")
        current+=(-p "$id")
    done < <(cargo metadata --locked --no-deps --format-version 1 |
        jq -r '.packages[] | select(.publish != []) |
            "\(.name) \([.targets[].kind[]] | unique | join(",")) \([.targets[] | select(.kind | index("lib") or index("rlib")) | .name][0] // "-") \(.id)"')

    # The baseline resolves its own lockfile, as a consumer of those releases
    # would. A release without a library, or with another name for it, shows here.
    while read -r name version lib; do
        released["$name $version"]=$lib
    done < <(cargo metadata --manifest-path "$manifest" --format-version 1 |
        jq -r '.packages[] | select(.source != null) |
            "\(.name) \(.version) \([.targets[] | select(.kind | index("lib") or index("rlib")) | .name][0] // "-")"')
    for i in "${!crates[@]}"; do
        if [[ "${released["${crates[$i]} ${versions[$i]}"]:--}" == - ]]; then
            row "${crates[$i]}" "${versions[$i]} (no library)" new
            unset "crates[$i]"
        else
            baseline+=(-p "${crates[$i]}@${versions[$i]}")
        fi
    done

    export RUSTC_BOOTSTRAP=1 RUSTDOCFLAGS="-Z unstable-options --output-format json"
    target=$(cargo metadata --locked --no-deps --format-version 1 | jq -r .target_directory)
    cargo doc --locked --no-deps --all-features "${current[@]}"
    CARGO_TARGET_DIR=$target/api-diff-baseline cargo doc --manifest-path "$manifest" --no-deps "${baseline[@]}"
    unset RUSTC_BOOTSTRAP RUSTDOCFLAGS

    for i in "${!crates[@]}"; do
        name=${crates[$i]}
        lib=${released["$name ${versions[$i]}"]}
        log=$out/rust/$name.log
        status=0
        cargo semver-checks check-release --color never --release-type minor -p "$name" \
            --baseline-rustdoc "$target/api-diff-baseline/doc/${lib//-/_}.json" \
            --current-rustdoc "$target/doc/${libs[$i]//-/_}.json" >"$log" 2>&1 || status=$?
        case "$status" in
            0) row "$name" "${versions[$i]}" unchanged ;;
            # Keep each failed lint's title and the items it failed on, with
            # paths relative to the checkout or the crates.io source.
            100)
                row "$name" "${versions[$i]}" break "$(grep -vE '^ *(Parsing|Parsed|Checked|Finished) ' "$log" |
                    sed -E "/^Description:$/,/^ +impl: /d; s|$root/||g; s|[^ ]*/registry/src/[^/]+/||g")" text
                ;;
            *)
                cat "$log" >&2
                echo "cargo semver-checks failed on $name with exit code $status" >&2
                exit 1
                ;;
        esac
    done
}

# Each published `@moq/*` package's committed API Extractor reports against the
# base branch's. The build checks the reports against the code on every PR, so
# neither side is rebuilt here.
js() {
    git rev-parse --verify --quiet "$base^{commit}" >/dev/null || {
        echo "api-diff: $base does not exist; fetch it or set API_DIFF_BASE" >&2
        exit 1
    }
    local manifest dir name version file entry surface
    for manifest in js/*/package.json; do
        jq -e .scripts.release "$manifest" >/dev/null || continue
        dir=${manifest%/package.json}
        name=$(jq -r .name "$manifest")
        version=$(git show "$base:$manifest" 2>/dev/null | jq -r .version) || version=-
        mkdir -p "$out/js/$name"
        while read -r file; do
            git show "$base:$dir/api/$file" >"$out/js/$name/$file"
        done < <(git ls-tree --name-only "$base" "$dir/api/" | sed 's|.*/||')
        while read -r file; do
            entry=${file%.api.md}
            surface=$name/$entry
            [[ "$entry" != index ]] || surface=$name
            compare "$surface" "$version" "$out/js/$name/$file" "$dir/api/$file"
        done < <(find "$out/js/$name" "$dir/api" -name '*.api.md' -printf '%f\n' 2>/dev/null | sort -u)
    done
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

if [[ -n "$deny" ]] && grep -q '| break |$' "$summary"; then
    echo "api-diff: a published API changed; backports must keep it" >&2
    exit 1
fi
