#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

# Full check for the Kotlin packages: regenerate the bindings + native lib
# (sh/kt/generate.sh), then run the raw-bindings and wrapper JVM tests.
#
# Unlike generation, this needs a JDK and Gradle. Both ship in the `nix
# develop` dev shell (see flake.nix ktDeps), so a missing one is an error
# rather than a silent skip: skipping here lets Kotlin wrapper drift slip
# past a green `just check`. Environments that intentionally lack Gradle
# should run `just kt generate` instead.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/../../kt" && pwd)"

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

# The Gradle daemon, and the Kotlin daemon it starts, outlive the build, so the next check
# skips their warm-up. A GitHub Actions runner kills whatever a job leaves running, found by
# its RUNNER_TRACKING_ID, so Gradle runs without the job's variables: the landing worker's
# persistent runner then keeps the daemons for its next job. A daemon keeps the environment it
# started with, so this also keeps the job's tokens out of it; each build gets its client's.
gradle() (
    unset "${!ACTIONS_@}" "${!GITHUB_@}" "${!RUNNER_@}"
    "$GRADLE_CMD" "$@"
)

# Compile the documentation samples with the tests, beside the inputs Prelude.kt
# declares, so a doc that drifts from the wrapper fails here.
DOCS_DIR="$KT_DIR/moq/src/jvmAndAndroidTest/kotlin/dev/moq/docs"
{
    echo "package dev.moq.docs"
    bash "$KT_DIR/../doc/lib/samples.sh" kotlin "$KT_DIR/../doc/lib/kt/index.md" "$KT_DIR/README.md"
} >"$DOCS_DIR/Samples.kt"

gradle -p "$KT_DIR" -Pmoqffi.version=0.0.0-dev :moq-ffi:jvmTest :moq:jvmTest

# The sibling-project substitution must not leak its dev version into published POMs.
gradle -p "$KT_DIR" :moq:generatePomFileForKotlinMultiplatformPublication :moq:generatePomFileForJvmPublication
python3 "$SCRIPT_DIR/check-poms.py" "$KT_DIR/moq/build/publications"
