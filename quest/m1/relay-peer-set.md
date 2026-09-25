# [M] A wire consumer tells a client hop from a peer hop

## Goal

A sidecar reading a relay over the wire knows whether a route entered on that
relay or came from one of its cluster peers, and every way a peer is admitted
can say it is one. In-process consumers already read `Route::source()` and
`origin::Consumer::local()`; a sidecar sees `[.., x, relay]` and cannot tell a
client `x` from a peer `x`. moq.pro's Python sidecar answers it today with a
hop-id bit prefix (`agent/moq_agent/matching.py`).

## Plan

- The relay publishes its cluster peer set in its stats broadcast: the hops of
  the peers whose routes it currently holds, which is exactly what makes a
  delivered route a peer's. Derive it from the origin's peer-marked routes
  rather than from session bookkeeping, so the wire and in-process answers
  cannot disagree. Name the frame's shape in `doc/bin/relay/config.md` (stats
  section) so the sidecar and the producer agree, and land it after the
  FlatBuffers stats flavor settles the track layout.
- `moq auth serve` can mark a peer (`peer: true` in the grant): an mTLS grant
  for the cluster CA, and a JWT whose claims say so. Today only dials the relay
  makes, LAN peers, and a custom auth server's grant mark one, so a stock mesh
  counts inbound JWT and mTLS peers as clients.

Public API: additive on moq-stats and moq-auth. Wire: the stats broadcast gains
a peer-set frame; the JWT claims may gain a field.

## Required

- [Stats wire contract](/quest/m0/stats-binary/README.md) - settles the stats track layout the frame joins

## Related

- [Route cost](/quest/m1/route-cost.md) - the other route fact JS lacks
