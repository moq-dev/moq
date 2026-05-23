#!/usr/bin/env just --justfile

# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation


# Per-language modules. Anything that's specific to one language lives in
# its own justfile; the recipes below orchestrate across them.
mod js
mod rs
mod py
mod kt
mod swift

# Demos and infra.
mod demo

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
check *args:
	just js check
	just rs check {{ args }}
	bun remark . --quiet --frail

# Run every per-language `ci` unconditionally; each self-gates against
# BASE and exits 0 fast when its scope hasn't changed. Pass BASE="" to
# force-run everything.
#
# Computes the changed file list once and exports it as $JUST_CHANGED_FILES
# so per-language `changed` recipes can reuse it instead of each running
# their own git diff.
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

	# One git diff for the whole run. If merge-base fails (e.g. shallow
	# clone, unreachable base), leave $JUST_CHANGED_FILES unset so each
	# per-language `changed` recipe falls back to its own conservative
	# behavior (force-run).
	base_unreachable=false
	if merge_base=$(git merge-base "$base" HEAD 2>/dev/null); then
		export JUST_CHANGED_FILES=$(git diff --name-only "$merge_base")
	else
		echo "warning: $base unreachable; force-running everything" >&2
		base_unreachable=true
	fi

	just js    ci "$base"
	just rs    ci "$base"
	just py    ci "$base"
	just kt    ci "$base"
	just swift ci "$base"

	# nix flake + markdown have no per-language module; gate inline
	# against the same file list.
	if $base_unreachable; then
		nix flake check
		bun remark . --quiet --frail
	else
		if echo "$JUST_CHANGED_FILES" | grep -qE '^(flake\.nix$|flake\.lock$)'; then nix flake check; fi
		if echo "$JUST_CHANGED_FILES" | grep -qE '\.md$'; then bun remark . --quiet --frail; fi
	fi

# Auto-fix linting/formatting issues across all languages.
fix:
	just js fix
	just rs fix
	just py fix
	bun remark . --quiet --output

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
