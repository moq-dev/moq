# [S] Announce SYNCED

## Goal

On lite-07, the Announce stream marks the end of its initial set with the
same ordered SYNCED event [TRACKS](/quest/m1/archive/tracks.md) uses,
replacing ANNOUNCE_OK's `Active Count`. A subscriber knows it holds every
route this hop held that is still live; one that ends before a slow reader
reaches its ANNOUNCE_START is omitted, as in TRACKS. Published versions keep
`Active Count` unchanged.

## Plan

Decided by the maintainer on 2026-10-19 with TRACKS ("We don't need the
count at all"), and named SYNCED the same day. A count sent first commits to
that many ANNOUNCE_STARTs, so an initial route that ends before a slow reader
reaches its start leaves the count unmet, unless the hop retains every
initial announcement per reader. SYNCED is sent once, in order, at the
first drain of the reader's queue (its `poll_next` returns Pending) or once
the reader has been sent N events, N being the snapshot size it recorded when
it subscribed, so live churn cannot postpone it (maintainer, 2026-10-19). An initial
route that comes and goes before then is omitted, collapsing in the reader's
pending queue like any other, and a retraction of a route the reader saw may
land before SYNCED. Keep the event identical in shape to TRACKS' SYNCED, so
one encoding and one consumer rule serve both streams.

- N holds only if the queue delivers in insertion order, so the snapshot
  stays ahead of live entries. The origin consumer's queue
  (`OriginConsumerState::pending`) is a `BTreeMap` delivered in
  lexicographic order, where a live prefix sorting first can precede a
  snapshot one indefinitely, and its `owed` set waits on that order. Keep
  the keyed collapse but deliver in insertion order. A count of N then
  replaces the `owed` set.
- Synced means synced with this hop: a relay answers at once with what it
  holds and never waits on its upstream (relays never block on peers).
- The in-process consumers already expose this as synced
  (`AnnounceConsumer::poll_synced` in `rs/moq-net/src/model/origin.rs`,
  `synced` in `js/net/src/announced.ts`); the wire event feeds the same
  signal.
- lite-07's ANNOUNCE_OK loses `Active Count`; no count field remains. Update
  `drafts/draft-lcurley-moq-lite.md` (ANNOUNCE_OK, the Announce stream text,
  and the lite-07 changelog), then run `just drafts check` and
  `just test interop --all`.
- Rust and JS consumers that batch the initial set (such as `moq ls`) wait
  for SYNCED on lite-07 and keep reading the count on older versions.
- Coordinate with [Routes and announces](/quest/m1/cluster-routing/routes.md),
  which reshapes the lite-07 Announce stream and adds ANNOUNCE_FLOOR to
  certify per-route completeness. SYNCED is per stream and orthogonal;
  whichever lands second adapts its wire to the other.
- The moq-transport Active Count extension
  (`drafts/draft-lcurley-moq-active-count.md`) is out of scope; it is a
  separate published draft.

Test on mock time, directly and through a relay: SYNCED after the initial
routes, sent the first time the reader's queue drains; an initial route
ending before a stalled reader reaches it, with SYNCED still arriving; a
retraction of a seen route landing before SYNCED; a cold relay answering
SYNCED at once; a stalled reader under continuous churn of new prefixes,
including ones sorting before the snapshot, getting SYNCED once it has been
sent N events; and a lite-06 peer still getting `Active Count`.

Public API: none, if consumers already expose "initial set done"; otherwise
the same signal as TRACKS, mirrored in Rust and `@moq/net`. Wire: lite-07-wip
ANNOUNCE_OK drops `Active Count` and the Announce stream gains SYNCED.

## Related

- [TRACKS](/quest/m1/archive/tracks.md) - the same event on the track listing
