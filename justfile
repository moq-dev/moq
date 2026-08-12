#!/usr/bin/env just --justfile
# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation

set unstable

# Per-language modules. Language-specific recipes live in their own justfiles.
mod js
mod rs
mod py
mod kt
mod swift
mod go
# OBS Studio plugin (C++). See doc/bin/obs.md.
mod obs 'cpp/obs'
# Unit tests per language (`just test`).
mod test
# Demos and infra.
mod demo
mod infra
# IETF Internet-Drafts (`just drafts build`, `just drafts publish`).
mod drafts
# GitHub Actions workflow linting.
mod gh '.github'
# Shortcuts to avoid `demo::` prefix.
mod boy 'demo/boy'
mod pub 'demo/pub'
mod relay 'demo/relay'
mod sub 'demo/sub'
mod web 'demo/web'

# Run the demo by default.
default:
    just demo

# Alias for `just demo`.
dev:
    just demo

# Install repo-wide tooling. Per-language deps install on first check.
install:
    bun install
    cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit cargo-semver-checks release-plz

# Reports the base it picked on stderr, so a surprising scope is traceable.

# Print the files this branch changed relative to BASE, one per line.
[private]
_changed $BASE:
    #!/usr/bin/env bash
    set -euo pipefail

    # Resolve BASE: arg > $GITHUB_BASE_REF > upstream > origin/main. A branch's
    # upstream is the branch it merges into, which is the base a `dev`-targeted
    # branch needs. `git push -u` repoints upstream at the branch's own remote
    # copy, which would diff HEAD against itself, so ignore that case (see
    # CLAUDE.md). GITHUB_BASE_REF outranks the upstream because a PR checkout
    # has no upstream configured, and the branch being merged into is exactly
    # the base GitHub is asking about.
    base="$BASE"
    if [[ -z "$base" && -n "${GITHUB_BASE_REF:-}" ]]; then
    	base="origin/${GITHUB_BASE_REF}"
    fi
    if [[ -z "$base" ]]; then
    	base=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null || true)
    	if [[ -z "$base" || "$base" == */"$(git branch --show-current)" ]]; then
    		base="origin/main"
    	fi
    fi

    merge_base=$(git merge-base "$base" HEAD) || {
    	echo "error: cannot resolve merge-base against $base (is full history fetched?)" >&2
    	exit 1
    }
    echo "base: $base" >&2

    # Untracked files count too: a brand new crate or module is the whole change.
    {
    	git diff --name-only "$merge_base"
    	git ls-files --others --exclude-standard
    } | sort -u

# Tools every scope guards with `command -v`, so an incomplete local toolchain
# checks less instead of failing. That trade is wrong in CI, where a skip is
# indistinguishable from a pass, so CI exports MOQ_STRICT=1 and this turns the
# required set into a precondition. Checked up front, and as one list, so a
# missing tool is reported before a long compile rather than after it.
#
# Required per scope, mirroring what `check` actually dispatches for a given
# diff: demanding gradle on a docs-only PR would fail a run that was never going
# to invoke it. Takes the same file list as the dispatch, or `ALL` to require
# everything (`check-all`).
#
# Two deliberate absences:
#   - swift exists only on macOS, and `swift check` skips off-macOS by design;
#     swift.yml is its real gate.
#   - go and uniffi-bindgen-go are NOT in the dev shell (uniffi-bindgen-go isn't
#     in nixpkgs; it installs from a NordSecurity git tag). Requiring them would
#     fail every Go-scoped PR, so `just go check` still skips itself in CI, as it
#     always has. Packaging them is what would close that hole.

# Fail when a tool the diff's scopes need is missing. No-op unless MOQ_STRICT.
[private]
_tools $FILES="":
    #!/usr/bin/env bash
    set -euo pipefail
    [[ -n "${MOQ_STRICT:-}" ]] || exit 0

    scoped() { [[ "$FILES" == ALL ]] || grep -qE "$1" <<< "$FILES"; }

    # `_check-common` runs on every invocation, so its tools are unconditional.
    tools=(actionlint bun jq nix nixfmt shellcheck shfmt taplo)
    scoped '^(rs/|Cargo\.(toml|lock)$|rust-toolchain\.toml$)' && tools+=(cargo)
    scoped '^(py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/)'     && tools+=(uv)
    scoped '^(kt/|rs/moq-ffi/)'                                && tools+=(gradle java)
    # The OBS lints ship only in the Linux dev shell; nixpkgs marks obs-studio
    # broken on Darwin.
    if [[ "$(uname -s)" == "Linux" ]] && scoped '^cpp/obs/'; then
    	tools+=(clang-format gersemi)
    fi

    missing=()
    for tool in "${tools[@]}"; do
    	command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
    done
    if ((${#missing[@]})); then
    	echo "error: MOQ_STRICT is set but these tools are missing: ${missing[*]}" >&2
    	echo "       run inside 'nix develop', or unset MOQ_STRICT to skip what isn't installed" >&2
    	exit 1
    fi

# Lints and compiles only the packages the branch changed plus everything
# depending on them, so several worktrees can build at once. This is also what
# CI runs (with MOQ_STRICT=1), so there is no second, drifting definition of
# "checked". Tests are the sibling `just test`; `check-all` is the unscoped suite.

# Lint and compile what the branch changed since BASE, plus its dependents.
check $BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

    files=$(just _changed "$BASE")
    just _tools "$files"

    # The dispatch below lives in these two files, and neither matches any
    # language scope, so a PR that rewrites how CI dispatches would otherwise
    # validate none of it. Hand off to the unscoped suite instead.
    if grep -qE '^(justfile|test/justfile)$' <<< "$files"; then
    	echo "check: root orchestration changed; checking everything." >&2
    	just check-all
    	exit 0
    fi

    # An empty list means "force-run" to the per-lang recipes, which is the
    # wrong semantic here, so don't dispatch at all.
    if [[ -n "$files" ]]; then
    	just js check "$files"
    	just rs check-changed "$files"
    	just py check "$files"
    	just kt check "$files"
    	just swift check "$files"
    	just go check "$files"
    	# The OBS plugin has no compile job in PR CI, so its lint + CMake
    	# guards are the only automated coverage it gets.
    	if echo "$files" | grep -q '^cpp/obs/'; then
    		just obs check
    	fi
    	# Validates flake eval + dev shell build; it no longer compiles the
    	# workspace, so it's cheap. Gated anyway: a pure doc/JS PR can't
    	# affect flake eval.
    	if echo "$files" | grep -qE '(^rs/|^Cargo\.(toml|lock)$|^flake\.lock$|\.nix$)'; then
    		just _flake
    	fi
    else
    	echo "check: nothing changed."
    fi

    just _check-common

# Check every package in every language, plus moq-wasm.
check-all *args:
    just _tools ALL
    just js check
    just rs check --workspace {{ args }}
    # Not covered by the line above: moq-wasm only exists on the wasm32 target.
    just rs wasm
    just py check
    just kt check
    just swift check
    just go check
    just obs check
    just _flake
    just _check-common

# Skips when nix is absent: the flake is not a precondition for working on the
# repo, and `_tools` already makes it required under MOQ_STRICT.

# Validate flake evaluation and the dev shell build.
[private]
_flake:
    @if command -v nix >/dev/null 2>&1; then nix flake check; fi

# Repository-wide non-compiling checks shared by `check` and `check-all`.
# Optional shell, workflow, TOML, Nix, and justfile lints skip if missing.
#
# `bun install` because remark-cli lives in node_modules and `just js check` is
# where it would otherwise be installed, which a Rust-only diff skips.

# Run shell checks or formatting over tracked files that exist in the worktree.
[private]
_shell $ACTION:
    #!/usr/bin/env bash
    set -euo pipefail

    if ! command -v shfmt >/dev/null 2>&1; then
        exit 0
    fi
    if [[ "$ACTION" == "check" ]] && ! command -v shellcheck >/dev/null 2>&1; then
        exit 0
    fi

    scripts_file=$(mktemp)
    trap 'rm -f "$scripts_file"' EXIT
    shfmt -f=0 . > "$scripts_file"

    scripts=()
    while IFS= read -r -d '' file; do
        if git --literal-pathspecs ls-files --error-unmatch -- "$file" >/dev/null 2>&1; then
            scripts+=("$file")
        fi
    done < "$scripts_file"
    ((${#scripts[@]})) || exit 0

    case "$ACTION" in
        check)
            shfmt --diff "${scripts[@]}"
            shellcheck "${scripts[@]}"
            ;;
        fix)
            shfmt --write "${scripts[@]}"
            ;;
        *)
            echo "invalid shell action: $ACTION" >&2
            exit 2
            ;;
    esac

# Repository-wide lints, shared by `check` and `check-all`.
[private]
_check-common:
    bun install --frozen-lockfile
    bun remark . --quiet --frail
    just _shell check
    @if command -v taplo >/dev/null 2>&1; then RUST_LOG=error taplo format --check; fi
    @if command -v nixfmt >/dev/null 2>&1; then nixfmt --check $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); fi
    @for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); do just --fmt --check --justfile "$f"; done
    just gh check

# Scoped exactly like `check`, because `clippy --fix` compiles what it fixes.
# `fix-all` is the unscoped version.

# Auto-fix lint and formatting for what the branch changed since BASE.
fix $BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

    files=$(just _changed "$BASE")

    if [[ -n "$files" ]]; then
    	just js fix "$files"
    	just rs fix-changed "$files"
    	just py fix "$files"
    	if echo "$files" | grep -q '^cpp/obs/'; then
    		just obs fix
    	fi
    else
    	echo "fix: nothing changed."
    fi

    just _fix-common

# Auto-fix every JavaScript workspace and every default Rust member.
fix-all:
    just js fix
    just rs fix --workspace
    just py fix
    just obs fix
    just _fix-common

# Optional tools skip if missing locally. `bun install` for the same reason as
# `_check-common`.

# Repository-wide fixes, shared by `fix` and `fix-all`.
[private]
_fix-common:
    bun install
    bun remark . --quiet --output
    just _shell fix
    @if command -v taplo >/dev/null 2>&1; then RUST_LOG=error taplo format; fi
    @if command -v nixfmt >/dev/null 2>&1; then nixfmt $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); fi
    @for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); do just --fmt --justfile "$f"; done

# Build the packages.
build:
    just js build
    just rs build
    if command -v uv &> /dev/null; then just py build; fi
    if command -v wasm-bindgen &> /dev/null; then just wasm; fi

# Build browser/WASM bindings into @moq/wasm using the pinned wasm-bindgen toolchain.
wasm:
    cargo build -p moq-wasm --target wasm32-unknown-unknown --profile wasm-release
    wasm-bindgen --target web --out-name moq \
    	--out-dir js/wasm/dist "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/wasm-release/moq_wasm.wasm"

# Delete build artifacts and caches, including per-language outputs and agent worktrees.
clean:
    #!/usr/bin/env bash
    set -euo pipefail

    just rs clean
    just js clean
    just py clean
    just kt clean
    just swift clean
    just go clean

    # Caches not owned by any one language: nix build result, direnv, wrangler.
    rm -rf result .direnv
    find . -name .claude -prune -o -type d -name .wrangler -prune -exec rm -rf {} +

    # Agent worktrees each carry their own artifacts now that the shared
    # target dir is gone. Worktrees don't nest, so this recurses exactly one
    # level. Tolerate stale worktrees on branches that predate this recipe.
    for wt in .claude/worktrees/*/; do
    	[ -f "${wt}justfile" ] || continue
    	echo "==> cleaning ${wt}"
    	(cd "$wt" && just clean) || echo "    (skipped: just clean failed in ${wt})"
    done

# Upgrade any tooling
update:
    just js update
    just rs update
    nix flake update

# Serve the documentation locally.
doc:
    cd doc && bun run dev
