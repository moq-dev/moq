#!/usr/bin/env bash
#
# Fixtures for the impact map. Every one of these is a diff shape that used to
# reach `main` with no end-to-end coverage at all, because the lane that covers
# it was selected by a path filter naming the harness rather than the source.
#
# Both directions are asserted for every lane. A selector that says yes to
# everything costs an hour a pull request; one that says no to everything is the
# hole this exists to close, and it is the one that stays green.

set -euo pipefail

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() {
    echo "select: $1" >&2
    exit 1
}

# `<lane>=<value>` for one changed-file list, memoised: each call runs
# `cargo metadata`, and the fixtures below ask about the same diffs repeatedly.
declare -A memo
select_for() {
    if [[ -z "${memo[$1]+set}" ]]; then
        memo[$1]="$(printf '%s' "$1" | "$scripts/select.sh")"
    fi
    printf '%s' "${memo[$1]}"
}

expect() {
    local files=$1 lane=$2 want=$3
    local got
    got="$(select_for "$files" | sed -n "s/^$lane=//p")"
    [[ -n "$got" ]] || fail "no lane named $lane"
    [[ "$got" == "$want" ]] || fail "$lane=$got for [${files//$'\n'/, }], expected $want"
}

# A wire change. The Cross-Package Sync rule in CLAUDE.md asks for the full
# matrix here, and the wide lane subsumes the narrow one.
wire='rs/moq-net/src/lib.rs'
expect "$wire" smoke_full true
expect "$wire" smoke false
expect "$wire" wasm true

# The FFI surface every non-Rust binding is generated from. Same rule.
ffi='rs/moq-ffi/src/lib.rs'
expect "$ffi" smoke_full true
expect "$ffi" smoke false

# The Go wrapper. Its client is a full-matrix-only participant, so the narrow set
# would prove nothing about it.
expect 'go/wrapper/moq/broadcast.go' smoke_full true
expect 'go/scripts/stage.sh' smoke_full true

# A browser player change: covered by the representative set, which publishes and
# subscribes from a real headless browser. Nothing here reaches wasm32.
watch='js/watch/src/element.ts'
expect "$watch" smoke true
expect "$watch" smoke_full false
expect "$watch" wasm false
expect "$watch" windows false

# The relay is the server every matrix cell connects through, but it is not the
# wire, the binding, or a gateway, so the narrow lane is the right width.
relay='rs/moq-relay/src/web.rs'
expect "$relay" smoke true
expect "$relay" smoke_full false

# The two lanes whose harness builds a relay and publishes from @moq/net. Neither
# fixture is in moq-wasm's Cargo dependency graph, so the closure alone says no
# and a break arriving through either would run nowhere.
expect "$relay" wasm true
expect 'js/net/src/connection.ts' wasm true

# A platform backend. moq-video holds `#[cfg(target_os = ...)]` capture and
# encode that no Linux job compiles, and it is a moq-cli dependency, so the
# delivery path it feeds is worth proving too.
backend='rs/moq-video/src/lib.rs'
expect "$backend" windows true
expect "$backend" macos true
expect "$backend" smoke true

# The relay holds macOS-gated code as well, but `just rs macos` does not compile
# it, so claiming the lane covers it would be a lie.
expect "$relay" macos false
expect "$relay" windows false

# A lockfile bump can change any crate's behavior, so every behavioral lane runs.
# The wide matrix does not: what the lockfile moved is a dependency, not the wire
# format, the binding, or a gateway.
lock='Cargo.lock'
expect "$lock" smoke true
expect "$lock" smoke_full false
expect "$lock" wasm true
expect "$lock" ts true
# Four extra workspace compiles for a bump that did not touch the feature graph.
expect "$lock" features false

# A manifest does touch the feature graph, and is the one place an optional
# dependency or a `#[cfg(feature)]` gate goes missing.
expect 'rs/moq-video/Cargo.toml' features true
expect 'rs/moq-relay/build.rs' features true

# The dev shell supplies ffmpeg, TSDuck, and the wasm-bindgen CLI whose version
# has to match the crate, and nothing in the Cargo or bun graph names it. A lock
# bump used to select no lane at all.
shell='flake.lock'
expect "$shell" smoke true
expect "$shell" wasm true
expect "$shell" ts true
# Both of these run on a runner-native toolchain, so nix never enters them.
expect "$shell" windows false
expect "$shell" macos false

# The bun workspace the wasm harness installs frozen and bundles its publisher
# out of. It reaches the native node and bun clients too, which only the wide
# matrix runs.
expect 'bun.lock' wasm true
expect 'bun.lock' smoke_full true

# test/wasm extends the shared compiler options, and run.sh's `tsc --noEmit` is
# the only thing that type-checks the harness against the generated @moq/wasm
# declarations: the workspace has no `check` script for `just js check` to run.
expect 'js/tsconfig.json' wasm true

# Docs cannot change behavior, and this is the case that must finish without
# waiting on a lane: it is why the aggregate exists.
docs='doc/concept/index.md'
for lane in smoke smoke_full wasm ts windows macos features; do
    expect "$docs" "$lane" false
done

# The gate machinery itself matches no lane's own inputs, so a pull request
# rewriting it would otherwise validate none of them.
expect '.github/scripts/select.sh' smoke true
expect '.github/scripts/select.sh' smoke_full true
expect '.github/scripts/select.sh' wasm true
expect 'test/justfile' smoke true
expect 'test/justfile' smoke_full true

# rs/justfile owns the platform and feature recipes, so a change to its command
# lines has to execute those recipes rather than merely widening the Cargo
# dependency closure.
expect 'rs/justfile' windows true
expect 'rs/justfile' macos true
expect 'rs/justfile' features true

# `just _changed` says ALL when the diff outgrew argv. Nothing is known about it,
# so nothing is assumed.
expect 'ALL' smoke_full true
expect 'ALL' features true

# For ordinary source changes the wide matrix subsumes the narrow one. Gate
# machinery and an unreasonably large diff deliberately run both, because the
# core recipe and workflow input are themselves behavior under test.
for files in "$wire" "$ffi"; do
    [[ "$(select_for "$files" | grep -c '^smoke\(_full\)\?=true$')" -eq 1 ]] ||
        fail "smoke and smoke_full must not both run for [$files]"
done
for files in 'ALL' 'test/justfile' '.github/scripts/select.sh'; do
    [[ "$(select_for "$files" | grep -c '^smoke\(_full\)\?=true$')" -eq 2 ]] ||
        fail "smoke and smoke_full must both run for [$files]"
done

# The aggregate catches a lane whose job is missing only once both are wired into
# the same run. A lane added here and nowhere else has no job to be missing.
gates="$scripts/../workflows/gates.yml"
while IFS='=' read -r lane _; do
    grep -qE "^  $lane:$" "$gates" ||
        fail "lane $lane has no job in gates.yml"
    grep -qE "^      $lane: \\\$\{\{ steps\..*\.outputs\.$lane \}\}$" "$gates" ||
        fail "lane $lane is not an output of the gates.yml selector job"
done < <(select_for "$docs")

echo "select: impact map ok"
