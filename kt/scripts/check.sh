#!/usr/bin/env bash
set -euo pipefail

# Full check for the Kotlin wrapper: regenerate the bindings + native lib
# (scripts/generate.sh), then compile the wrapper and run `:moq:jvmTest`.
#
# Unlike generation, this needs a JDK and Gradle. Both ship in the `nix
# develop` dev shell (see flake.nix ktDeps), so a missing one is an error
# rather than a silent skip: skipping here lets Kotlin wrapper drift slip
# past a green `just check`. Environments that intentionally lack Gradle
# should run `just kt generate` instead.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

bash "$SCRIPT_DIR/generate.sh"

if ! command -v java >/dev/null 2>&1; then
    echo "kt check: no JDK on PATH; run 'nix develop' or use 'just kt generate' to only regenerate bindings" >&2
    exit 1
fi

GRADLE_CMD="${GRADLE_CMD:-$(command -v gradle || true)}"
if [[ -z "$GRADLE_CMD" ]]; then
    echo "kt check: gradle not on PATH; run 'nix develop' or use 'just kt generate' to only regenerate bindings" >&2
    exit 1
fi

"$GRADLE_CMD" -p "$KT_DIR" -Pmoqffi.version=0.0.0-dev :moq:jvmTest
