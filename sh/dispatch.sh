#!/usr/bin/env bash
# Run `just check`, `just fix`, or `just test` over what the branch changed.
#
# Usage: sh/dispatch.sh check|fix|test [BASE|--all]
#
# The branch diff is resolved once and matched against the impact map below,
# the one place that says which paths put which module in scope. CI runs the
# same thing with MOQ_STRICT=1, so there is no second definition of "checked".
set -euo pipefail

usage="usage: sh/dispatch.sh check|fix|test [BASE|--all]"
action=${1:?$usage}
base=${2:-}
case "$action" in
    check | fix | test) ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac

cd "$(git rev-parse --show-toplevel)"

changed=$(mktemp)
trap 'rm -f "$changed"' EXIT

all=
if [[ "$base" == --all ]]; then
    all=1
else
    # BASE: the argument, then $GITHUB_BASE_REF (a PR checkout has no
    # upstream), then the upstream, then origin/main. `git push -u` points the
    # upstream at the branch's own remote copy, which would diff HEAD against
    # itself, so that case falls through to origin/main.
    if [[ -z "$base" && -n "${GITHUB_BASE_REF:-}" ]]; then
        base="origin/$GITHUB_BASE_REF"
    fi
    if [[ -z "$base" ]]; then
        base=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null || true)
        if [[ -z "$base" || "$base" == */"$(git branch --show-current)" ]]; then
            base=origin/main
        fi
    fi
    merge_base=$(git merge-base "$base" HEAD) || {
        echo "error: cannot resolve merge-base against $base (is full history fetched?)" >&2
        exit 1
    }
    echo "$action: base $base" >&2

    # Untracked files count too: a brand new crate or module is the whole change.
    {
        git diff --name-only "$merge_base"
        git ls-files --others --exclude-standard
    } | sort -u >"$changed"

    # These files hold the map and the recipes that call it, and match no
    # module, so a change to them would otherwise validate none of it.
    if grep -qE '^(justfile|test/justfile|sh/dispatch\.sh)$' "$changed"; then
        echo "$action: root orchestration changed; running everything." >&2
        all=1
    fi
fi

# The impact map: a module is in scope when a changed path matches its
# pattern. An empty pattern is a repository-wide lint that runs on every diff.
declare -A scope=(
    [js]='^(js/|doc/|drafts/|demo/(boy|web)/|test/interop/clients/js|test/wasm/|sh/js/|sh/rs/stats-docs\.py$|package\.json$|bun\.lock(b)?$|biome\.jsonc$)'
    # Workers with lockfiles outside the Bun workspace.
    [workers]='^(infra/apt/|infra/rpm/|demo/pub/|sh/js/workers\.sh$)'
    # sh/rs/select.sh widens to the whole workspace for inputs every crate shares.
    [rs]='^(rs/|sh/rs/|Cargo\.(toml|lock)$|rust-toolchain\.toml$|\.config/nextest\.toml$)'
    [bench]='^bench/'
    [drafts]='^(drafts/|sh/drafts/|doc/\.vitepress/drafts\.ts$)'
    # Quest documents form one graph, so any change validates the whole tree.
    [quest]='^(quest/|rs/quest/)'
    # maturin bundles rs/moq-ffi into the moq-ffi wheel, and the other
    # bindings generate from it.
    [py]='^(py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/)'
    [kt]='^(kt/|sh/kt/|rs/moq-ffi/)'
    [swift]='^(swift/|sh/swift/|rs/moq-ffi/)'
    [go]='^(go/|sh/go/|rs/moq-ffi/)'
    [dart]='^(dart/|sh/dart/|rs/moq-ffi/)'
    # The plugin calls libmoq through its generated header, and flake.nix owns
    # the libobs headers it compiles against.
    [obs_compile]='^(cpp/obs/|sh/obs/|rs/libmoq/|flake\.nix$)'
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

# The tools each module needs. A module missing one is skipped locally, so an
# incomplete toolchain checks less; under MOQ_STRICT (CI) it is an error, up
# front, because there a skip is indistinguishable from a pass. swift needs
# none: it skips off macOS by design, and swift.yml is its real gate.
declare -A tools=(
    [js]='bun python3'
    [workers]='bun'
    [rs]='cargo jq'
    [bench]='cargo'
    [drafts]='bun kramdown-rfc xml2rfc'
    [quest]='cargo'
    [py]='uv'
    [kt]='cargo gradle java'
    [swift]=''
    [go]='cargo go uniffi-bindgen-go'
    [dart]='cargo dart uniffi_bindgen_dart'
    [obs_compile]='cargo jq pkg-config'
    [obs]='clang-format cmake gersemi jq'
    [flake]='nix'
    [markdown]='bun'
    [shell]='shellcheck shfmt'
    [toml]='taplo'
    [nix]='nixfmt'
    [justfile]=''
    [gh]='actionlint bun'
)

case "$action" in
    check) modules=(js workers drafts rs bench quest py kt swift go dart obs_compile obs flake markdown shell toml nix justfile gh) ;;
    fix) modules=(js rs py dart obs markdown shell toml nix justfile) ;;
    test) modules=(js rs py) ;;
esac

selected=()
missing=()
for module in "${modules[@]}"; do
    pattern=${scope[$module]}
    if [[ -z "$all" && -n "$pattern" ]] && ! grep -qE "$pattern" "$changed"; then
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

if ((${#missing[@]})); then
    echo "error: MOQ_STRICT is set but these tools are missing: $(printf '%s\n' "${missing[@]}" | sort -u | tr '\n' ' ')" >&2
    echo "       run inside 'nix develop', or unset MOQ_STRICT to skip what isn't installed" >&2
    exit 1
fi

# rs selects crates from the same list, or the whole workspace.
rs_list=$changed
[[ -z "$all" ]] || rs_list=--all

# Tracked and new files, skipping whatever .gitignore does (node_modules, target).
files() {
    git ls-files -z --cached --others --exclude-standard -- "$@"
}

for module in "${selected[@]}"; do
    case "$action:$module" in
        check:js) just js check ;;
        check:workers) just js workers ;;
        check:rs) just rs check-changed "$rs_list" ;;
        check:bench) cargo check --locked --package moq-relay --package moq-bench --features moq-relay/io-uring ;;
        check:quest) cargo run --quiet --locked --package quest -- check ;;
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
        test:rs) just rs test-changed "$rs_list" ;;
        *:shell) sh/shell.sh "$action" ;;
        *) just "$module" "$action" ;;
    esac
done
