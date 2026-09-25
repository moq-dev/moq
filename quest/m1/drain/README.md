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

GOAWAY only reaches MoQ sessions, and only the Rust client acts on it today:
`moq_tokio::Connection` migrates, while `js/net` decodes and logs the message
and at most closes the session afterwards (the IETF path does, the lite path
does not), leaving any reconnect to the ordinary close-triggered backoff
rather than migrating. The wire message is lite04+/IETF only besides. So the stop deadline is the
real backstop - for pre-lite04 versions, for client SDKs deployed before
client-goaway ships, and for in-process ingest gateways (RTMP/SRT/WHIP/WHEP),
which have no GOAWAY equivalent at all: their grace is the DNS-drain window
stopping new arrivals plus the encoder's own reconnect. The DNS-drain-first
ordering is what keeps that hard-close window small.

## Plan

This questline holds the two relay/client halves. The orchestration around
them (a planned-drain health state, the SIGTERM sequencing and stop timeouts,
per-PoP serial deploys, a two-node PoP floor, and the gateway drain contract)
is moq.pro's (downstream) fleet drain work, which consumes these quests.

The relay's drain hook has landed: `Relay::with_signals(false)` hands SIGTERM
to the embedder, and its `shutdown_trigger` GOAWAYs every session, arrivals
included, against one deadline.

**client-goaway.** The JS reconnector migrates like the Rust one, preserving
the app-visible session while resolving DNS again before dialing, and the Rust
path gains the regression test it lacks. This is a
scale-down prerequisite, not merely a deploy improvement. RTMP/SRT/WHIP/WHEP
cannot receive MoQ GOAWAY, so their contract remains DNS withdrawal followed
by the stop deadline and encoder reconnect.

## Quests

- [Client goaway](/quest/m1/drain/client-goaway.md) - the JavaScript client
  migrates on GOAWAY with a handover and the guarded redirect the Rust client
  already has, and the Rust drain path gets its regression test
- [Drain exit](/quest/m1/drain/drain-exit.md) - a drain ends as soon as every
  session has left, and reports whether that or the deadline ended it

## Related

- [pop-skipping](/quest/m1/pop-skipping/README.md) - its same-PoP link price and full eligible pairing become important when a deployment adds a second relay per PoP
