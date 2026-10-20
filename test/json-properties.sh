#!/usr/bin/env bash
# Nightly explores fresh sequences; printed settings replay either implementation.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

MOQ_JSON_SEED=${MOQ_JSON_SEED:-$(python3 -c 'import secrets; print(secrets.randbits(31))')}
MOQ_JSON_CASES=${MOQ_JSON_CASES:-4096}
export MOQ_JSON_SEED MOQ_JSON_CASES
echo "JSON codec properties: MOQ_JSON_SEED=$MOQ_JSON_SEED MOQ_JSON_CASES=$MOQ_JSON_CASES"
bun install --frozen-lockfile
just rs test -p moq-json --lib -E 'test(snapshot_codec_property) | test(window_codec_property)'
bun test js/json/src/snapshot/properties.test.ts js/json/src/window/properties.test.ts
bunx tsc --noEmit --project js/json/bench/tsconfig.json
bun js/json/bench/codec.ts
