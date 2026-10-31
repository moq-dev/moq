#!/usr/bin/env bash
# Format the handwritten Kotlin with ktfmt's kotlinlang style, or with --check
# fail on any file it would change. The generated bindings are gitignored, so
# `git ls-files` never lists them.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../kt"

args=(--kotlinlang-style --quiet)
case "${1:-}" in
    --check) args+=(--dry-run --set-exit-if-changed) ;;
    "") ;;
    *)
        echo "usage: $0 [--check]" >&2
        exit 2
        ;;
esac

if ! command -v ktfmt >/dev/null 2>&1; then
    echo "kt format: ktfmt not on PATH; run 'nix develop'" >&2
    exit 1
fi

# --dry-run names each file it would change.
git ls-files -z --cached --others --exclude-standard -- '*.kt' '*.kts' | xargs -0 ktfmt "${args[@]}" || {
    [[ "${1:-}" != --check ]] || echo "kt format: run 'just kt fix' on the files above" >&2
    exit 1
}
