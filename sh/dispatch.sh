#!/usr/bin/env bash
# Run `just check`, `just fix`, or `just ci` over one resolved branch diff.
# check and ci run CHECK_JOBS lanes of modules at once (default 3); 1 runs
# them in order and streams their output. CHECK_MODULES limits the selection to
# the modules it names, or skips those named with a leading `-`, so a gate too
# large for one job splits across several.
# CHECK_FAIL_FAST=1 stops every lane at the first failed module, for the land
# gate, which ejects a failing batch either way and gains nothing from the rest.
# `--query` prints the selection before CI installs its toolchain and caches.
# `ci-platform` exists only as a query for the native platform workflows.
set -euo pipefail

usage="usage: sh/dispatch.sh check|fix|ci [BASE|--all] [--query], or ci-platform BASE|--all --query"
action=${1:?$usage}
base=${2:-}
query=${3:-}
if [[ $# -gt 3 || (-n "$query" && "$query" != --query) || ("$action" == ci-platform && -z "$query") ]]; then
    echo "$usage" >&2
    exit 2
fi
case "$action" in
    check | fix | ci | ci-platform) ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

# A host toolchain fails deep in a build instead: its gcc keeps the
# _FORTIFY_SOURCE the dev shell disables, which breaks jemalloc's -Werror
# configure probes. This flake's dev shell sets IN_MOQ_DEV_SHELL. Windows has no
# Nix, so it always runs on the host.
if [[ -z "$query" && -z "${IN_MOQ_DEV_SHELL:-}" && -z "${ALLOW_HOST_TOOLCHAIN:-}" && "$OSTYPE" != msys && "$OSTYPE" != cygwin ]]; then
    echo "error: run inside the Nix dev shell ('nix develop' or direnv) so tools match CI" >&2
    echo "       or set ALLOW_HOST_TOOLCHAIN=1 to use the host toolchain anyway" >&2
    exit 1
fi

cd "$(git rev-parse --show-toplevel)"

changed=$(mktemp)
js_changed=$(mktemp)
trap 'rm -f "$changed" "$js_changed"' EXIT

all=
if [[ "$base" == --all ]]; then
    all=1
else
    sh/changed.sh "$base" >"$changed"

    # These files hold the map and the recipes that call it, and match no
    # module, so a change to them would otherwise validate none of it.
    if grep -qE '^(justfile|sh/(dispatch|changed)\.sh|sh/local\.(sh|ts))$' "$changed"; then
        echo "$action: root orchestration changed; running everything." >&2
        all=1
    fi
fi

# The impact map: a module is in scope when a changed path matches its
# pattern. An empty pattern is a repository-wide lint that runs on every diff.
declare -A scope=(
    # The JS tests also read Rust's shared JSON vectors; sh/js/workspace.py
    # fails a check that reads a cross-tree input this pattern misses.
    # demo/pub is a Worker outside the Bun workspace, listed so Biome lints it.
    # Root Nix inputs own the browser version every npm Playwright pin matches.
    [js]='^(js/|bench/browser/|doc/|drafts/|demo/(boy|pub|web|carla/web)/|test/|rs/.*\.json$|sh/js/|sh/rs/stats-docs\.py$|package\.json$|bun\.lock(b)?$|biome\.jsonc$|flake\.(nix|lock)$)'
    # Workers with lockfiles outside the Bun workspace.
    [workers]='^(infra/|demo/pub/|sh/js/workers\.sh$)'
    [moq_sh]='^infra/moq-sh/'
    # sh/rs/select.sh widens to the whole workspace for inputs every crate shares.
    # moq-cli builds the Rust interop clients as examples from outside its
    # crate; sh/rs/select.test.py fails when a target's directory is missing.
    # moq-relay's tests read its NixOS module.
    [rs]='^(\.cargo/|nix/modules/moq-relay\.nix$|sh/local\.(sh|ts)$|rs/|demo/carla/bridge/|test/interop/clients/rust/|sh/rs/|Cargo\.(toml|lock)$|clippy\.toml$|rust-toolchain\.toml$|\.config/nextest\.toml$)'
    [bench]='^bench/'
    # The CARLA demo's simulator worker and launchers. Its player is a JS
    # package and its GPU bridge a Rust crate, so those modules cover them.
    [carla]='^demo/carla/(sim/|justfile$)'
    [drafts]='^(drafts/|sh/drafts/|doc/\.vitepress/drafts\.ts$)'
    # Quest documents form one graph, so any change validates the whole tree,
    # as does a bump of the flake-pinned validator.
    [quest]='^(quest/|flake\.lock$)'
    # The drill mutations patch Rust source, so a Rust change can move the code
    # they target. Only nightly runs the drills; this catches a stale patch.
    [drill]='^(rs/|test/drill/)'
    # maturin bundles rs/moq-ffi into the moq-ffi wheel, and the other
    # bindings generate from it, built by the `cargo build-ffi` alias in
    # .cargo/config.toml. Each check also compiles its doc samples.
    [py]='^(py/|sh/py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/|doc/lib/py/|doc/lib/samples\.sh$)'
    [kt]='^(\.cargo/config\.toml$|kt/|sh/kt/|rs/moq-ffi/|doc/lib/kt/|doc/lib/samples\.sh$)'
    [swift]='^(\.cargo/config\.toml$|swift/|sh/swift/|rs/moq-ffi/|doc/lib/swift/|doc/lib/samples\.sh$)'
    [go]='^(\.cargo/config\.toml$|go/|sh/go/|rs/moq-ffi/|doc/lib/go/|doc/lib/samples\.sh$)'
    [dart]='^(\.cargo/config\.toml$|dart/|sh/dart/|rs/moq-ffi/|doc/lib/dart/|doc/lib/samples\.sh$)'
    # The C package links moq-c's native-libs lists, and the C++ package's FFI pin.
    [c]='^(\.cargo/config\.toml$|c/|sh/c/|rs/moq-ffi/|rs/moq-c/native-libs/|cpp/moq/FFI_VERSION$|cpp/moq/cmake/moq-ffi\.cmake$)'
    # The package links moq-c's native-libs lists, and compiles its doc samples.
    [cpp]='^(\.cargo/config\.toml$|cpp/moq/|cpp/justfile$|sh/cpp/|rs/moq-ffi/|rs/moq-c/native-libs/|doc/lib/cpp/|doc/lib/samples\.sh$)'
    # The plugin and its tests call through the bindings generated from cpp/moq
    # and moq-ffi and link through moq-c's native-libs lists, and flake.nix owns
    # the libobs headers it compiles against.
    [obs_compile]='^(\.cargo/config\.toml$|cpp/obs/|cpp/moq/|sh/obs/|rs/moq-ffi/|rs/moq-c/native-libs/|flake\.nix$)'
    # `obs check` compares the OBS pinned in buildspec.json, flake.nix, and
    # nixpkgs, and the last moves on a flake.lock bump alone.
    [obs]='^(cpp/obs/|sh/obs/|flake\.(nix|lock)$)'
    # Rust source is compiled below. Nix reads Cargo metadata, packaging inputs,
    # and its own modules and test helpers; those still gate the full flake.
    [flake]='(^\.cargo/|^rs/.*\.(toml|txt|ptx|in)$|^Cargo\.(toml|lock)$|^rust-toolchain\.toml$|^flake\.lock$|^nix/|\.nix$)'
    # ts.yml's crate filters against the closure test/ts/run.sh builds, which
    # moves only with a manifest, the lockfile, or the harness's packages.
    [ts_paths]='^(rs/.*Cargo\.toml$|Cargo\.(toml|lock)$|test/ts/run\.sh$|\.github/workflows/release-gate\.yml$|sh/gh/ts-paths\.py$)'
    [markdown]=''
    [shell]=''
    [large_files]=''
    [toml]=''
    [nix]=''
    [justfile]=''
    [gh]=''
)

# Report each native suite from the same impact map, before any toolchain is
# installed. Rust dependencies can break external consumers too, so include
# them in each build.
if [[ "$action" == ci-platform ]]; then
    needed=false
    shared=$all
    if grep -qE '^(\.cargo/|\.github/|[^/]+/justfile$|justfile$)' "$changed"; then
        shared=1
    fi
    for module in rs swift c cpp obs_compile moq_sh; do
        enabled=false
        pattern=${scope[$module]}
        case "$module" in
            swift | c | cpp | obs_compile) pattern+="|${scope[rs]}" ;;
        esac
        if [[ -n "$shared" ]] || grep -qE "$pattern" "$changed"; then
            enabled=true
            needed=true
        fi
        echo "$module=$enabled"
    done
    echo "needed=$needed"
    exit
fi

# The tools each module needs. A module missing one is skipped locally, so an
# incomplete toolchain checks less; under CI_STRICT (CI) it is an error, up
# front, because there a skip is indistinguishable from a pass. swift needs
# none: it skips off macOS by design, and platform.yml is its real gate.
declare -A tools=(
    [js]='bun python3'
    [workers]='bun'
    [moq_sh]='dash'
    [rs]='cargo jq'
    [bench]='cargo'
    [carla]='uv'
    [drafts]='bun kramdown-rfc xml2rfc'
    [quest]='quest'
    [drill]=''
    [py]='uv'
    [kt]='cargo gradle java'
    [swift]=''
    [go]='cargo go uniffi-bindgen-go'
    [dart]='cargo dart uniffi_bindgen_dart'
    [c]='cargo cmake cc c++ pkg-config uniffi-bindgen-cpp'
    [cpp]='cargo cmake c++ uniffi-bindgen-cpp'
    [obs_compile]='cargo cmake pkg-config uniffi-bindgen-cpp'
    [obs]='clang-format cmake gersemi jq'
    [flake]='nix'
    [ts_paths]='cargo python3'
    [markdown]='bun'
    [shell]='shellcheck shfmt'
    [large_files]=''
    [toml]='taplo'
    [nix]='nixfmt'
    [justfile]=''
    [gh]='actionlint zizmor bun python3 cargo clippy-driver dpkg-deb nfpm rpm'
)

# CI and the local check share one selection and one lint/test dispatch.
verb=$action
[[ "$verb" != ci ]] || verb=check

# The modules whose run compiles Rust through Cargo, so CI restores the Rust
# build cache only when one is selected. The bindings build moq-ffi.
declare -A compiles=(
    [check]='rs bench py kt go dart c cpp obs_compile'
    [fix]='rs'
)

# Lanes start in this order, so the slowest come first. dart runs alone (see
# below), so it waits for the rest.
case "$action" in
    check | ci) modules=(py rs c cpp obs_compile js kt flake bench carla swift go drafts markdown gh drill workers moq_sh quest obs ts_paths shell large_files toml nix justfile dart) ;;
    fix) modules=(js rs py carla dart obs markdown shell toml nix justfile) ;;
esac

# Modules that share mutable state run in one lane, in module order; every
# other module is a lane of its own, and lanes run concurrently.
declare -A lane=(
    # `bun install` relinks the root workspace's packages even when nothing
    # changed, so a concurrent install or import of them fails.
    [js]=bun
    [drafts]=bun
    [markdown]=bun
    [gh]=bun
    # Both build cpp/moq/build.
    [cpp]=cpp
    [obs_compile]=cpp
)

# kt, swift, go, cpp, and obs_compile each build moq-ffi's cdylib with the same
# features and profile before reading target/debug/libmoq_ffi.so. Cargo runs
# one build at a time per target directory and leaves a fresh output untouched,
# so the first of those lanes builds it while the rest wait in Cargo, and none
# rewrites it while another reads it. py builds its test features into a target
# directory of its own, and carla runs its tools outside py's venv.

# Lanes that run with no other lane beside them. dart builds moq-ffi with the
# filtered environment its build hook gets, then checks that the hook finds
# that build fresh. Any other Cargo build in between reruns the host build
# scripts in target/debug that track the environment, such as bindgen's
# BINDGEN_EXTRA_CLANG_ARGS, and so rebuilds what the hook then rebuilds again.
declare -A alone=([dart]=1)

# CHECK_MODULES names the modules to run, or with a leading `-` those to skip.
declare -A only=() skip=()
for word in ${CHECK_MODULES:-}; do
    name=${word#-}
    if [[ " ${modules[*]} " != *" $name "* ]]; then
        echo "error: CHECK_MODULES names '$name', which $action does not run; pick from: ${modules[*]}" >&2
        exit 2
    fi
    if [[ "$word" == -* ]]; then skip[$name]=1; else only[$name]=1; fi
done

selected=()
missing=()
for module in "${modules[@]}"; do
    if [[ -v "skip[$module]" ]] || { ((${#only[@]})) && [[ ! -v "only[$module]" ]]; }; then
        continue
    fi
    pattern=${scope[$module]}
    if [[ -z "$all" && -n "$pattern" ]] && ! grep -qE "$pattern" "$changed"; then
        continue
    fi
    # Selection must not depend on the tools setup has yet to install.
    if [[ -n "$query" ]]; then
        selected+=("$module")
        continue
    fi
    absent=()
    for tool in ${tools[$module]}; do
        command -v "$tool" >/dev/null 2>&1 || absent+=("$tool")
    done
    if ((${#absent[@]} == 0)); then
        selected+=("$module")
    elif [[ -n "${CI_STRICT:-}" ]]; then
        missing+=("${absent[@]}")
    else
        echo "$action: skipping $module; missing ${absent[*]}" >&2
    fi
done

# A query runs on the host before Nix or cache restore. Keep the normal
# selection loop authoritative, including its repository-wide lints.
if [[ -n "$query" ]]; then
    needed=false
    rust=false
    for module in "${selected[@]}"; do
        needed=true
        [[ " ${compiles[$verb]} " != *" $module "* ]] || rust=true
    done
    echo "needed=$needed"
    echo "rust=$rust"
    exit
fi

if ((${#missing[@]})); then
    echo "error: CI_STRICT is set but these tools are missing: $(printf '%s\n' "${missing[@]}" | sort -u | tr '\n' ' ')" >&2
    echo "       run inside 'nix develop', or unset CI_STRICT to skip what isn't installed" >&2
    exit 1
fi

# rs selects crates from the same list, or the whole workspace.
rs_list=$changed
[[ -z "$all" ]] || rs_list=--all

# large_files diffs against the same base, or checks every file.
large_base=$base
[[ -z "$all" ]] || large_base=--all

# Pass only JS-impacting paths, so an unrelated quest or Rust file does not
# turn a narrow package selection into the shared-input fallback.
js_list=--all
if [[ -z "$all" ]]; then
    grep -E "${scope[js]}" "$changed" >"$js_changed" || true
    js_list=$js_changed
fi

# Tracked and new files, skipping whatever .gitignore does (node_modules, target).
files() {
    git ls-files -z --cached --others --exclude-standard -- "$@"
}

# One phase of one module. The bindings run their tests in their check; rs,
# js, py, and gh test right after it, in the same lane.
step() {
    case "$1:$2" in
        check:js) just js check-changed "$js_list" ;;
        check:workers) just js workers ;;
        check:moq_sh) just infra moq-sh test ;;
        # The check keeps docs; the test phase enforces default-feature clippy
        # without a second compile.
        check:rs) just rs lint-changed "$rs_list" ;;
        check:bench)
            python3 bench/compare.test.py
            bash sh/local.sh cargo check --locked --package moq-relay --package moq-bench --features moq-relay/io-uring
            ;;
        check:quest) quest check ;;
        check:drill) just test drill-sensitivity --apply-only ;;
        check:obs_compile) just obs compile ;;
        check:flake)
            if [[ -z "${CI_STRICT:-}" && "$OSTYPE" == linux* && "$HOSTTYPE" == x86_64 ]]; then
                # Repart creates its namespace as a Nix builder, whose policy can
                # differ from this shell's. A fresh name prevents cached success.
                read -r nonce </proc/sys/kernel/random/uuid
                if ! result=$(bash sh/local.sh nix build --impure --file nix/runner/userns-probe.nix \
                    --argstr nonce "$nonce" --no-link --builders '' --max-jobs 1 --print-build-logs 2>&1); then
                    printf '%s\n' "$result" >&2
                    [[ "$result" == *MOQ_USERNS_UNAVAILABLE* ]] || return 1
                    echo "$action: skipping flake; the local Nix builder cannot create user namespaces" >&2
                    return 77
                fi
            fi
            # Without KVM, QEMU falls back to emulation and the VM tests time
            # out long after. Refuse rather than skip, so a local gate never
            # reads as passing without them.
            if [[ "$OSTYPE" == linux* && "$HOSTTYPE" == x86_64 && ! -e /dev/kvm ]]; then
                echo "$action: flake's NixOS VM tests need KVM, but /dev/kvm is missing; run them on a KVM host, as CI does" >&2
                return 1
            fi
            # 77 marks the probe's skip, so the check's own 77 is a failure.
            # Full build logs name a failing VM subtest; Nix otherwise keeps
            # only the last 25 lines, which are QEMU's cleanup.
            bash sh/local.sh nix flake check --print-build-logs || {
                local status=$?
                [[ -n "${CI_STRICT:-}" ]] || ((status != 77)) || status=1
                return "$status"
            }
            ;;
        check:ts_paths) python3 sh/gh/ts-paths.py ;;
        check:markdown)
            bun install --frozen-lockfile
            sh/markdown.sh check
            ;;
        check:large_files) sh/large-files.sh "$large_base" ;;
        check:toml) RUST_LOG=error taplo format --check ;;
        check:nix) files '*.nix' | xargs -0 nixfmt --check ;;
        check:justfile) files justfile '*/justfile' | xargs -0 -n1 just --fmt --check --justfile ;;
        test:rs) just rs test-changed "$rs_list" ;;
        test:js) just js test-changed "$js_list" ;;
        test:py) just py test ;;
        test:gh) just gh test ;;
        fix:rs) just rs fix-changed "$rs_list" ;;
        fix:js) just js fix "$js_list" ;;
        fix:markdown)
            bun install
            sh/markdown.sh fix
            ;;
        fix:toml) RUST_LOG=error taplo format ;;
        fix:nix) files '*.nix' | xargs -0 nixfmt ;;
        fix:justfile) files justfile '*/justfile' | xargs -0 -n1 just --fmt --justfile ;;
        *:shell) sh/shell.sh "$1" ;;
        *) just "$2" "$1" ;;
    esac
}

# Lanes run at most this many at a time. fix stays serial: its fixers rewrite
# files the others read, such as taplo under `cargo clippy --fix`.
limit=${CHECK_JOBS:-3}
[[ "$action" != fix ]] || limit=1
if [[ ! "$limit" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: CHECK_JOBS must be a positive integer, not '$limit'" >&2
    exit 2
fi

lanes=()
declare -A members=()
for module in "${selected[@]}"; do
    name=${lane[$module]:-$module}
    [[ -v "members[$name]" ]] || lanes+=("$name")
    members[$name]+=" $module"
done

github() {
    [[ -n "${GITHUB_ACTIONS:-}" ]]
}

# Run one lane's modules in order, appending each phase's seconds to the lane's
# times file and announcing each phase's start and end on fd 3. A failed phase
# skips the rest of that module, but independent modules still run. A TERM
# records the phase it cut short.
run_lane() {
    times=$1
    shift
    current=
    lane_status=0
    trap 'cancelled; exit 143' TERM
    for module in "$@"; do
        phases=("$verb")
        # gh's tooling tests run on every diff: path selection would let a
        # broken selector skip the very tests that catch it.
        if [[ "$verb" == check ]]; then
            case "$module" in
                rs | js | py | gh) phases+=(test) ;;
            esac
        fi
        for phase in "${phases[@]}"; do
            current="$phase $module"
            began=$SECONDS
            if github; then echo "::group::$current"; else echo "==> $current"; fi
            echo "$action: $current started" >&3
            # A plain statement, so errexit stays on inside the subshell.
            set +e
            (
                set -e
                # A subshell dies on TERM at once, orphaning the phase's
                # processes. Trapped, it exits once they do, so a cancel
                # returns only after the whole tree has exited.
                trap 'exit 143' TERM
                step "$phase" "$module"
            )
            status=$?
            set -e
            ! github || echo "::endgroup::"
            if ((status == 77)) && [[ "$module" == flake && -z "${CI_STRICT:-}" ]]; then
                record skipped
                current=
                break
            fi
            if ((status)); then
                record failed
                ((lane_status)) || lane_status=$status
                current=
                [[ -z "${CHECK_FAIL_FAST:-}" ]] || return "$lane_status"
                break
            fi
            record
            current=
        done
    done
    return "$lane_status"
}
record() {
    local seconds=$((SECONDS - began))
    echo "$seconds $current${1:+ ($1)}" >>"$times"
    echo "$action: $current ${1:-passed} in ${seconds}s" >&3
}
cancelled() {
    [[ -n "$current" ]] || return 0
    ! github || echo "::endgroup::"
    record cancelled
}

# Each phase's wall time, printed when the run ends, pass or fail, so the slow
# module of a check or a queue run is named without reading log timestamps.
# GitHub Actions also gets it in the job summary. Concurrent phases overlap, so
# the run's own wall time comes last.
report() {
    local name row seconds label rows=()
    for name in "${lanes[@]:0:next}"; do
        [[ ! -f "$logs/$name.times" ]] || mapfile -t -O "${#rows[@]}" rows <"$logs/$name.times"
    done
    ((${#rows[@]})) || return 0
    rows+=("$SECONDS wall time")
    echo "$action: seconds per module" >&2
    for row in "${rows[@]}"; do
        read -r seconds label <<<"$row"
        printf '%6d  %s\n' "$seconds" "$label" >&2
    done
    [[ -n "${GITHUB_STEP_SUMMARY:-}" ]] || return 0
    {
        printf '| %s module | Seconds |\n| --- | ---: |\n' "$action"
        for row in "${rows[@]}"; do
            read -r seconds label <<<"$row"
            printf '| %s | %d |\n' "$label" "$seconds"
        done
    } >>"$GITHUB_STEP_SUMMARY"
}

# Stop every running lane, whose process group holds its whole tree.
cancel() {
    local pid
    for pid in "${!running[@]}"; do
        kill -TERM -- "-$pid" 2>/dev/null || true
    done
}

logs=$(mktemp -d)
declare -A running=()
lone=
next=0
failed=()
failed_status=0
trap 'cancel; wait; report; rm -rf "$logs" "$changed" "$js_changed"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Each lane runs in its own process group, so a cancel reaches its whole tree;
# a lane shell defers its own TERM trap until the running phase exits.
# Concurrent lanes buffer their output and print it whole when they finish; a
# serial run streams instead. A buffered lane still announces each phase on the
# run's stderr as it starts and ends, so a run cut off mid-phase, such as by a
# job timeout that never reaches this script, names the phase it stopped in.
# Every lane completes even when another fails, so one unavailable check cannot
# hide unrelated failures; under CHECK_FAIL_FAST the first failure cancels the
# running lanes and starts no more.
set -m
stopping=
while true; do
    while [[ -z "$lone$stopping" ]] && ((next < ${#lanes[@]} && ${#running[@]} < limit)); do
        name=${lanes[next]}
        if [[ -v "alone[$name]" ]] && ((${#running[@]})); then
            break
        fi
        next=$((next + 1))
        read -ra lane_modules <<<"${members[$name]}"
        if ((limit == 1)); then
            run_lane "$logs/$name.times" "${lane_modules[@]}" </dev/null 3>/dev/null &
        else
            run_lane "$logs/$name.times" "${lane_modules[@]}" </dev/null 3>&2 >"$logs/$name.log" 2>&1 &
        fi
        running[$!]=$name
        [[ ! -v "alone[$name]" ]] || lone=$!
    done
    ((${#running[@]})) || break

    status=0
    wait -n -p pid "${!running[@]}" || status=$?
    name=${running[$pid]}
    unset "running[$pid]"
    [[ "$pid" != "$lone" ]] || lone=
    if ((status)) && [[ -z "$stopping" ]]; then
        failed+=("$name")
        ((failed_status)) || failed_status=$status
        if [[ -n "${CHECK_FAIL_FAST:-}" ]]; then
            stopping=1
            cancel
        fi
    fi
    ((limit == 1)) || cat "$logs/$name.log"
done

# Name every failed module after all independent work has completed.
if ((${#failed[@]})); then
    for name in "${failed[@]}"; do
        failed_phases=$(sed -n 's/^[0-9]* \(.*\) (failed)$/\1/p' "$logs/$name.times")
        while IFS= read -r phase; do
            if github; then echo "::error::$action: $phase failed"; else echo "$action: $phase failed" >&2; fi
        done <<<"${failed_phases:-${members[$name]# }}"
    done
    exit "$failed_status"
fi
