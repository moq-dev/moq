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

GOAWAY only reaches MoQ sessions, and today nobody acts on it: moq-net (Rust
and JS) decodes and logs the message without closing or migrating the session,
and the wire message is lite04+/IETF only besides. So the stop deadline is the
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

**relay-drain-api.** A drain hook that GOAWAYs every established session and
immediately GOAWAYs any new arrival, so an embedding process can enter drain
on SIGTERM after the DNS window and still bound the total stop time. Expose
enough phase/session state to prove which bound ended a drain.

**client-goaway.** Rust and JS reconnectors preserve the app-visible session
but discard the pinned address and resolve DNS again before dialing. This is a
scale-down prerequisite, not merely a deploy improvement. RTMP/SRT/WHIP/WHEP
cannot receive MoQ GOAWAY, so their contract remains DNS withdrawal followed
by the stop deadline and encoder reconnect.

## Quests

- [Relay drain api](/quest/m2/drain/relay-drain-api.md) - a drain hook that
  GOAWAYs every session, including new arrivals, triggered by the embedding
  process on SIGTERM
- [Client goaway](/quest/m2/drain/client-goaway.md) - native and JavaScript
  clients reconnect through a fresh DNS resolve after GOAWAY; today both only
  log it

## Related

- [pop-skipping](/quest/m2/pop-skipping/README.md) - its same-PoP link price and full eligible pairing become important when a deployment adds a second relay per PoP
