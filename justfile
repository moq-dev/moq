#!/usr/bin/env just --justfile

# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation


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
	bun install
	cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit cargo-sweep cargo-semver-checks release-plz

# Run the CI checks
check *args:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the Javascript checks.
	bun install --frozen-lockfile
	if tty -s; then
		bun run --filter='*' --elide-lines=0 check
	else
		bun run --filter='*' check
	fi
	bun biome check

	# Run the Markdown checks.
	bun remark . --quiet --frail

	# Run the (slower) Rust checks.
	cargo check --all-targets {{ args }}
	cargo clippy --all-targets {{ args }} -- -D warnings
	cargo fmt --all --check

	# Check documentation warnings (default-members only, not dependencies)
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps {{ args }}

	# requires: cargo install cargo-shear
	cargo shear

	# requires: cargo install cargo-sort
	cargo sort --workspace --check

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

	# Run the standard checks first, including non-default workspace members
	just check --workspace

	# Run the unit tests with all features to exercise all QUIC backends
	just test --all-features

	# Make sure everything builds
	just build

	# Check feature edge cases for all crates
	cargo check --workspace --no-default-features
	cargo check --workspace --all-features

	# Dry-run publish to verify packaging
	cargo publish --dry-run

# Check semver compatibility against crates.io (default-members only)
# requires: cargo install cargo-semver-checks
semver:
	cargo semver-checks check-release

# Update versions and changelogs via release-plz
bump:
	release-plz update

# Create release PRs and publish crates
release:
	release-plz release-pr --git-token "$GITHUB_TOKEN"
	release-plz release --git-token "$GITHUB_TOKEN"

# Run the unit tests
test *args:
	#!/usr/bin/env bash
	set -euo pipefail

	# Run the Javascript tests.
	bun install --frozen-lockfile
	if tty -s; then
		bun run --filter='*' --elide-lines=0 test
	else
		bun run --filter='*' test
	fi

	cargo test --all-targets {{ args }}

	# Run the Python tests.
	if command -v uv &> /dev/null; then
		uv run maturin develop -m rs/moq-ffi/Cargo.toml --uv
		uv run --package moq-lite pytest py/moq-lite/tests/
	fi

# Automatically fix some issues.
fix:
	# Fix the Javascript dependencies.
	bun install
	bun biome check --write

	# Fix the Markdown issues.
	bun remark . --quiet --output

	# Fix the Rust issues.
	cargo clippy --fix --allow-staged --allow-dirty --all-targets
	cargo fmt --all

	# requires: cargo install cargo-shear
	cargo shear --fix

	# requires: cargo install cargo-sort
	cargo sort --workspace

	# Fix the Python issues.
	if command -v uv &> /dev/null; then uv run ruff check --fix py/ && uv run ruff format py/; fi

	if command -v tofu &> /dev/null; then (cd cdn && just fix); fi

	# Remove old build artifacts to save disk space.
	if command -v cargo-sweep &> /dev/null; then cargo sweep --time 3; fi

# Upgrade any tooling
update:
	bun update
	bun outdated

	# Update any patch versions
	cargo update

	# Requires: cargo install cargo-upgrades cargo-edit
	cargo upgrade --incompatible

	# Update the Nix flake.
	nix flake update

# Build the packages
build:
	#!/usr/bin/env bash
	set -euo pipefail

	bun run --filter='*' build
	cargo build

	# Build moq-ffi from source into py/moq-lite's venv.
	if command -v uv &> /dev/null; then
		(cd py/moq-lite && uv run maturin develop -m ../../rs/moq-ffi/Cargo.toml --uv)
	fi

# Serve the documentation locally.
doc:
	cd doc && bun run dev
