# [L] Resolve

## Goal

A relay resolves a subscribe or FETCH for an unannounced path against the best
matching wildcard.

## Plan

`lite::publisher::recv_subscribe` already falls back to
`Consumer::request_broadcast`, which drains an `origin::Dynamic` handler that
nothing installs, so the serving half looks done. Do NOT build on it as it
stands. Its request queue is keyed by absolute path alone, so requests from
consumers with different `exclude` origins coalesce onto one entry; a `Request`
carries the path and nothing else, no requester identity, hop list, or cost; and
every live handler drains one shared FIFO, so installing one handler per
received wildcard means whichever polls first wins rather than whichever the
cost ranks. `request_broadcast`'s own comment names the consequence: "a handler
resolves paths with no route chain to check, so falling through would let it
route around the split horizon and rebuild the loop."

So wildcard lookup is a first-class origin routing table, sitting beside the
announce tree and consulted by the same resolution path that already returns
`Found`/`Excluded`/`Missing`. It holds each wildcard's pattern, hop list, and
accumulated cost, and resolution takes the requester's
excluded origin as an argument the way `resolve_broadcast` already does.
Candidates whose hop list contains the requester are filtered BEFORE selection,
so an out-of-band request can never be served back through the peer that made
it.

Among the survivors, only the tier selected by the matcher's shared structural
specificity is consulted, with equal-specificity patterns forming one pool. A
refusal from that tier never falls through to a less
specific one, so `**/transcode.pro` shadows the archive's `**` for
every transcode path, matched or refused. Selection within the tier is lowest
accumulated cost first, then a hash of the REQUESTED path against each
advertiser's origin id. Keying on the request rather than the pattern is the
whole point: hashing the pattern would hand one advertiser every path matching
it. A wildcard covering a path competes on that same cost against a concrete
announcement of it; nothing special-cases the two, so a standby seed below the
topology-cost floor is a real routing bug rather than a tuning preference.

Both lookup kinds route through this table: subscribe via `recv_subscribe`'s
existing fallback, and FETCH the same way, since the archive's whole use case
is serving stored groups to FETCH for paths nothing announces. A FETCH selects
the same advertiser through the same hash and completes without installing a
route.

A served path is not announced downstream, so a wildcard never manufactures
announcements. But a resolved upstream SUBSCRIPTION must be installed as a
ROUTE on the path's origin node, seeded with the wildcard's accumulated cost,
not parked in the request-level `served` cache alone: a concrete announcement
arriving later lands on that same node, competes on cost at one front, and the
front's ordinary route change moves consumers at a group boundary. A
cache-only answer would strand every bound consumer on the wildcard
subscription with nothing able to migrate or stop it. When the concrete claim
is a DIFFERENT publisher, migrating its consumers additionally needs
[Epoch](/quest/m2/wildcard/epoch.md); this quest's obligation is the route
install that makes both possible. Repeat requests for one path
share that one route, keyed to survive the exclusion filter rather than
handing one peer's answer to another.

An upstream reset is a refusal. A capacity code re-resolves once against the
routing table, with the refusing advertiser excluded from that attempt. The
exclusion is what makes the retry safe: the reset and the advertiser's
retraction travel independently, so the table may not have learned yet, and
re-resolution may equally find no other advertiser and return unroutable. That
is a correct outcome, not a case to handle away. Every other code, and any
unrecognized one, is terminal and propagates. Hold no state either way.

Tests, at the process level with real sessions rather than an in-process stand-in:

- One path always selects the same advertiser, and a fixed set of many paths
  spreads across advertisers rather than piling onto one. Do not assert that two
  particular paths differ: a correct hash may legitimately rank the same
  advertiser first for both, so that assertion fails valid implementations.
- A request arriving from a peer that appears in the cheapest wildcard's hop
  list selects a clean alternative, or fails unroutable, and never opens a
  cyclic subscription.
- Two requesters with different excluded origins do not receive each other's
  resolved broadcast.
- A concrete announcement from the SAME publisher takes over from the wildcard
  route at a group boundary, without announcement churn; the cross-publisher
  case needs `Epoch` and belongs with [Epoch](/quest/m2/wildcard/epoch.md).
- A FETCH for an unannounced archived path resolves through the catch-all the
  same way a subscribe does.
- A capacity refusal re-resolves onto another advertiser exactly once, and a
  second capacity refusal is terminal rather than looping.
- A retraction racing an in-flight request (the request arrives after the
  advertiser filled its last slot) ends with the requester served by another
  advertiser, or unroutable, but never hung and never looping.
- A permanent refusal does not re-resolve, so a request for a path nobody serves
  costs exactly one round trip.
- A path matched by both a suffix pattern and the catch-all resolves against
  the suffix pattern's pool only, and a terminal refusal from it never reaches
  the catch-all advertiser.
- A refused subscribe resets rather than hanging, and leaves no state behind.
- A wildcard retracted mid-serve does not disturb the subscription already
  running.

## Required

- [Advertise](/quest/m2/wildcard/advertise.md)
