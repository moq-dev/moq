#!/usr/bin/env bash
# Nightly explores fresh sequences; printed settings replay either implementation.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

MOQ_RESUME_SEED=${MOQ_RESUME_SEED:-$(python3 -c 'import secrets; print(secrets.randbits(31))')}
MOQ_RESUME_CASES=${MOQ_RESUME_CASES:-4096}
export MOQ_RESUME_SEED MOQ_RESUME_CASES
echo "Resume model: MOQ_RESUME_SEED=$MOQ_RESUME_SEED MOQ_RESUME_CASES=$MOQ_RESUME_CASES"
just rs test -p moq-net --lib -E 'test(=model::resume_model::readers_match_the_logical_track)'
bun install --frozen-lockfile
bun test js/net/src/resume.model.test.ts
