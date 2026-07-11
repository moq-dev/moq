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

# Fast inner-loop checks. Optional shell, workflow, TOML, Nix, and justfile lints skip if missing.
check *args:
    just js check
    just rs check {{ args }}
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

    # Validate the flake (eval + dev shell build) via `nix flake check`. This no
    # longer compiles the workspace -- the heavy Rust CI (clippy/doc/test) moved
    # to `just rs ci` (plain cargo) and `checks` is unwired (see flake.nix) -- so
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

# Auto-fix linting/formatting issues; optional tools skip if missing locally.
fix:
    just js fix
    just rs fix
    just py fix
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

    # Reclaim Nix store space too, if Nix is installed.
    if command -v nix-collect-garbage &> /dev/null; then nix-collect-garbage -d; fi

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
