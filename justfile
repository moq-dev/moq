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

# Print the language scopes touched vs $GITHUB_BASE_REF (CI) or origin/main (local).
changed:
	#!/usr/bin/env bash
	set -euo pipefail

	# Resolve the diff base.
	if [[ -n "${GITHUB_BASE_REF:-}" ]]; then
		base="origin/${GITHUB_BASE_REF}"
	else
		base="origin/main"
	fi

	if ! git rev-parse --verify --quiet "${base}" >/dev/null; then
		echo "warning: ${base} not available; emitting all scopes" >&2
		echo "js rs py kt swift nix md"
		exit 0
	fi

	# Diff against the merge-base with the base ref. This captures
	# committed branch changes + staged + unstaged working tree edits,
	# so `just ci` locally reflects what you're about to push.
	if ! merge_base=$(git merge-base "${base}" HEAD 2>/dev/null); then
		echo "warning: no common ancestor with ${base}; emitting all scopes" >&2
		echo "js rs py kt swift nix md"
		exit 0
	fi
	files=$(git diff --name-only "${merge_base}")

	# If the diff is empty (e.g. branch is even with base), emit nothing.
	if [[ -z "$files" ]]; then
		exit 0
	fi

	# Changes to orchestration (root justfile, per-language justfiles, or
	# either of the check.yml / check-swift.yml workflows) fan out to
	# every scope.
	if echo "$files" | grep -qE '^(justfile|[^/]+/justfile|\.github/workflows/check(-swift)?\.yml)$'; then
		echo "js rs py kt swift nix md"
		exit 0
	fi

	scopes=()
	if echo "$files" | grep -qE '^(js/|package\.json$|bun\.lock$|bun\.lockb$|biome\.jsonc$)'; then
		scopes+=(js)
	fi
	if echo "$files" | grep -qE '^(rs/|Cargo\.toml$|Cargo\.lock$)'; then
		scopes+=(rs)
	fi
	if echo "$files" | grep -qE '^(py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/)'; then
		scopes+=(py)
	fi
	if echo "$files" | grep -qE '^(kt/|rs/moq-ffi/)'; then
		scopes+=(kt)
	fi
	if echo "$files" | grep -qE '^(swift/|rs/moq-ffi/)'; then
		scopes+=(swift)
	fi
	if echo "$files" | grep -qE '^(flake\.nix$|flake\.lock$)'; then
		scopes+=(nix)
	fi
	if echo "$files" | grep -qE '\.md$'; then
		scopes+=(md)
	fi

	echo "${scopes[*]}"

# Run per-language `ci` recipes for whichever scopes `just changed` reports.
ci:
	#!/usr/bin/env bash
	set -euo pipefail

	scopes=$(just changed)
	if [[ -z "$scopes" ]]; then
		echo "No language scopes changed; nothing to do."
		exit 0
	fi
	echo "Running CI for scopes: $scopes"

	has() { [[ " $scopes " == *" $1 "* ]]; }

	if has js;    then just js ci;    fi
	if has rs;    then just rs ci;    fi
	if has py;    then just py ci;    fi
	if has kt;    then just kt ci;    fi
	if has swift; then just swift ci; fi
	if has nix;   then nix flake check; fi
	if has md;    then bun remark . --quiet --frail; fi

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
