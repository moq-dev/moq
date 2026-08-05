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

    # Resolve BASE: arg > upstream > origin/main. A branch's upstream is the
    # branch it merges into, which is the base a `dev`-targeted branch needs.
    # `git push -u` repoints upstream at the branch's own remote copy, which
    # would diff HEAD against itself, so ignore that case (see CLAUDE.md).
    base="$BASE"
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

# Fast inner-loop checks. Lints and compiles only the packages the branch
# changed plus everything depending on them, so several worktrees can build at
# once. `check-all` is the unscoped suite.

# Lint and compile what the branch changed since BASE, plus its dependents.
check $BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

    files=$(just _changed "$BASE")

    # An empty list means "force-run" to the per-lang recipes, which is the
    # wrong semantic here, so don't dispatch at all.
    if [[ -n "$files" ]]; then
    	just js check "$files"
    	just rs check-changed "$files"
    	# The OBS plugin has no compile job in PR CI, so its lint + CMake
    	# guards are the only automated coverage it gets.
    	if echo "$files" | grep -q '^cpp/obs/'; then
    		just obs check
    	fi
    else
    	echo "check: nothing changed."
    fi

    just _check-common

# Check every JavaScript workspace and every default Rust member.
check-all *args:
    just js check
    just rs check {{ args }}
    just obs check
    just _check-common

# Repository-wide non-compiling checks shared by `check` and `check-all`.
# Optional shell, workflow, TOML, Nix, and justfile lints skip if missing.
#
# `bun install` because remark-cli lives in node_modules and `just js check` is
# where it would otherwise be installed, which a Rust-only diff skips.

# Repository-wide lints, shared by `check` and `check-all`.
[private]
_check-common:
    bun install --frozen-lockfile
    bun remark . --quiet --frail
    @if command -v shellcheck >/dev/null 2>&1 && command -v shfmt >/dev/null 2>&1; then shfmt --diff $(shfmt -f . | grep -v '\.direnv/') && shellcheck $(shfmt -f . | grep -v '\.direnv/'); fi
    @if command -v taplo >/dev/null 2>&1; then RUST_LOG=error taplo format --check; fi
    @if command -v nixfmt >/dev/null 2>&1; then nixfmt --check $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); fi
    @for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); do just --fmt --check --justfile "$f"; done
    just gh check

# Run per-language CI against BASE, skipping scopes with no relevant diff.
ci BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

    # Resolve BASE: arg > $GITHUB_BASE_REF > origin/main.
    if [[ -n "{{ BASE }}" ]]; then
    	base="{{ BASE }}"
    elif [[ -n "${GITHUB_BASE_REF:-}" ]]; then
    	base="origin/${GITHUB_BASE_REF}"
    else
    	base="origin/main"
    fi

    # One git diff for the whole run; pass the file list to each per-lang.
    merge_base=$(git merge-base "$base" HEAD) || {
    	echo "error: cannot resolve merge-base against $base (is full history fetched?)" >&2
    	exit 1
    }
    files=$(git diff --name-only "$merge_base")

    # Skip per-lang dispatch when nothing changed (empty FILES means
    # "force-run" to per-lang, which is the wrong semantic here).
    if [[ -n "$files" ]]; then
    	just js    ci "$files"
    	just rs    ci "$files"
    	just py    ci "$files"
    	just kt    ci "$files"
    	just swift ci "$files"
    	just go    ci "$files"
    fi

    # The OBS plugin has no compile job in PR CI, so its lint + CMake guards
    # are the only automated coverage it gets. Empty $files is a force-run,
    # so run then.
    if [[ -z "$files" ]] || echo "$files" | grep -q '^cpp/obs/'; then
    	just obs check
    else
    	echo "ci: no cpp/obs changes; skipping obs check."
    fi

    # Validate the flake (eval + dev shell build) via `nix flake check`. This no
    # longer compiles the workspace -- the heavy Rust CI (clippy/doc/test) moved
    # to `just rs ci` (plain cargo), leaving only lightweight Nix checks -- so
    # it's cheap. Gate it to Nix/Rust input changes anyway: a pure doc/JS PR
    # can't affect flake eval. Empty $files is a force-run, so run then.
    if [[ -z "$files" ]] || echo "$files" | grep -qE '(^rs/|^Cargo\.(toml|lock)$|^flake\.lock$|\.nix$)'; then
    	nix flake check
    else
    	echo "ci: no Nix/Rust inputs changed; skipping nix flake check."
    fi

    # Cheap; always run. `bun install` is needed for remark-cli, since
    # `just js ci` (where bun deps would otherwise install) is skipped
    # when the diff has no JS-scoped files.
    bun install --frozen-lockfile
    bun remark . --quiet --frail
    shfmt --diff $(shfmt -f . | grep -v '\.direnv/')
    shellcheck $(shfmt -f . | grep -v '\.direnv/')
    RUST_LOG=error taplo format --check
    nixfmt --check $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*')
    for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); do just --fmt --check --justfile "$f"; done
    just gh ci

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
    	if echo "$files" | grep -q '^cpp/obs/'; then
    		just obs fix
    	fi
    else
    	echo "fix: nothing changed."
    fi

    just py fix
    just _fix-common

# Auto-fix every JavaScript workspace and every default Rust member.
fix-all:
    just js fix
    just rs fix
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
    @if command -v shfmt >/dev/null 2>&1; then shfmt --write $(shfmt -f . | grep -v '\.direnv/'); fi
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
