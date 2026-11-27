#!/usr/bin/env bash
# Nightly prints the route benchmarks' tables; split from route-sweep.sh so each fits its job's hour.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Release keeps the growth sweeps inside nextest's hang budget; the checks stay on so they still catch what a debug run would.
export CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true
# One run, so the three long growth sweeps share the runner's cores instead of queueing:
# - the multicast bake-off's tables against the path rule and the router, at 12 and 32 relays, and its growth;
# - warmth's path joins, asking, advertising, and ranked readers over fixed shapes, 400 churned meshes per arm on three kinds of mesh, and growth over cores, edges per core, drones, and up to 4096 broadcasts;
# - node layers under MoQ: link state and a flat domain against the router on every single-domain shape at 12 and 32 relays, and as relays, announcing relays, and broadcasts grow.
just rs test --profile=sweep -p moq-net --release --run-ignored only -E 'test(/^route::tests::(bakeoff|warmth|node_layer)::benchmark_/)'
