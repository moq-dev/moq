#!/usr/bin/env bash
# Run `just rs check|check-test|lint|fix|test` on the crates a diff touches,
# plus their dependents. `platform` is the Windows and macOS compile.
#
# Usage: sh/rs/select.sh check|check-test|lint|fix|test|platform|uring LISTFILE|--all
#
# LISTFILE holds the changed paths, one per line. `--all` is the whole
# workspace, as is a change to anything every crate is built or tested by.
set -euo pipefail

usage="usage: sh/rs/select.sh check|check-test|lint|fix|test|platform|uring LISTFILE|--all"
action=${1:?$usage}
list=${2:?$usage}

cd "$(git rev-parse --show-toplevel)"

# Print `ALL`, nothing (no crate affected), or cargo package ids.
select_packages() {
    if [[ "$action" == uring && -f "$list" ]] && grep -qE '^(justfile|flake\.(nix|lock)|sh/changed\.sh|\.github/workflows/uring\.yml|\.cargo/.*|\.github/actions/.*)$' "$list"; then
        echo ALL
        return
    fi
    if [[ "$list" == --all ]] || grep -qE '^(\.cargo/.*|sh/local\.(sh|ts)|Cargo\.(toml|lock)|rust-toolchain\.toml|rs/justfile|sh/rs/(select|platform|nextest|send-depth)\.sh|\.config/nextest\.toml)$' "$list"; then
        echo ALL
        return
    fi

    # `--no-deps` keeps this to workspace members and off the network.
    local metadata
    metadata=$(cargo metadata --format-version 1 --no-deps)

    # A seed is the first path segment under `rs/`, so only a crate whose
    # manifest sits directly at `rs/<dir>/Cargo.toml` can be one. That drops
    # the `*-fuzz` crates (at `rs/<crate>/fuzz`), which a diff to their crate would
    # otherwise select as a dependent and compile libFuzzer for. A demo's crate
    # sits at `demo/<demo>/<dir>/Cargo.toml` and is a seed for its directory.
    # The separator is normalized first so Windows paths split too.
    metadata=$(jq '.packages |= map((.manifest_path |= gsub("\\\\"; "/")) | select((.manifest_path | split("/")) as $p | $p[-3] == "rs" or $p[-4] == "demo"))' <<<"$metadata")

    # A changed path under `rs/` is a seed only because every crate directory
    # there is named after its crate. Under-selecting checks nothing, so fall
    # back to all.
    if jq -e '.packages[] | (.manifest_path | split("/")) as $p | select($p[-3] == "rs" and $p[-2] != .name)' <<<"$metadata" >/dev/null; then
        echo "rs: a crate directory no longer matches its crate name; selecting everything." >&2
        echo ALL
        return
    fi

    local seeds benches edges selected
    # A crate's `benches/` builds only its own bench targets, which no other crate can
    # depend on and the test build never compiles, so it selects that crate alone
    # (for its lint) rather than every dependent's tests.
    benches=$(sed -n 's|^rs/\([^/]*\)/benches/.*|\1|p' "$list")
    # A target can also sit outside its crate, as moq-cli's interop examples
    # under test/interop/clients/rust/ do, so a change in the directory of a
    # target's root file, which holds that file's modules, seeds its crate.
    # Cargo leaves the manifest's `..` in `src_path`, so they are resolved here.
    # sh/dispatch.sh's `rs` scope must match those directories too; the
    # selector's tests check that.
    seeds=$({
        sed -n '\|^rs/[^/]*/benches/|d; s|^rs/\([^/]*\)/.*|\1|p' "$list"
        # moq-relay's tests read its NixOS module.
        sed -n 's|^nix/modules/moq-relay\.nix$|moq-relay|p' "$list"
        jq -r --rawfile list "$list" '
            def resolve: gsub("\\\\"; "/") | reduce split("/")[] as $s ([];
                if $s == ".." then .[:-1] elif $s == "." then . else . + [$s] end) | join("/");
            ($list | split("\n") | map(select(. != ""))) as $paths
            | ((.workspace_root | resolve) + "/") as $root
            | .packages[]
            | (.manifest_path | split("/")) as $p
            | ($p[:-1] | join("/") + "/") as $crate
            | select(($p[-4] == "demo" and any($paths[]; startswith("demo/\($p[-3])/\($p[-2])/")))
                or any(.targets[].src_path | resolve | select(startswith($crate) | not)
                    | ltrimstr($root) | sub("[^/]*$"; ""); . as $dir | any($paths[]; startswith($dir))))
            | .name' <<<"$metadata"
    } | sort -u)
    edges=$(jq -r '.packages[] | .name as $name | .dependencies[] | "\($name) \(.name)"' <<<"$metadata")

    # A crate is rebuilt when anything it depends on changed, so walk the edges
    # backwards until the selection stops growing. Test the value, not the key:
    # awk creates a key on every `want[dep[i]]` read. The id lookup below drops
    # names that are not workspace crates, such as a seed from `rs/AGENTS.md`.
    # The seeds go through the environment because BSD awk (macOS) rejects a
    # newline in a `-v` value.
    selected=$(SEEDS="$seeds" awk '
		BEGIN { split(ENVIRON["SEEDS"], s, "\n"); for (i in s) want[s[i]] = 1 }
		{ pkg[NR] = $1; dep[NR] = $2 }
		END {
			do {
				grew = 0
				for (i = 1; i <= NR; i++)
					if (want[dep[i]] && !want[pkg[i]]) { want[pkg[i]] = 1; grew = 1 }
			} while (grew)
			for (name in want) if (want[name]) print name
		}
	' <<<"$edges")
    selected=$(printf '%s\n%s\n' "$selected" "$benches" | sed '/^$/d' | sort -u)

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

# Selection and empty crate sets stay outside admission. The paths are
# root-relative because the `cd` above invalidates a relative `$0`.
if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash sh/local.sh bash sh/rs/select.sh "$@"
fi

# The `*-fuzz` crates need a nightly toolchain; only `just rs fuzz` compiles them.
if [[ "$packages" == ALL ]]; then
    flags=(--workspace --exclude moq-net-fuzz --exclude moq-quic-fuzz)
else
    # Space-separated `--package <id>` pairs; ids contain no spaces.
    # shellcheck disable=SC2206
    flags=($packages)
fi

case "$action" in
    uring)
        if wants '(moq-uring|moq-relay)'; then just rs uring; fi
        ;;
    check | check-test | lint)
        # The isolated feature builds (`just rs features`) stay out of the
        # gate; the Features workflow runs them nightly and on release PRs.
        just rs "$action" "${flags[@]}"
        if wants '(kio|moq-net)'; then just rs loom-check; fi
        # Native Send proofs at the default recursion limit, on the pinned nightly.
        if wants moq-net; then just rs send-depth; fi
        # Device code behind the off-by-default `capture` feature. check-test
        # lints it through the test build, as for the default features.
        if [[ "$action" != lint ]] && wants '(moq-video|moq-audio|moq-scene|moq-studio)'; then
            if [[ "$action" == check-test ]]; then just rs capture-test; else just rs capture; fi
        fi
        # moq-cli's media tasks are behind `play`; only check-test runs tests.
        if [[ "$action" == check-test ]] && wants moq-cli; then just rs play; fi
        ;;
    fix)
        just rs fix "${flags[@]}"
        if wants '(moq-wasm|moq-mux|moq-ffi|moq-clock)'; then just rs wasm-fix; fi
        ;;
    test)
        # A selection can hold nothing testable, and
        # nextest exits 4 on that; the whole workspace finding none really is wrong.
        [[ "$packages" == ALL ]] || flags+=(--no-tests=pass)
        # CI keeps Cargo's per-crate build timings, which check.yml uploads.
        timings=()
        [[ "${GITHUB_ACTIONS:-}" != true ]] || timings=(--timings)
        # The tests behind the off-by-default device and runtime features join this build
        # rather than compiling the workspace again per feature set. Those
        # features only add code, so every test still runs. Cargo rejects a
        # feature of a package outside the selection, hence the explicit picks.
        features=()
        if wants '(moq-video|moq-audio)'; then
            [[ "$packages" == ALL ]] || flags+=(--package moq-video --package moq-audio)
            features+=(moq-video/capture moq-audio/capture moq-audio/playback)
        fi
        if wants moq-cli; then features+=(moq-cli/play moq-cli/openh264); fi
        # The WebRTC module's hostile-input tests, such as mDNS backpressure.
        if wants moq-tokio; then features+=(moq-tokio/webrtc); fi
        # The relay's io_uring listener, Linux-only. A moq-uring change selects
        # the relay too, as its dependent.
        if [[ "$(uname -s)" == Linux ]] && wants moq-relay; then features+=(moq-relay/io-uring); fi
        if ((${#features[@]})); then flags+=(--features "${features[*]}"); fi
        just rs test "${flags[@]}" ${timings[@]+"${timings[@]}"}
        ;;
    platform)
        # moq-gst links GStreamer via pkg-config, which the runners don't have.
        if [[ "$packages" == ALL ]]; then
            flags+=(--exclude moq-gst)
        else
            flags=()
            for id in $packages; do
                [[ "$id" == --package || "$id" == */moq-gst#* ]] || flags+=(--package "$id")
            done
            if ((${#flags[@]} == 0)); then
                echo "rs: no crates affected; skipping."
                exit 0
            fi
        fi
        # `play` and `capture` are off by default and turn on the device and
        # render code in moq-video, moq-audio, and the cli. Only when moq-cli is
        # selected, since cargo rejects a feature of a package outside the
        # selection; a change to anything they reach selects moq-cli anyway.
        if wants moq-cli; then flags+=(--features "moq-cli/play moq-cli/capture"); fi
        # Apple UDP batching is opt-in; compile its implementation and tests on macOS.
        if [[ "$(uname -s)" == Darwin ]] && wants moq-sock; then
            flags+=(--features moq-sock/fast-apple-datapath)
        fi
        cargo check --locked "${flags[@]}" --all-targets
        ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac
