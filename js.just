# Install JS dependencies.
install:
	bun install

# Run the JS checks.
check:
	#!/usr/bin/env bash
	set -euo pipefail
	bun install --frozen-lockfile
	if tty -s; then
		bun run --filter='*' --elide-lines=0 check
	else
		bun run --filter='*' check
	fi

# Run the JS tests.
test:
	#!/usr/bin/env bash
	set -euo pipefail
	bun install --frozen-lockfile
	if tty -s; then
		bun run --filter='*' --elide-lines=0 test
	else
		bun run --filter='*' test
	fi

# Fix JS issues.
fix:
	bun install

# Build JS packages.
build:
	bun run --filter='*' build

# Update JS dependencies.
update:
	bun update
	bun outdated
