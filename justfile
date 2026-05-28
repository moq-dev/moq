#!/usr/bin/env just --justfile
# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation
# Per-language modules. Anything that's specific to one language lives in

# its own justfile; the recipes below orchestrate across them.
mod js
mod rs
mod py
mod kt
mod swift
mod go
# Demos and infra.
mod demo
mod infra
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

# Install repo-wide tooling. Per-language deps install on first invocation

# of `just <lang> check`.
install:
    bun install
    cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit cargo-sweep cargo-semver-checks release-plz

# Fast inner-loop checks. Runs JS, Rust, and Markdown lints.
# Shell + workflow + TOML + Nix + justfile lints skip silently if their
# binaries aren't on $PATH; `nix develop` provides them, and `just ci`

# requires them.
check *args:
    just js check
    just rs check {{ args }}
    bun remark . --quiet --frail
    @if command -v shellcheck >/dev/null 2>&1 && command -v shfmt >/dev/null 2>&1; then shfmt --diff $(shfmt -f .) && shellcheck $(shfmt -f .); fi
    @if command -v taplo >/dev/null 2>&1; then RUST_LOG=error taplo format --check; fi
    @if command -v nixfmt >/dev/null 2>&1; then nixfmt --check $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*'); fi
    @for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*'); do just --fmt --unstable --check --justfile "$f"; done
    just gh check

# Run every per-language `ci` with the diff vs BASE; each greps for its
# own scope and skips when nothing relevant changed. Pass BASE="" to

# default to $GITHUB_BASE_REF (CI) or origin/main (local).
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

    # Cheap; always run. `bun install` is needed for remark-cli, since
    # `just js ci` (where bun deps would otherwise install) is skipped
    # when the diff has no JS-scoped files.
    nix flake check
    bun install --frozen-lockfile
    bun remark . --quiet --frail
    shfmt --diff $(shfmt -f .)
    shellcheck $(shfmt -f .)
    RUST_LOG=error taplo format --check
    nixfmt --check $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*')
    for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*'); do just --fmt --unstable --check --justfile "$f"; done
    just gh ci

# Auto-fix linting/formatting issues across all languages.

# shfmt / taplo / nixfmt / just --fmt skipped silently if missing locally.
fix:
    just js fix
    just rs fix
    just py fix
    bun remark . --quiet --output
    @if command -v shfmt >/dev/null 2>&1; then shfmt --write $(shfmt -f .); fi
    @if command -v taplo >/dev/null 2>&1; then RUST_LOG=error taplo format; fi
    @if command -v nixfmt >/dev/null 2>&1; then nixfmt $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*'); fi
    @for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*'); do just --fmt --unstable --justfile "$f"; done

# Run unit tests for every language.
test *args:
    just js test
    just rs test {{ args }}
    if command -v uv &> /dev/null; then just py test; fi

# Build the packages.
build:
    just js build
    just rs build
    if command -v uv &> /dev/null; then just py build; fi

# Upgrade any tooling
update:
    just js update
    just rs update
    nix flake update

# Serve the documentation locally.
doc:
    cd doc && bun run dev
