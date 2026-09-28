# Graceful relay drains (GOAWAY)

## Goal

Relay restarts drain sessions instead of hard-dropping them. The end state: a
draining node is first withdrawn from DNS (marked unhealthy so resolvers stop
handing it out), waits out the DNS TTL plus a margin for monitor detection,
THEN sends GOAWAY on every MoQ session. Clients reconnect through a fresh DNS
resolve and land on a different relay, and a straggler that dials the draining
node anyway (a cached resolve, or a pool alias) just gets another GOAWAY. Only
after sessions drain or the stop deadline expires does the process exit and
the new software boot.

GOAWAY only reaches MoQ sessions. Both clients migrate on it:
`moq_tokio::Connection` and the `js/net` `Connection` dial the replacement
through a fresh resolve while the old session drains. The wire message is
lite04+/IETF only besides. So the stop deadline is the
real backstop - for pre-lite04 versions, for client SDKs deployed before
the JS migration shipped, and for in-process ingest gateways (RTMP/SRT/WHIP/WHEP),
which have no GOAWAY equivalent at all: their grace is the DNS-drain window
stopping new arrivals plus the encoder's own reconnect. The DNS-drain-first
ordering is what keeps that hard-close window small.

## Plan

This questline holds the two relay/client halves. The orchestration around
them (a planned-drain health state, the SIGTERM sequencing and stop timeouts,
per-PoP serial deploys, a two-node PoP floor, and the gateway drain contract)
is moq.pro's (downstream) fleet drain work, which consumes these quests.

Drain stays at the MoQ layer. WebTransport's `WT_DRAIN_SESSION` capsule and
the browser `draining` promise are advisory and carry no redirect URI or
timeout, and qmux and WebSocket have no equivalent, so neither the relay nor
the clients send or act on them (decided 2026-09-26).

The relay's drain hook has landed: `Relay::with_signals(false)` hands SIGTERM
to the embedder, and its `shutdown_trigger` GOAWAYs every session, arrivals
included, against one deadline. `Relay::run` returns as soon as every session
has left, logging whether the deadline force-closed any, and
`moq_relay_draining_sessions` shows the drain's progress.

**Clients (landed).** The JS reconnector migrates like the Rust one,
preserving the app-visible session while resolving DNS again before dialing.
Both are covered against stand-in servers. This is a
scale-down prerequisite, not merely a deploy improvement. RTMP/SRT/WHIP/WHEP
cannot receive MoQ GOAWAY, so their contract remains DNS withdrawal followed
by the stop deadline and encoder reconnect.

**End to end (landed).** `just test drain` (`test/drain/`, nightly) has a JS
viewer watch a live track through relay A behind a stand-in for DNS, withdraws
A, SIGTERMs it, and requires the viewer to move to relay B without a dropped
group. It passes today only with a 1s latency budget: following the route
means resubscribing on B, and a group boundary inside the swap loses that
group. The line completes once the viewer needs no budget.

## Required

- [JS group-boundary handover](/quest/m1/drain/js-group-handover.md) - a JS track subscription carries across a route swap at a group boundary, so `test/drain` passes at zero latency budget
- [JS GOAWAY requests](/quest/m1/drain/js-goaway-requests.md) - after GOAWAY the JS client opens no new request on the old session, like Rust

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - the configured topology and link costs a second relay per PoP joins
