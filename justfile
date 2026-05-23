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

# Comprehensive CI: per-language checks + cross-cutting smoke tests.
ci:
	#!/usr/bin/env bash
	set -euo pipefail

	just check --workspace

	# Per-language extended checks.
	just py check
	just rs ci
	just kt check
	just swift check

	# Cross-cutting: nix, multi-language tests, build.
	nix flake check
	just test --all-features
	just build

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
