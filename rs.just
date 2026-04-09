# Install Rust development tools.
install:
	cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit cargo-sweep cargo-semver-checks release-plz

# Run the Rust checks.
check:
	cargo check --all-targets
	cargo clippy --all-targets -- -D warnings
	cargo fmt --all --check
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
	cargo shear
	cargo sort --workspace --check

# Run the Rust tests.
test *args:
	cargo test --all-targets {{ args }}

# Fix Rust issues.
fix:
	#!/usr/bin/env bash
	set -euo pipefail
	cargo clippy --fix --allow-staged --allow-dirty --all-targets
	cargo fmt --all
	cargo shear --fix
	cargo sort --workspace
	if command -v cargo-sweep &> /dev/null; then cargo sweep --time 3; fi

# Build Rust packages.
build:
	cargo build

# Update Rust dependencies.
update:
	cargo update
	cargo upgrade --incompatible

# Check feature edge cases and dry-run publish for CI.
ci:
	cargo check --workspace --no-default-features --exclude moq-ffi
	cargo check --workspace --all-features --exclude moq-ffi
	cargo publish --workspace --dry-run --exclude libmoq --exclude moq-ffi

# Check semver compatibility against crates.io.
# libmoq is an internal C-ABI crate and is intentionally excluded.
semver:
	cargo semver-checks check-release --workspace --exclude libmoq

# Update versions and changelogs via release-plz.
bump:
	release-plz update

# Create release PRs and publish crates.
release:
	release-plz release-pr --git-token "$GITHUB_TOKEN"
	release-plz release --git-token "$GITHUB_TOKEN"
