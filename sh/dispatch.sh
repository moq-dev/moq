#!/usr/bin/env bash
# Run `just check`, `just fix`, or `just ci check|test` over the branch diff.
# `--query` prints the selection instead; `ci-platform` exists only as a query.
#
# Usage: sh/dispatch.sh check|fix|ci-check|ci-test [BASE|--all] [--query]
#        sh/dispatch.sh ci-platform BASE|--all --query
#
# The branch diff is resolved once and matched against the impact map below,
# the one place that says which paths put which module in scope. `check` is
# `ci-check` plus `ci-test`, which CI runs as parallel jobs with MOQ_STRICT=1,
# so there is no second definition of "checked".
set -euo pipefail

usage="usage: sh/dispatch.sh check|fix|ci-check|ci-test [BASE|--all] [--query], or ci-platform BASE|--all --query"
action=${1:?$usage}
base=${2:-}
query=${3:-}
if [[ $# -gt 3 || (-n "$query" && "$query" != --query) || ("$action" == ci-platform && -z "$query") ]]; then
    echo "$usage" >&2
    exit 2
fi
case "$action" in
    check | fix | ci-check | ci-test | ci-platform) ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

# A host toolchain fails deep in a build instead: its gcc keeps the
# _FORTIFY_SOURCE the dev shell disables, which breaks jemalloc's -Werror
# configure probes. This flake's dev shell sets MOQ_DEV_SHELL. Windows has no
# Nix, so it always runs on the host.
if [[ -z "$query" && -z "${MOQ_DEV_SHELL:-}" && -z "${MOQ_ALLOW_HOST:-}" && "$OSTYPE" != msys && "$OSTYPE" != cygwin ]]; then
    echo "error: run inside the Nix dev shell ('nix develop' or direnv) so tools match CI" >&2
    echo "       or set MOQ_ALLOW_HOST=1 to use the host toolchain anyway" >&2
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
    [js]='^(js/|doc/|drafts/|demo/(boy|pub|web)/|test/|rs/moq-wasm/|rs/.*\.json$|sh/js/|sh/rs/stats-docs\.py$|package\.json$|bun\.lock(b)?$|biome\.jsonc$)'
    # Workers with lockfiles outside the Bun workspace.
    [workers]='^(infra/apt/|infra/rpm/|infra/moq-sh/|demo/pub/|sh/js/workers\.sh$)'
    [moq_sh]='^infra/moq-sh/'
    # sh/rs/select.sh widens to the whole workspace for inputs every crate shares.
    [rs]='^(rs/|sh/rs/|Cargo\.(toml|lock)$|rust-toolchain\.toml$|\.config/nextest\.toml$)'
    [bench]='^bench/'
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
    [flake]='(^rs/|^Cargo\.(toml|lock)$|^flake\.lock$|\.nix$)'
    [markdown]=''
    [shell]=''
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
# incomplete toolchain checks less; under MOQ_STRICT (CI) it is an error, up
# front, because there a skip is indistinguishable from a pass. swift needs
# none: it skips off macOS by design, and platform.yml is its real gate.
declare -A tools=(
    [js]='bun python3'
    [workers]='bun'
    [moq_sh]='dash'
    [rs]='cargo jq'
    [bench]='cargo'
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
    [markdown]='bun'
    [shell]='shellcheck shfmt'
    [toml]='taplo'
    [nix]='nixfmt'
    [justfile]=''
    [gh]='actionlint bun'
)

# `check` and `ci-check` share every lint; they differ in how rs compiles and
# whether the tests run.
verb=${action#ci-}

# The modules whose run compiles Rust through Cargo, so CI restores the Rust
# build cache only when one is selected. JS checks build @moq/wasm for its
# consumers, and the bindings build moq-ffi.
declare -A compiles=(
    [check]='rs bench js py kt go dart cpp obs_compile'
    [fix]='rs'
    [test]='rs py'
)

case "$action" in
    check | ci-check) modules=(js workers moq_sh drafts rs bench quest drill py kt swift go dart cpp obs_compile obs flake markdown shell toml nix justfile gh) ;;
    fix) modules=(js rs py dart obs markdown shell toml nix justfile) ;;
    ci-test) modules=(js rs py) ;;
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
    elif [[ -n "${MOQ_STRICT:-}" ]]; then
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
    echo "error: MOQ_STRICT is set but these tools are missing: $(printf '%s\n' "${missing[@]}" | sort -u | tr '\n' ' ')" >&2
    echo "       run inside 'nix develop', or unset MOQ_STRICT to skip what isn't installed" >&2
    exit 1
fi

# rs selects crates from the same list, or the whole workspace.
rs_list=$changed
[[ -z "$all" ]] || rs_list=--all

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

for module in "${selected[@]}"; do
    case "$verb:$module" in
        check:js) just js check-changed "$js_list" ;;
        check:workers) just js workers ;;
        check:moq_sh) just infra moq-sh test ;;
        # Test enforces default-feature clippy through its compile. Check keeps
        # the other lints and distinct feature/target builds.
        check:rs) if [[ "$action" == check ]]; then just rs check-test-changed "$rs_list"; else just rs lint-changed "$rs_list"; fi ;;
        check:bench) cargo check --locked --package moq-relay --package moq-bench --features moq-relay/io-uring ;;
        check:quest) quest check ;;
        check:drill) just test drill-sensitivity --apply-only ;;
        check:obs_compile) just obs compile ;;
        check:flake) nix flake check ;;
        check:markdown)
            bun install --frozen-lockfile
            sh/markdown.sh check
            ;;
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
        test:js) just js test-changed "$js_list" ;;
        test:rs) just rs test-changed "$rs_list" ;;
        *:shell) sh/shell.sh "$verb" ;;
        *) just "$module" "$verb" ;;
    esac
done

# The rest of `check`'s tests. rs ran its own above, and the kt, swift, go, and
# dart checks always run theirs.
if [[ "$action" == check ]]; then
    for module in "${selected[@]}"; do
        case "$module" in
            js) just js test-changed "$js_list" ;;
            py) just py test ;;
        esac
    done
fi
