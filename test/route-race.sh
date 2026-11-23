#!/usr/bin/env bash
# Nightly races fresh rounds; the printed settings replay a failing round.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

MOQ_RACE_SEED=${MOQ_RACE_SEED:-$(python3 -c 'import secrets; print(secrets.randbits(31))')}
# With moq-dev/moq#9052's fix reverted, a round fails about once in 820 (40 to 1464 over 10 seeds),
# so 10000 rounds miss it less than once in 100,000 nights; about 2.5 minutes.
MOQ_RACE_ROUNDS=${MOQ_RACE_ROUNDS:-10000}
export MOQ_RACE_SEED MOQ_RACE_ROUNDS
echo "Route race: MOQ_RACE_SEED=$MOQ_RACE_SEED MOQ_RACE_ROUNDS=$MOQ_RACE_ROUNDS"
# Release keeps the rounds quick; the checks stay on so it still catches what a debug run would.
export CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true
just rs test -P sweep -p moq-net --release --features async --test integration -E 'test(=route_race::seeded_races_hold_every_claim_shown)'
