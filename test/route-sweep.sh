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
# The router's handoff sweeps replay their fixed scenarios past the gate's 150.
MOQ_ROUTE_SCENARIOS=${MOQ_ROUTE_SCENARIOS:-20000} just rs test -P sweep -p moq-net --release -E 'test(/^route::tests::handoff::(distance|path)::sweep_/) | test(/^route::tests::scope::sweep_scoped_meshes_show_what_the_rule_says$/)'
# Cache-aware routing's long sweep, from the same seed, settles every churned mesh of copies: 3000 scenarios each with and without held links, about 3 minutes.
MOQ_ROUTE_CASES=3000 just rs test -P sweep -p moq-net --release --run-ignored only -E 'test(=route::tests::cache::sweep_cache_routing_long)'
