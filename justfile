#!/usr/bin/env just --justfile

# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation


mod rs
mod js
mod demo
mod cdn

# Shortcuts to avoid `demo::` prefix.
mod boy 'demo/boy'
mod pub 'demo/pub'
mod relay 'demo/relay'
mod web 'demo/web'

# Run the demo by default.
default:
	just demo

# Alias for `just demo`.
dev:
	just demo

# Install any dependencies.
install:
	just js install
	just rs install

# Run the CI checks
check:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the Javascript checks.
	just js check
	bun biome check

	# Run the Markdown checks.
	bun remark . --quiet --frail

	# Run the (slower) Rust checks.
	just rs check

	# Run the Python checks.
	if command -v uv &> /dev/null; then
		uv run ruff check py/
		uv run ruff format --check py/
		uv run --package moq-lite pyright
	fi

	# Only run the tofu checks if tofu is installed.
	if command -v tofu &> /dev/null; then (cd cdn && just check); fi

	# Only run the nix checks if nix is installed.
	if command -v nix &> /dev/null; then nix flake check; fi

# Run comprehensive CI checks including feature edge cases
ci:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the standard checks first
	just check

	# Run the unit tests with all features to exercise all QUIC backends
	just test --all-features

	# Make sure everything builds
	just build

	# Check feature edge cases for all crates
	just rs ci

# Run the unit tests
test *args:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the Javascript tests.
	just js test

	# Run the (slower) Rust tests.
	just rs test {{ args }}

	# Run the Python tests.
	if command -v uv &> /dev/null; then
		uv run maturin develop -m rs/moq-ffi/Cargo.toml --uv
		uv run --package moq-lite pytest py/moq-lite/tests/
	fi

# Automatically fix some issues.
fix:
	#!/usr/bin/env bash
	set -euo pipefail

	# Fix the Javascript issues.
	just js fix
	bun biome check --write

	# Fix the Markdown issues.
	bun remark . --quiet --output

	# Fix the Rust issues.
	just rs fix

	# Fix the Python issues.
	if command -v uv &> /dev/null; then uv run ruff check --fix py/ && uv run ruff format py/; fi

	if command -v tofu &> /dev/null; then (cd cdn && just fix); fi

# Upgrade any tooling
update:
	#!/usr/bin/env bash
	set -euo pipefail

	just js update
	just rs update

	# Update the Nix flake.
	if command -v nix &> /dev/null; then nix flake update; fi

# Build the packages
build:
	#!/usr/bin/env bash
	set -euo pipefail

	just js build
	just rs build

	# Build moq-ffi from source into py/moq-lite's venv.
	if command -v uv &> /dev/null; then
		(cd py/moq-lite && uv run maturin develop -m ../../rs/moq-ffi/Cargo.toml --uv)
	fi

# Serve the documentation locally.
doc:
	cd doc && bun run dev
