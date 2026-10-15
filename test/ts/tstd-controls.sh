#!/usr/bin/env bash
# Keep the model's synthetic fixtures and reports when any control fails.
set -euo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin ts-tstd "just test ts-tstd"
tsp --version >"$HARNESS_RUN/tsduck-version.txt" 2>&1
python3 "$DIR/tstd-controls.py" "$HARNESS_RUN"
