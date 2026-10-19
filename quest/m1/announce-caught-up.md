# [S] Announce CAUGHT_UP

## Goal

On lite-07, the Announce stream marks the end of its initial set with the
same ordered CAUGHT_UP event [TRACKS](/quest/m1/archive/tracks.md) uses,
replacing ANNOUNCE_OK's `Active Count`. A subscriber knows it holds every
route this hop knew when it asked, even when one of those routes ends before
a slow reader reaches it. Published versions keep `Active Count` unchanged.

## Plan

Decided by the maintainer on 2026-10-19 with TRACKS ("We don't need the
count at all"). A count sent first commits to that many ANNOUNCE_STARTs, so
an initial route that ends before a slow reader reaches its start leaves
the count unmet, unless the hop retains every initial announcement per
reader. CAUGHT_UP is sent once, in order, when the reader reaches what was
the live head when it asked, so an initial route that comes and goes before
then collapses like any other. Keep the event identical in shape to TRACKS'
CAUGHT_UP, so one encoding and one consumer rule serve both streams.

- Caught up means caught up with this hop: a relay answers at once with what
  it holds and never waits on its upstream (relays never block on peers).
- lite-07's ANNOUNCE_OK loses `Active Count`; no count field remains. Update
  `drafts/draft-lcurley-moq-lite.md` (ANNOUNCE_OK, the Announce stream text,
  and the lite-07 changelog), then run `just drafts check` and
  `just test interop --all`.
- Rust and JS consumers that batch the initial set (such as `moq ls`) wait
  for CAUGHT_UP on lite-07 and keep reading the count on older versions.
- Coordinate with [Routes and announces](/quest/m1/cluster-routing/routes.md),
  which reshapes the lite-07 Announce stream and adds ANNOUNCE_FLOOR to
  certify per-route completeness. CAUGHT_UP is per stream and orthogonal;
  whichever lands second adapts its wire to the other.
- The moq-transport Active Count extension
  (`drafts/draft-lcurley-moq-active-count.md`) is out of scope; it is a
  separate published draft.

Test on mock time, directly and through a relay: CAUGHT_UP after the
initial routes; an initial route ending before a stalled reader reaches it,
with CAUGHT_UP still arriving; a cold relay answering CAUGHT_UP at once; and
a lite-06 peer still getting `Active Count`.

Public API: none, if consumers already expose "initial set done"; otherwise
the same signal as TRACKS, mirrored in Rust and `@moq/net`. Wire: lite-07-wip
ANNOUNCE_OK drops `Active Count` and the Announce stream gains CAUGHT_UP.

## Related

- [TRACKS](/quest/m1/archive/tracks.md) - the same event on the track listing
