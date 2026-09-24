#!/usr/bin/env bash
# Run `just rs check|fix|test` on the crates a diff touches, plus their dependents.
#
# Usage: sh/rs/select.sh check|fix|test LISTFILE|--all
#
# LISTFILE holds the changed paths, one per line. `--all` is the whole
# workspace, as is a change to anything every crate is built or tested by.
set -euo pipefail

usage="usage: sh/rs/select.sh check|fix|test LISTFILE|--all"
action=${1:?$usage}
list=${2:?$usage}

cd "$(git rev-parse --show-toplevel)"

# Print `ALL`, nothing (no crate affected), or cargo package ids.
select_packages() {
    if [[ "$list" == --all ]] || grep -qE '^(Cargo\.(toml|lock)|rust-toolchain\.toml|rs/justfile|sh/rs/select\.sh|\.config/nextest\.toml)$' "$list"; then
        echo ALL
        return
    fi

    grep -q '^rs/' "$list" || return 0

    # `--no-deps` keeps this to workspace members and off the network.
    local metadata
    metadata=$(cargo metadata --format-version 1 --no-deps)

    # A seed is the first path segment under `rs/`, so only a crate whose
    # manifest sits directly at `rs/<dir>/Cargo.toml` can be one. That drops
    # `moq-net-fuzz` (at `rs/moq-net/fuzz`), which a moq-net diff would
    # otherwise select as a dependent and compile libFuzzer for. The separator
    # is normalized first so Windows paths split too.
    metadata=$(jq '.packages |= map((.manifest_path |= gsub("\\\\"; "/")) | select(.manifest_path | split("/")[-3] == "rs"))' <<<"$metadata")

    # A changed path is a seed only because every crate directory is named
    # after its crate. Under-selecting checks nothing, so fall back to all.
    if jq -e '.packages[] | select((.manifest_path | split("/")[-2]) != .name)' <<<"$metadata" >/dev/null; then
        echo "rs: a crate directory no longer matches its crate name; selecting everything." >&2
        echo ALL
        return
    fi

    local seeds edges selected
    seeds=$(sed -n 's|^rs/\([^/]*\)/.*|\1|p' "$list" | sort -u)
    edges=$(jq -r '.packages[] | .name as $name | .dependencies[] | "\($name) \(.name)"' <<<"$metadata")

    # A crate is rebuilt when anything it depends on changed, so walk the edges
    # backwards until the selection stops growing. Test the value, not the key:
    # awk creates a key on every `want[dep[i]]` read. The id lookup below drops
    # names that are not workspace crates, such as a seed from `rs/CLAUDE.md`.
    selected=$(awk -v seeds="$seeds" '
		BEGIN { split(seeds, s, "\n"); for (i in s) want[s[i]] = 1 }
		{ pkg[NR] = $1; dep[NR] = $2 }
		END {
			do {
				grew = 0
				for (i = 1; i <= NR; i++)
					if (want[dep[i]] && !want[pkg[i]]) { want[pkg[i]] = 1; grew = 1 }
			} while (grew)
			for (name in want) if (want[name]) print name
		}
	' <<<"$edges" | sort -u)

    [[ -n "$selected" ]] || return 0

    # Cargo's own package ids, not bare names: a workspace crate that is also
    # pulled from crates.io (kio) makes `--package kio` ambiguous, and cargo
    # refuses before compiling anything.
    jq -r --arg selected "$selected" '
		($selected | split("\n")) as $want
		| .packages[]
		| select(.name as $name | $want | index($name) != null)
		| "--package \(.id)"
	' <<<"$metadata" | sort -u | tr '\n' ' '
}

packages=$(select_packages)

# Ids are `path+file://<dir>#<version>` for a directory named after its crate.
names=$(tr ' ' '\n' <<<"$packages" | sed -n 's|.*/\([^/#]*\)#.*|\1|p' | sort -u | tr '\n' ' ')

# True when the selection includes a crate matching the pattern.
wants() {
    [[ "$packages" == ALL ]] || grep -qwE "$1" <<<"$names"
}

case "$packages" in
    "")
        echo "rs: no crates affected; skipping."
        exit 0
        ;;
    ALL) echo "rs: selecting the workspace" ;;
    *) echo "rs: selecting $names" ;;
esac

# `moq-net-fuzz` needs a nightly toolchain; only `just rs fuzz` compiles it.
if [[ "$packages" == ALL ]]; then
    flags=(--workspace --exclude moq-net-fuzz)
else
    # Space-separated `--package <id>` pairs; ids contain no spaces.
    # shellcheck disable=SC2206
    flags=($packages)
fi

case "$action" in
    check)
        just rs check "${flags[@]}"
        # Workspace feature unification hides a broken moq-tokio feature set
        # whenever any dependent enables a transport.
        if wants moq-tokio; then just rs tokio-features; fi
        # moq-wasm's crate root is `#![cfg(target_arch = "wasm32")]`, so only a
        # wasm32 pass checks it. moq-mux and moq-ffi ride along in that pass, and
        # nothing depends on moq-ffi, so each has to reach it on its own.
        if wants '(moq-wasm|moq-mux|moq-ffi)'; then just rs wasm; fi
        # Device code behind the off-by-default `capture` feature.
        if wants '(moq-video|moq-audio)'; then just rs capture; fi
        # The relay's io_uring listener, off the default feature set.
        if wants moq-relay; then just rs uring-check; fi
        ;;
    fix)
        just rs fix "${flags[@]}"
        if wants '(moq-wasm|moq-mux|moq-ffi)'; then just rs wasm-fix; fi
        ;;
    test)
        # A selection can hold nothing testable (moq-wasm has no host tests), and
        # nextest exits 4 on that; the whole workspace finding none really is wrong.
        [[ "$packages" == ALL ]] || flags+=(--no-tests=pass)
        just rs test "${flags[@]}"
        if wants moq-relay; then just rs relay-minimal; fi
        ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac
