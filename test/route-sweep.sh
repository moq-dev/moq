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
# The multicast bake-off: its prototype carries every failure on every shape at 32 relays, and its tables against the path rule and the router at 12.
MOQ_BAKEOFF_RELAYS=12 just rs test --profile=sweep -p moq-net --release --run-ignored only -E 'test(/^route::tests::bakeoff::(sweep|benchmark)_/)'
# The multicast router: every failure on every shape at 32 relays, every delivery order the gate's searches leave out, what one event costs it, and how its floors grow.
just rs test --profile=sweep -p moq-net --release --run-ignored only -E 'test(/^route::tests::multicast(_search|_cost)?::/)'
# Warmth: path joins, asking, advertising, and ranked readers over fixed shapes, 400 churned meshes per arm on three kinds of mesh, and growth over cores, edges per core, drones, and up to 4096 broadcasts, about 10 minutes.
just rs test --profile=sweep -p moq-net --release --run-ignored only -E 'test(/^route::tests::warmth::benchmark_/)'
# Node layers under MoQ: link state and a flat domain against the router on every single-domain shape at 12 and 32 relays, and as relays, announcing relays, and broadcasts grow.
MOQ_BAKEOFF_RELAYS=12,32 just rs test --profile=sweep -p moq-net --release --run-ignored only -E 'test(/^route::tests::node_layer::benchmark_/)'
