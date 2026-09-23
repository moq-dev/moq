# [M] Transit in the JS origin

## Goal

A tab forwards the routes it receives on one session to the sessions it
serves, so a watcher can re-serve a broadcast to a peer and a room of
watchers can cost the relay one egress stream. Split horizon holds: a route
is never announced back to the session it arrived on, and every forwarded
chain carries the tab's hop id.

## Plan

The JS origin keeps two tables so a received entry can never be announced
back to a peer; that separation stays. Transit adds the forwarding step: a
route received on session A is announced on every other attached session
with the tab's hop id appended to `hops`, the receiving link's cost added
per [cost across scopes](/quest/m1/p2p/cost-scopes.md) once it exists and
`Cost::UNKNOWN` semantics until then, and dropped when the chain reaches
`MAX_HOPS` or already contains the tab's id. A retraction forwards the same
way.

Serving a forwarded route means the tab subscribes upstream once and fans out
from its own origin, which is what the Rust origin already does with
`with_publisher` and `with_subscriber`; the JS origin gains the same
splice-and-share behavior for remote routes it serves onward.

The hop id must be one per origin, not per session, so the roster id from
[signaling](/quest/m1/p2p/signal.md) and the id in forwarded chains agree.

Tests with the mock transport pair: a route received on A appears on B with
the id appended and never on A; a chain already containing the id is dropped;
a retraction on A retracts on B; two watchers of one tab share one upstream
subscription.

## Required

- [Route cost in the JS origin](/quest/m1/route-cost.md) - cost and hops must be carried on the entry before they can be forwarded

## Related

- [Watch opts in](/quest/m1/p2p/watch.md) - the first topology that needs a forwarding tab
