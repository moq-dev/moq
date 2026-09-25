# [M] Plan: routing without a hop list

## Goal

Decide whether moq-lite announcements can drop the full hop list and still
route loop-free, then write the decision into quests. Today every
announcement carries the Hop IDs it traversed. That path does three jobs:
loop prevention (a relay discards a path containing itself), request
exclusion (never serve a request back through the peer that made it), and
route identity (the first hop decides whether a failover may stitch
seamlessly). The hop list was designed before prefix claims, and once the Spread quest in
[Wildcard advertisements](/quest/m1/wildcard/README.md) moves stitching
identity onto the subscribe reply, only the first two jobs remain.

## Plan

Open questions, to settle with the maintainer:

- Loop freedom without a path. Rising cost alone ("cost + 1, never
  advertise lower") counts to infinity after a withdrawal, as two relays
  offer each other the stale route at ever higher cost. Remembering each
  route's immediate neighbor and never echoing a route back to it (split
  horizon) stops two-relay loops, but not three-relay loops. It also hides
  backups: a relay never learns an alternative that passes back through the
  neighbor it came from. Babel's feasibility condition (RFC 8966: a
  per-origin sequence number, accepting only routes cheaper than the best
  seen for it) is loop-free with only the origin on the wire. Weigh it and
  any simpler scheme against what the relay cluster actually needs.
- What replaces request exclusion, and whether it still matters once
  announcements are loop-free.
- What the stats and sidecar consumers lose without a path, and whether
  anything besides the origin must stay on the wire.
- The version it lands in (lite-07 is `moq-lite-07-wip` today) and the
  bridge to versions that still carry a hop list.

## Related

- [Wildcard advertisements](/quest/m1/wildcard/README.md) - its Spread quest moves stitching identity to the subscribe reply, which this builds on
- [Announce compression](/quest/m1/announce-compression.md) - reuses the hop-chain tail this would remove
