#!/usr/bin/env bash
# Format the handwritten Swift with Apple's swift-format, configured by
# swift/.swift-format, or with --check fail on any file it would change. The
# generated bindings are gitignored, so `git ls-files` never lists them.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../swift"

check=
case "${1:-}" in
    --check) check=1 ;;
    "") ;;
    *)
        echo "usage: $0 [--check]" >&2
        exit 2
        ;;
esac

# The dev shell's swift-format, rather than the toolchain's `swift format`, so
# the check runs on Linux too and every host formats with the same version.
# The dev shell has none on aarch64-linux (see flake.nix).
if ! command -v swift-format >/dev/null 2>&1; then
    echo "swift format: no swift-format on PATH, skipping" >&2
    exit 0
fi

files=()
while IFS= read -r -d '' file; do
    files+=("$file")
done < <(git ls-files -z --cached --others --exclude-standard -- '*.swift')

if [[ -z "$check" ]]; then
    swift-format format --in-place --parallel "${files[@]}"
    exit
fi

# `swift-format lint` also runs the style rules, so compare against the
# formatter's own output instead.
unformatted=()
for file in "${files[@]}"; do
    swift-format format "$file" | cmp -s - "$file" || unformatted+=("$file")
done
if ((${#unformatted[@]})); then
    echo "swift format: run 'just swift fix' on these files:" >&2
    printf '%s\n' "${unformatted[@]}" >&2
    exit 1
fi
