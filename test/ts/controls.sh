#!/usr/bin/env bash
# Real software-validator controls, with evidence retained on failure.
set -euo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin ts-controls "just test ts-controls"
python3 "$DIR/controls.py" "$HARNESS_RUN"
bash "$DIR/process.test.sh"
