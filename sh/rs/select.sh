#!/usr/bin/env bash
# Run `just rs check|check-test|lint|fix|test` on the crates a diff touches,
# plus their dependents. `platform` is the Windows and macOS compile on those
# hosts, and `cross` lints for both from any host.
#
# Usage: sh/rs/select.sh check|check-test|lint|fix|test|platform|cross|uring LISTFILE|--all
#
# LISTFILE holds the changed paths, one per line. `--all` is the whole
# workspace, as is a change to anything every crate is built or tested by.
set -euo pipefail

usage="usage: sh/rs/select.sh check|check-test|lint|fix|test|platform|cross|uring LISTFILE|--all"
action=${1:?$usage}
list=${2:?$usage}

cd "$(git rev-parse --show-toplevel)"

# Print `ALL`, nothing (no crate affected), or cargo package ids.
select_packages() {
    if [[ "$action" == uring && -f "$list" ]] && grep -qE '^(justfile|flake\.(nix|lock)|sh/changed\.sh|\.github/workflows/uring\.yml|\.cargo/.*|\.github/actions/.*)$' "$list"; then
        echo ALL
        return
    fi
    # The flake pins the cross toolchain and zig.
    if [[ "$action" == cross && -f "$list" ]] && grep -qE '^flake\.(nix|lock)$' "$list"; then
        echo ALL
        return
    fi
    if [[ "$list" == --all ]] || grep -qE '^(\.cargo/.*|sh/local\.(sh|ts)|Cargo\.(toml|lock)|clippy\.toml|rust-toolchain\.toml|rs/justfile|sh/rs/(select|platform|cross-lint|zig-(cc|c\+\+|ar)|nextest)\.sh|\.config/(nextest|hakari)\.toml)$' "$list"; then
        echo ALL
        return
    fi

    # `--no-deps` keeps this to workspace members and off the network.
    local metadata
    metadata=$(cargo metadata --format-version 1 --no-deps)

    # A seed is the first path segment under `rs/`, so only a crate whose
    # manifest sits directly at `rs/<dir>/Cargo.toml` can be one. That drops
    # the `*-fuzz` crates (at `rs/<crate>/fuzz`), which a diff to their crate would
    # otherwise select as a dependent and compile libFuzzer for.
    # The separator is normalized first so Windows paths split too.
    metadata=$(jq '.packages |= map((.manifest_path |= gsub("\\\\"; "/")) | select((.manifest_path | split("/")) as $p | $p[-3] == "rs"))' <<<"$metadata")

    # A changed path under `rs/` is a seed only because every crate directory
    # there is named after its crate. Under-selecting checks nothing, so fall
    # back to all.
    local mismatched
    mismatched=$(jq 'any(.packages[]; (.manifest_path | split("/")) as $p | $p[-3] == "rs" and $p[-2] != .name)' <<<"$metadata")
    if [[ "$mismatched" == true ]]; then
        echo "rs: a crate directory no longer matches its crate name; selecting everything." >&2
        echo ALL
        return
    fi

    local seeds targets readers edges selected
    # A crate's `benches/`, `tests/`, and `examples/` build only its own
    # targets, which no other crate can depend on, so a change there selects
    # that crate alone rather than every dependent, unless its library reads
    # the file (sh/rs/reads.sh lists those reads too).
    # A crate that reads a file outside itself, such as a fixture, a script its
    # tests run, or another crate's bench module, is a seed when that file
    # changes, or only a target change when the reading source is one of its
    # targets. sh/rs/reads.sh lists the reads as globs, made regexes here. A
    # read is followed through the files it reads: a change to a module that an
    # included module includes in turn reaches the crate at the top.
    readers=$(bash sh/rs/reads.sh | jq -nrR --rawfile list "$list" '
        def escape: gsub("(?<c>[.+?^$()\\[\\]{}|\\\\])"; "\\\(.c)");
        def regex: split("**/") | map(split("**") | map(split("*") | map(escape) | join("[^/]*")) | join(".*"))
            | "^\(join("(.*/)?"))(/.*)?$";
        [inputs | capture("^rs/(?<crate>[^/ ]+) (?<glob>[^ ]*) (?<file>.*):[0-9]+$") | .regex = (.glob | regex)] as $reads
        | def hit($paths): [$reads[] | select(.regex as $regex | any($paths[]; test($regex)))];
          def changed: (. + [hit(.)[].file] | unique) as $more | if $more == . then . else $more | changed end;
        ($list | split("\n") | map(select(. != "")) | unique | changed) as $paths
        | hit($paths)[]
        | "\(.crate) \(if .file | test("^rs/[^/]+/(benches|tests|examples)/") then "target" else "library" end)"')
    targets=$({
        sed -nE 's#^rs/([^/]*)/(benches|tests|examples)/.*#\1#p' "$list"
        sed -n 's/ target$//p' <<<"$readers"
    } | sort -u)
    # A target can also sit outside its crate, as moq-interop's clients
    # under test/interop/clients/rust/ do, so a change in the directory of a
    # target's root file, which holds that file's modules, seeds its crate.
    # Cargo leaves the manifest's `..` in `src_path`, so they are resolved here.
    # sh/dispatch.sh's `rs` scope must match those directories too; the
    # selector's tests check that.
    seeds=$({
        sed -nE '\#^rs/[^/]*/(benches|tests|examples)/#d; s#^rs/([^/]*)/.*#\1#p' "$list"
        sed -n 's/ library$//p' <<<"$readers"
        # The gate's Send-depth proof of moq-net's session driver is this script.
        sed -n 's|^sh/rs/send-depth\.sh$|moq-net|p' "$list"
        # The gate compiles the C fixtures and doc samples against libmoq's header.
        sed -nE 's#^(sh/rs/c-tests\.sh|doc/lib/c/.*|doc/lib/samples\.sh)$#libmoq#p' "$list"
        jq -r --rawfile list "$list" '
            def resolve: gsub("\\\\"; "/") | reduce split("/")[] as $s ([];
                if $s == ".." then .[:-1] elif $s == "." then . else . + [$s] end) | join("/");
            ($list | split("\n") | map(select(. != ""))) as $paths
            | ((.workspace_root | resolve) + "/") as $root
            | .packages[]
            | (.manifest_path | split("/")) as $p
            | ($p[:-1] | join("/") + "/") as $crate
            | select(any(.targets[].src_path | resolve | select(startswith($crate) | not)
                    | ltrimstr($root) | sub("[^/]*$"; ""); . as $dir | any($paths[]; startswith($dir))))
            | .name' <<<"$metadata"
    } | sort -u)
    edges=$(jq -r '.packages[] | .name as $name | .dependencies[] | "\($name) \(.name) \(.kind // "normal")"' <<<"$metadata")

    # A library is rebuilt when a normal or build dependency changed, so walk
    # those edges backwards until the selection stops growing. A dev-dependency
    # reaches only the tests of the crate that takes it, whose library and
    # dependents stay as they were, so it selects that crate and stops there.
    # Test the value, not the key: awk creates a key on every `want[dep[i]]`
    # read. The id lookup below drops names that are not workspace crates, such
    # as a seed from `rs/AGENTS.md`. The seeds go through the environment
    # because BSD awk (macOS) rejects a newline in a `-v` value.
    selected=$(SEEDS="$seeds" awk '
		BEGIN { split(ENVIRON["SEEDS"], s, "\n"); for (i in s) want[s[i]] = 1 }
		{ pkg[NR] = $1; dep[NR] = $2; dev[NR] = $3 == "dev" }
		END {
			do {
				grew = 0
				for (i = 1; i <= NR; i++)
					if (!dev[i] && want[dep[i]] && !want[pkg[i]]) { want[pkg[i]] = 1; grew = 1 }
			} while (grew)
			for (i = 1; i <= NR; i++) if (dev[i] && want[dep[i]]) tests[pkg[i]] = 1
			for (name in want) if (want[name]) print name
			for (name in tests) print name
		}
	' <<<"$edges")
    selected=$(printf '%s\n%s\n' "$selected" "$targets" | sed '/^$/d' | sort -u)

    [[ -n "$selected" ]] || return 0

    # Cargo's own package ids, not bare names: a workspace crate that is also
    # pulled from crates.io makes `--package <name>` ambiguous, and cargo
    # refuses before compiling anything.
    local pkg_flags
    pkg_flags=$(jq -r --arg selected "$selected" '
		($selected | split("\n")) as $want
		| .packages[]
		| select(.name as $name | $want | index($name) != null)
		| "--package \(.id)"
	' <<<"$metadata" | sort -u | tr '\n' ' ')
    [[ -n "$pkg_flags" ]] || return 0
    printf '%s\n%s\n' "$pkg_flags" "$(printf '%s\n' "$seeds" | tr '\n' ' ')"
}

# A command substitution clears -e, so a failing jq inside selection would
# otherwise print a partial selection. Restore it there; Bash 4.4's
# `inherit_errexit` would too, but macOS ships Bash 3.2.
selection=$(
    set -e
    select_packages
)
packages=${selection%%$'\n'*}
if [[ "$selection" == *$'\n'* ]]; then
    seed_names=${selection#*$'\n'}
else
    seed_names=
fi

# Ids are `path+file://<dir>#<version>` for a directory named after its crate.
names=$(tr ' ' '\n' <<<"$packages" | sed -n 's|.*/\([^/#]*\)#.*|\1|p' | sort -u | tr '\n' ' ')

# True when the selection includes a crate matching the pattern.
wants() {
    [[ "$packages" == ALL ]] || grep -qwE "$1" <<<"$names"
}

# True when the diff directly seeded a crate matching the pattern (or selected ALL).
seeded() {
    [[ "$packages" == ALL ]] || grep -qwE "$1" <<<"$seed_names"
}

# The `package/feature` pairs named by `required-features` on the targets of
# the crates in $1 (every non-fuzz crate under ALL). Cargo skips a target whose
# required features are off without a word. $2 is `tests` for tested targets
# (`.test`), `benches-examples` for benches and examples, or `all` for
# `--all-targets`.
required_features() {
    cargo metadata --locked --format-version 1 --no-deps | jq -r --arg names "$1" --arg all "$packages" --arg which "$2" '
        ($names | split(" ") | map(select(. != ""))) as $want
        | .packages[]
        | select(if $all == "ALL" then (.name | endswith("-fuzz") | not) else (.name as $n | $want | index($n) != null) end)
        | .name as $name
        | .targets[]
        | select(
            $which == "all"
            or ($which == "tests" and .test)
            or ($which == "benches-examples" and (.kind // [] | any(. == "bench" or . == "example")))
        )
        | (.["required-features"] // [])[]
        | "\($name)/\(.)"' | LC_ALL=C sort -u | tr '\n' ' '
}

# Prints `true` when any selected crate in $1 (or any non-fuzz crate under ALL)
# defines a bench or example, else `false`.
has_benches_or_examples() {
    cargo metadata --locked --format-version 1 --no-deps | jq -r --arg names "$1" --arg all "$packages" '
        ($names | split(" ") | map(select(. != ""))) as $want
        | any(
            .packages[];
            (if $all == "ALL" then (.name | endswith("-fuzz") | not) else (.name as $n | $want | index($n) != null) end)
            and any(.targets[]; .kind // [] | any(. == "bench" or . == "example"))
        )'
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
    # Assigned apart so a failure stops the script.
    fuzz_crates=$(bash sh/rs/no-fuzz.sh)
    flags=(--workspace)
    for fuzz in $fuzz_crates; do flags+=(--exclude "$fuzz"); done
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
        # The session driver's Send proof runs on nightly in its own target dir
        # (target/send-depth), so overlap it with the stable lint steps and
        # print its buffered output once it finishes.
        send_depth_pid=
        send_depth_log=
        trap 'rm -f "${send_depth_log:-}"' EXIT
        if [[ "$action" == lint ]] && wants '(moq-net|moq-tokio)'; then
            send_depth_log=$(mktemp)
            just rs send-depth >"$send_depth_log" 2>&1 &
            send_depth_pid=$!
        fi

        fail=0
        # The isolated feature builds (`just rs features`) stay out of the
        # gate; the Features workflow runs them nightly and on release PRs.
        just rs "$action" "${flags[@]}" || fail=1
        # Lint benches and examples in check mode (no codegen or linking) with
        # their required features, since `test` runs nextest on `--tests` alone.
        if [[ "$fail" -eq 0 && ("$action" == lint || "$action" == check-test) ]]; then
            has_targets=$(
                set -e
                has_benches_or_examples "$names"
            ) || fail=1
            if [[ "$fail" -eq 0 && "$has_targets" == true ]]; then
                target_flags=("${flags[@]}")
                required=$(
                    set -e
                    required_features "$names" benches-examples
                ) || fail=1
                if [[ "$fail" -eq 0 ]]; then
                    features=()
                    for feature in $required; do
                        [[ " ${features[*]-} " == *" $feature "* ]] || features+=("$feature")
                    done
                    if ((${#features[@]})); then target_flags+=(--features "${features[*]}"); fi
                    just rs lint-targets "${target_flags[@]}" || fail=1
                fi
            fi
        fi
        # The test build enables device features, which can hide unused items
        # in the default-feature test cfg. Keep this compile isolated from the
        # selected dependents, whose features would unify capture back in.
        if [[ "$fail" -eq 0 && "$action" == lint ]] && wants '(moq-video|moq-audio)'; then
            just rs media-defaults || fail=1
        fi
        # Device code behind the off-by-default `capture` feature. check-test
        # lints it through the test build, as for the default features.
        if [[ "$fail" -eq 0 && "$action" != lint ]] && wants '(moq-video|moq-audio)'; then
            if [[ "$action" == check-test ]]; then just rs capture-test || fail=1; else just rs capture || fail=1; fi
        fi
        # moq-net without `async`, which every other build unifies back in.
        if [[ "$fail" -eq 0 ]] && wants moq-net; then just rs no-async || fail=1; fi
        # moq-net's `fuzz` module without the `test-support` other builds unify in.
        if [[ "$fail" -eq 0 ]] && wants moq-net; then just rs net-fuzz || fail=1; fi
        # Every C caller still matches libmoq's generated header. rs/libmoq/build.rs
        # generates moq.h from rs/libmoq/src alone, so transitive upstream edits
        # cannot alter the header (and `test rs` still compiles libmoq against them).
        if [[ "$fail" -eq 0 && "$action" == lint ]] && seeded libmoq; then just rs c-tests --syntax || fail=1; fi
        # moq-cli's media tasks are behind `play`; only check-test runs tests.
        if [[ "$fail" -eq 0 && "$action" == check-test ]] && wants moq-cli; then just rs play || fail=1; fi
        if [[ -n "$send_depth_pid" ]]; then
            send_depth_rc=0
            wait "$send_depth_pid" || send_depth_rc=$?
            cat "$send_depth_log"
            if [[ "$send_depth_rc" -ne 0 ]]; then fail=1; fi
        fi
        exit "$fail"
        ;;
    fix)
        just rs fix "${flags[@]}"
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
        # The crates the build holds, for their targets' features below.
        building=$names
        if wants '(moq-video|moq-audio)'; then
            [[ "$packages" == ALL ]] || flags+=(--package moq-video --package moq-audio)
            building+=" moq-video moq-audio"
            features+=(moq-video/capture moq-video/openh264 moq-audio/capture moq-audio/playback)
        fi
        if wants moq-cli; then features+=(moq-cli/play moq-cli/openh264); fi
        if wants moq-relay; then features+=(moq-relay/test-support); fi
        # Cargo skips a target whose `required-features` are off without a word,
        # and a crate alone in the selection (such as moq-boy, whose one target
        # is its binary) has no dependent to turn them on. Turn on every one
        # named by a tested target this build holds; benches and examples are
        # linted in check mode during `lint` instead of linked here.
        required=$(
            set -e
            required_features "$building" tests
        )
        for feature in $required; do
            [[ " ${features[*]-} " == *" $feature "* ]] || features+=("$feature")
        done
        if ((${#features[@]})); then flags+=(--features "${features[*]}"); fi
        just rs test "${flags[@]}" ${timings[@]+"${timings[@]}"}
        ;;
    platform | cross)
        # moq-gst links GStreamer via pkg-config, which neither the runners nor
        # a cross build have.
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
        # render code in moq-video, moq-audio, and the cli. The Linux sound
        # server flags ride along to prove they compile away elsewhere. Only
        # when moq-cli is selected, since cargo rejects a feature of a package
        # outside the selection; a change to anything they reach selects
        # moq-cli anyway.
        features=()
        if wants moq-cli; then features+=(moq-cli/play moq-cli/capture moq-cli/pipewire moq-cli/pulseaudio); fi
        # `--all-targets` skips a target whose required features are off, such
        # as moq-boy's binary without `openh264`, so turn those on for the
        # selected crates, as the test build does. moq-gst is never built here.
        building=$(tr ' ' '\n' <<<"$names" | grep -vx moq-gst | tr '\n' ' ' || true)
        required=$(
            set -e
            required_features "$building" all
        )
        for feature in $required; do
            [[ "$feature" == moq-gst/* || " ${features[*]-} " == *" $feature "* ]] || features+=("$feature")
        done
        if ((${#features[@]})); then flags+=(--features "${features[*]}"); fi
        if [[ "$action" == cross ]]; then
            sh/rs/cross-lint.sh "${flags[@]}"
        else
            cargo check --locked "${flags[@]}" --all-targets
        fi
        ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac
