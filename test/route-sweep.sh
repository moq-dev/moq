#!/usr/bin/env bash
# Nightly sweeps fresh meshes; the printed settings replay a failing case bit-exact.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

MOQ_ROUTE_SEED=${MOQ_ROUTE_SEED:-$(python3 -c 'import secrets; print(secrets.randbits(31))')}
MOQ_ROUTE_CASES=${MOQ_ROUTE_CASES:-500}
export MOQ_ROUTE_SEED MOQ_ROUTE_CASES
echo "Route sweep: MOQ_ROUTE_SEED=$MOQ_ROUTE_SEED MOQ_ROUTE_CASES=$MOQ_ROUTE_CASES"
# Release keeps the sweep inside nextest's hang budget; the checks stay on so it still catches what a debug run would.
export CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true
just rs test -p moq-net --release --features async --test integration -E 'test(=mesh_sweep::seeded_meshes_hold_the_route_rules)'
# The bake-off's prototype and the multicast router carry every failure on every shape at 32 relays, and every delivery order the gate's searches leave out.
# The route benchmarks run from route-bench.sh instead, in a Nightly job of their own.
just rs test --profile=sweep -p moq-net --release --run-ignored only -E 'test(/^route::tests::((bakeoff|multicast)::sweep_|multicast_search::)/)'
