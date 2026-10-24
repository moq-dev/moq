#!/usr/bin/env bash
# Run `just check`, `just fix`, or `just ci` over one resolved branch diff.
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
    if grep -qE '^(justfile|sh/(dispatch|changed)\.sh)$' "$changed"; then
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
    [js]='^(js/|bench/browser/|doc/|drafts/|demo/(boy|pub|web|carla/web)/|test/|rs/moq-wasm/|rs/.*\.json$|sh/js/|sh/rs/stats-docs\.py$|package\.json$|bun\.lock(b)?$|biome\.jsonc$|flake\.(nix|lock)$)'
    # Workers with lockfiles outside the Bun workspace.
    [workers]='^(infra/|demo/pub/|sh/js/workers\.sh$)'
    [moq_sh]='^infra/moq-sh/'
    # sh/rs/select.sh widens to the whole workspace for inputs every crate shares.
    [rs]='^(rs/|demo/carla/bridge/|sh/rs/|Cargo\.(toml|lock)$|rust-toolchain\.toml$|\.config/nextest\.toml$)'
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
    # bindings generate from it. Each check also compiles its doc samples.
    [py]='^(py/|sh/py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/|doc/lib/py/|doc/lib/samples\.sh$)'
    [kt]='^(kt/|sh/kt/|rs/moq-ffi/|doc/lib/kt/|doc/lib/samples\.sh$)'
    [swift]='^(swift/|sh/swift/|rs/moq-ffi/|doc/lib/swift/|doc/lib/samples\.sh$)'
    [go]='^(go/|sh/go/|rs/moq-ffi/|doc/lib/go/|doc/lib/samples\.sh$)'
    [dart]='^(dart/|sh/dart/|rs/moq-ffi/|doc/lib/dart/|doc/lib/samples\.sh$)'
    # The package links moq-c's native-libs lists, and compiles its doc samples.
    [cpp]='^(cpp/moq/|cpp/justfile$|sh/cpp/|rs/moq-ffi/|rs/moq-c/native-libs/|doc/lib/cpp/|doc/lib/samples\.sh$)'
    # The plugin and its tests call through the bindings generated from cpp/moq
    # and moq-ffi and link through moq-c's native-libs lists, and flake.nix owns
    # the libobs headers it compiles against.
    [obs_compile]='^(cpp/obs/|cpp/moq/|sh/obs/|rs/moq-ffi/|rs/moq-c/native-libs/|flake\.nix$)'
    # `obs check` compares the OBS pinned in buildspec.json, flake.nix, and
    # nixpkgs, and the last moves on a flake.lock bump alone.
    [obs]='^(cpp/obs/|sh/obs/|flake\.(nix|lock)$)'
    # Rust source is compiled below. Nix reads Cargo metadata, packaging inputs,
    # and its own modules and test helpers; those still gate the full flake.
    [flake]='(^rs/.*\.(toml|txt|ptx|in)$|^Cargo\.(toml|lock)$|^rust-toolchain\.toml$|^flake\.lock$|^nix/|\.nix$)'
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
    for module in rs swift cpp obs_compile moq_sh; do
        enabled=false
        pattern=${scope[$module]}
        case "$module" in
            swift | cpp | obs_compile) pattern+="|${scope[rs]}" ;;
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
    [gh]='actionlint bun python3 cargo clippy-driver dpkg-deb nfpm rpm'
)

# CI and the local check share one selection and one lint/test dispatch.
verb=$action
[[ "$verb" != ci ]] || verb=check

# The modules whose run compiles Rust through Cargo, so CI restores the Rust
# build cache only when one is selected. JS checks build @moq/wasm for its
# consumers, and the bindings build moq-ffi.
declare -A compiles=(
    [check]='rs bench js py kt go dart cpp obs_compile'
    [fix]='rs'
)

case "$action" in
    check | ci) modules=(js workers moq_sh drafts rs bench carla quest drill py kt swift go dart cpp obs_compile obs flake ts_paths markdown shell large_files toml nix justfile gh) ;;
    fix) modules=(js rs py carla dart obs markdown shell toml nix justfile) ;;
esac

selected=()
missing=()
for module in "${modules[@]}"; do
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

# Each phase's wall time, printed when the run ends, pass or fail, so the slow
# module of a check or a queue run is named without reading log timestamps.
# GitHub Actions also gets it in the job summary.
labels=()
seconds=()
current=
began=0
begin() {
    current=$1
    began=$SECONDS
}
finish() {
    [[ -n "$current" ]] || return 0
    labels+=("$current${1:+ ($1)}")
    seconds+=("$((SECONDS - began))")
    current=
}
report() {
    finish failed
    ((${#labels[@]})) || return 0
    echo "$action: seconds per module" >&2
    for i in "${!labels[@]}"; do
        printf '%6d  %s\n' "${seconds[$i]}" "${labels[$i]}" >&2
    done
    [[ -n "${GITHUB_STEP_SUMMARY:-}" ]] || return 0
    {
        printf '| %s module | Seconds |\n| --- | ---: |\n' "$action"
        for i in "${!labels[@]}"; do
            printf '| %s | %d |\n' "${labels[$i]}" "${seconds[$i]}"
        done
    } >>"$GITHUB_STEP_SUMMARY"
}
trap 'report; rm -f "$changed" "$js_changed"' EXIT

for module in "${selected[@]}"; do
    begin "$verb $module"
    case "$verb:$module" in
        check:js) just js check-changed "$js_list" ;;
        check:workers) just js workers ;;
        check:moq_sh) just infra moq-sh test ;;
        # The preamble keeps docs and target-specific lints; the test build
        # below enforces default-feature clippy without a second compile.
        check:rs) just rs lint-changed "$rs_list" ;;
        check:bench)
            python3 bench/compare.test.py
            cargo check --locked --package moq-relay --package moq-bench --features moq-relay/io-uring
            ;;
        check:quest) quest check ;;
        check:drill) just test drill-sensitivity --apply-only ;;
        check:obs_compile) just obs compile ;;
        check:flake) nix flake check ;;
        check:ts_paths) python3 sh/gh/ts-paths.py ;;
        check:markdown)
            bun install --frozen-lockfile
            sh/markdown.sh check
            ;;
        check:large_files) sh/large-files.sh "$large_base" ;;
        check:toml) RUST_LOG=error taplo format --check ;;
        check:nix) files '*.nix' | xargs -0 nixfmt --check ;;
        check:justfile) files justfile '*/justfile' | xargs -0 -n1 just --fmt --check --justfile ;;
        fix:rs) just rs fix-changed "$rs_list" ;;
        fix:markdown)
            bun install
            sh/markdown.sh fix
            ;;
        fix:toml) RUST_LOG=error taplo format ;;
        fix:nix) files '*.nix' | xargs -0 nixfmt ;;
        fix:justfile) files justfile '*/justfile' | xargs -0 -n1 just --fmt --justfile ;;
        *:shell) sh/shell.sh "$verb" ;;
        *) just "$module" "$verb" ;;
    esac
    finish
done

# The bindings run their tests in the preamble; these suites run once here.
# gh's tooling tests run on every diff: path selection would let a broken
# selector skip the very tests that catch it.
if [[ "$verb" == check ]]; then
    for module in "${selected[@]}"; do
        case "$module" in
            rs | js | py | gh) begin "test $module" ;;
        esac
        case "$module" in
            rs) just rs test-changed "$rs_list" ;;
            js) just js test-changed "$js_list" ;;
            py) just py test ;;
            gh) just gh test ;;
        esac
        finish
    done
fi
