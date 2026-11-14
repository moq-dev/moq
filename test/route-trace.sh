#!/usr/bin/env bash
# Trace validation of the route model: record the router's handoff sweeps under the
# path rule and check with TLC that each run is a behaviour of RouteHandoff.tla
# (rs/moq-net/verify/route/trace/). Nightly traces a fresh window of the sweeps'
# scenarios; the printed settings replay it.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

MOQ_ROUTE_TRACE_FROM=${MOQ_ROUTE_TRACE_FROM:-$(python3 -c 'import secrets; print(secrets.randbelow(20000))')}
MOQ_ROUTE_SCENARIOS=${MOQ_ROUTE_SCENARIOS:-$((MOQ_ROUTE_TRACE_FROM + 150))}
export MOQ_ROUTE_TRACE_FROM MOQ_ROUTE_SCENARIOS
echo "Route traces: MOQ_ROUTE_TRACE_FROM=$MOQ_ROUTE_TRACE_FROM MOQ_ROUTE_SCENARIOS=$MOQ_ROUTE_SCENARIOS"

mkdir -p .tmp
MOQ_ROUTE_TRACE=$(mktemp -d "$PWD/.tmp/route-trace.XXXXXX")
export MOQ_ROUTE_TRACE
trap 'rm -rf "$MOQ_ROUTE_TRACE"' EXIT
# Release keeps a window far into the sweeps inside nextest's hang budget.
export CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true
just rs test -P sweep -p moq-net --release -E 'test(/^route::tests::handoff::path::(sweep_handoff|sweep_scoped_handoff)$/)'
python3 sh/tla-trace.py rs/moq-net/verify/route/trace/RouteTrace.tla "$MOQ_ROUTE_TRACE"/*.ndjson
