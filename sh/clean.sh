#!/usr/bin/env bash
# Delete build artifacts and caches: sh/clean.sh [here|all]
#
# Only this checkout by default. Agent worktrees each carry their own
# artifacts, and another agent is usually building in one: `cargo clean` under
# a running build fails it, and there is no telling a finished worktree from a
# busy one from out here. `all` is the explicit opt-in for an idle machine.
#
# Source is never touched, dirty or untracked: this deletes build output, not
# work. Nothing here reaches a machine-wide store (Nix, cargo/bun/uv home
# caches), because those are shared with every other checkout.
set -euo pipefail

scope=${1:-here}
case "$scope" in
    here | all) ;;
    *)
        echo "usage: sh/clean.sh [here|all]" >&2
        exit 2
        ;;
esac

cd "$(git rev-parse --show-toplevel)"

# Rust.
cargo clean

# JS: the bun workspace spans the repo, so sweep from the root.
find . -name .claude -prune -o \
    -type d \( -name node_modules -o -name dist -o -name out -o -name pkg \) \
    -prune -exec rm -rf {} +
find . -name .claude -prune -o -type f -name '*.tsbuildinfo' -exec rm -f {} +

# Python: the venv, release dist, bytecode, and the bindings maturin drops in.
rm -rf .venv py/.venv py/dist py/moq-ffi/moq_ffi/_uniffi
find py -name .claude -prune -o -type d -name __pycache__ -prune -exec rm -rf {} +
find py -name .claude -prune -o -type f -name '*.pyc' -exec rm -f {} +

# Kotlin: gradle output plus the bindings and native libs sh/kt generates.
find kt -type d \( -name build -o -name .gradle -o -name .kotlin \) -prune -exec rm -rf {} +
rm -rf kt/local.properties \
    kt/moq-ffi/src/jvmAndAndroidMain/kotlin/uniffi \
    kt/moq-ffi/src/jvmMain/resources \
    kt/moq-ffi/src/androidMain/jniLibs

# Swift: SwiftPM output plus the XCFramework and bindings sh/swift lays down.
rm -rf swift/.build swift/.swiftpm swift/Package.resolved \
    swift/Sources/MoqFFI/Generated.swift swift/MoqFFI.xcframework

# Go: the gitignored bindings, native libs, and vendoring in go/.
rm -rf go/ffi/moq/moq.go go/ffi/moq/moq.h go/ffi/moq/lib go/ffi/go.sum \
    go/ffi/vendor go/wrapper/go.sum go/wrapper/vendor

# Dart: tool state and build output.
rm -rf dart/moq/.dart_tool dart/moq_ffi/.dart_tool dart/moq/build dart/moq_ffi/build

# Rendered drafts and their reference cache.
rm -rf drafts/draft-*.xml drafts/draft-*.txt drafts/draft-*.html drafts/.refcache

# Caches no one language owns: nix build result, direnv, wrangler.
rm -rf result .direnv
find . -name .claude -prune -o -type d -name .wrangler -prune -exec rm -rf {} +

# Worktrees don't nest, so this recurses exactly one level. Tolerate stale
# worktrees on branches that predate this script.
if [[ "$scope" == all ]]; then
    for wt in .claude/worktrees/*/; do
        [[ -f "${wt}justfile" ]] || continue
        echo "==> cleaning ${wt}"
        (cd "$wt" && just clean) || echo "    (skipped: just clean failed in ${wt})"
    done
fi
