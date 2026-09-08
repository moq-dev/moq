#!/usr/bin/env bash
#
# The impact map: which behavioral gates a diff selects.
#
# `just check` and `just test` already answer "which packages did this change
# touch". This answers the other half: which end-to-end lane proves the changed
# behavior still works. Path filters in a workflow's `on:` block cannot, because
# a lane's real input is a crate's dependents, not a directory.
#
# Reads a newline-separated changed-file list on stdin, the same list
# `just _changed` prints, and writes one `<lane>=true|false` line per lane. Every
# lane is always printed, so the output doubles as the list of lanes and can be
# appended straight to $GITHUB_OUTPUT.
#
# Two different questions are asked of the diff, and the difference matters:
#
#   closure  the crates a change can reach, from `just rs _select`: the changed
#            crates plus everything depending on them. This is the right question
#            for a behavioral lane, because a moq-net edit breaks the relay
#            without touching a file under rs/moq-relay.
#   seeds    the crate directories the diff actually edited. This is the right
#            question for a lane whose cost is a whole extra runner and whose
#            yield is code in that crate: a dependency-side API break reaching
#            platform code is left to nightly.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

files="$(cat)"

# Every lane, in output order. Also the contract `gates.sh` checks the workflow
# against: a lane here with no job, or a job with no lane, fails the aggregate.
lanes=(smoke smoke_full wasm ts windows macos features)

declare -A selected
for lane in "${lanes[@]}"; do
    selected[$lane]=false
done

emit() {
    for lane in "${lanes[@]}"; do
        printf '%s=%s\n' "$lane" "${selected[$lane]}"
    done
}

everything() {
    for lane in "${lanes[@]}"; do
        selected[$lane]=true
    done
    # The two smoke lanes are the same harness at two widths, so the wide one
    # subsumes the narrow one; running both would pay for the small matrix twice.
    selected[smoke]=false
    emit
    exit 0
}

# `just _changed` says ALL when the file list outgrew what argv can carry. That
# is a diff too large to reason about, so every lane runs.
if [[ "$files" == ALL ]]; then
    everything
fi

# The gate machinery itself. A pull request that rewrites how lanes are selected
# matches no lane's own inputs, so without this it would validate none of them.
# Mirrors the root `justfile`'s "orchestration changed, check everything" rule.
if grep -qE '^(\.github/(justfile|scripts/(select|gates)(\.test)?\.sh|workflows/(gates|smoke|wasm)\.yml)|justfile|test/justfile)$' <<<"$files"; then
    everything
fi

# `_select` emits `--package <id>` flags, or the bare word ALL when the diff
# touched something workspace-wide (a manifest, the lockfile, the toolchain
# pin). `_names` turns the flags back into crate names.
packages="$(just --justfile "$root/justfile" --working-directory "$root" rs _select "$files")"
if [[ "$packages" == ALL ]]; then
    closure=ALL
else
    closure="$(just --justfile "$root/justfile" --working-directory "$root" rs _names "$packages")"
fi

seeds="$(sed -n 's|^rs/\([^/]*\)/.*|\1|p' <<<"$files" | sort -u)"

# True when the diff can reach any of the named crates through the dependency
# graph. ALL is workspace-wide, so it reaches everything.
reaches() {
    [[ "$closure" == ALL ]] && return 0
    grep -qwE "$(printf '%s|' "$@" | sed 's/|$//')" <<<"$closure"
}

# True when the diff edited any of the named crates directly.
edits() {
    grep -qxE "$(printf '%s|' "$@" | sed 's/|$//')" <<<"$seeds"
}

# True when the diff touched any of the given path patterns.
touches() {
    grep -qE "$1" <<<"$files"
}

# The dev shell every harness runs inside. It is where ffmpeg, TSDuck, and the
# wasm-bindgen CLI (whose version has to match the crate) come from, and the
# rust-cache key already rolls on it because build scripts link against its store
# paths. Nothing in the Cargo or bun graph names it, so without this a lock bump
# selects no lane at all.
shell='^flake\.(nix|lock)$'

# The full interop matrix, per the Cross-Package Sync rule in CLAUDE.md: wire,
# FFI, and gateway changes run every publisher against every subscriber.
#
# Seeds rather than the closure, deliberately. moq-ffi sits on top of most of the
# workspace, so "the diff can reach moq-ffi" is true for nearly every Rust change
# and would make the wide matrix the default lane. What the rule actually names
# is a change TO the wire, the binding, or a gateway.
#
# The python, Go, and GStreamer arms exist only here, so a change to any of those
# clients selects the wide matrix even though nothing else about it is wide.
if edits moq-net moq-ffi libmoq moq-gst moq-rtmp moq-srt moq-rtc moq-hls ||
    touches '^(py/|pyproject\.toml$|uv\.lock$)' ||
    touches '^go/' ||
    touches '^(test/smoke/|test/justfile$|package\.json$|bun\.lock$)'; then
    selected[smoke_full]=true
elif reaches moq-relay moq-cli libmoq moq-ffi moq-gst ||
    touches '^(js/|demo/web/)' || touches "$shell"; then
    # The representative set: rust and browser publish, rust, browser and C
    # subscribe. Every client here is built from a crate or package in the
    # closure above, so this covers the delivery path end to end at roughly a
    # third of the wide matrix's cost.
    #
    # The five crates are the matrix's entry points, not its whole dependency
    # set: `reaches` already walks dependents, so a kio or hang edit arrives here
    # as moq-relay and moq-cli.
    selected[smoke]=true
fi

# moq-wasm's crate root is `#![cfg(target_arch = "wasm32")]`, so every other gate
# compiles it to nothing and `just rs wasm` only compiles it. This lane is the
# only thing that runs it.
#
# moq-relay and js/net are the harness's fixtures rather than the code under
# test, and they are in anyway: test/wasm/run.sh builds a real relay and
# publishes from @moq/net, and neither is in moq-wasm's Cargo dependency graph,
# so a break arriving through either would otherwise reach `main` unrun. That is
# what the paths filter this replaced deliberately gave up (a browser on a large
# share of pull requests); the impact map buys it back, because those diffs run
# the smoke lane alongside rather than after.
#
# The bun workspace is here for the same reason: run.sh installs it frozen and
# bundles the publisher out of it, so the root manifest and lockfile decide what
# the harness actually loads.
if reaches moq-wasm moq-relay ||
    touches '^(js/(wasm|net|signals)/|test/wasm/|\.cargo/config\.toml$)' ||
    touches '^(package\.json|bun\.lock)$' || touches "$shell"; then
    selected[wasm]=true
fi

# The MPEG-TS exporter graded against a real analyzer. moq-mux owns the muxer and
# moq-cli owns the `export ts` that drives it; TSDuck grades the output and comes
# from the dev shell.
if reaches moq-mux moq-cli || touches '^test/ts/' || touches "$shell"; then
    selected[ts]=true
fi

# `#[cfg(target_os = ...)]` code no Linux job compiles at all. Seeds rather than
# the closure: these cost a whole extra runner on a throttled pool, and the code
# they cover changes when its own crate changes. rust-toolchain.toml is in
# because a compiler bump is the other way this code stops building.
#
# The dev shell is deliberately absent: these two jobs need an Apple or Windows
# host and use the runner's own toolchain, so nix never runs in them.
if edits moq-video moq-audio moq-nvenc moq-transcode moq-native moq-cli ||
    touches '^rust-toolchain\.toml$'; then
    selected[windows]=true
fi
if edits moq-video moq-audio || touches '^rust-toolchain\.toml$'; then
    selected[macos]=true
fi

# The feature permutations. Selected on the manifests and build scripts that
# define the feature graph, not on source: an optional dependency going missing
# or a `#[cfg(feature)]` arm losing its gate shows up when the wiring is edited.
# Cargo.lock alone is out, so a lockfile-only bump does not pay for four extra
# workspace compiles.
if touches '^(Cargo\.toml|rs/.*/(Cargo\.toml|build\.rs)|rust-toolchain\.toml)$'; then
    selected[features]=true
fi

emit
