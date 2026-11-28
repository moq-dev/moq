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
# the check runs on Linux too. Only x86_64-linux dev shells carry one (see
# flake.nix); macOS uses Xcode's through Apple's xcrun (the dev shell's is
# xcbuild's), after dropping the dev shell's nixpkgs SDK as
# check.sh does, and aarch64-linux skips.
xcode=(env)
[[ "${SDKROOT:-}" != /nix/store/* ]] || xcode+=(-u SDKROOT)
[[ "${DEVELOPER_DIR:-}" != /nix/store/* ]] || xcode+=(-u DEVELOPER_DIR)
if command -v swift-format >/dev/null 2>&1; then
    swift_format=(swift-format)
elif [[ "$(uname -s)" == Darwin ]] && "${xcode[@]}" /usr/bin/xcrun --find swift-format >/dev/null 2>&1; then
    swift_format=("${xcode[@]}" /usr/bin/xcrun swift-format)
else
    echo "swift format: no swift-format on PATH, skipping" >&2
    exit 0
fi

files=()
while IFS= read -r -d '' file; do
    # Tracked files deleted from the working tree have nothing to format.
    [[ ! -e "$file" ]] || files+=("$file")
done < <(git ls-files -z --cached --others --exclude-standard -- '*.swift')

if [[ -z "$check" ]]; then
    "${swift_format[@]}" format --in-place --parallel "${files[@]}"
    exit
fi

# `swift-format lint` also runs the style rules, so compare against the
# formatter's own output instead.
unformatted=()
for file in "${files[@]}"; do
    "${swift_format[@]}" format "$file" | cmp -s - "$file" || unformatted+=("$file")
done
if ((${#unformatted[@]})); then
    echo "swift format: run 'just swift fix' on these files:" >&2
    printf '%s\n' "${unformatted[@]}" >&2
    exit 1
fi
