# [S] Transit in the JS origin

## Goal

A test proves a tab forwards the routes it receives on one session to the
sessions it serves, so a watcher can re-serve a broadcast to a peer and a room
of watchers can cost the relay one egress stream. Split horizon holds: a route
is never announced back to the session it arrived on, and every forwarded
chain carries the tab's hop id.

## Plan

Decided in the 2026-09-30 audit: shrink to a test. The Rust origin already
does transit (`with_publisher` and `with_subscriber` splice and share remote
routes), and [Generated lite](/quest/m1/rs2ts/lite.md) replaces the
hand-written JS origin with one generated from it, so there is no JS
forwarding step to write by hand.

The hop id must be one per origin, not per session, so the roster id from
[signaling](/quest/m2/p2p/signal.md) and the id in forwarded chains agree.

Tests with the mock transport pair against the generated origin: a route
received on A appears on B with the id appended and never on A; a chain
already containing the id or at `MAX_HOPS` is dropped; a retraction on A
retracts on B; two watchers of one tab share one upstream subscription. If a
case fails, the fix goes in the Rust origin.

## Required

- [Generated lite](/quest/m1/rs2ts/lite.md) - the JS origin generated from moq-net, which already does transit

## Related

- [Watch opts in](/quest/m2/p2p/watch.md) - the first topology that needs a forwarding tab
- [Cost across scopes](/quest/m2/p2p/cost-scopes.md) - adds the receiving link's cost to forwarded routes
