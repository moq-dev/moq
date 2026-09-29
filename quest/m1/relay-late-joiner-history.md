# [M] Late joiner history

## Goal

A second subscriber that joins a relay's track from group 0 while a first
subscriber still holds it receives every group the relay has cached,
including a finished group older than the open live one. A regression test
that joins late through a relay fails before the fix.

## Plan

Found downstream on moq.pro: its billing rollup tracks replay a finished
history group followed by an open live group, and a second browser opening
the Cost page on the same edge intermittently loses the older group, so
prior-period usage rows are missing. Production dashboards are affected,
not just tests. #4387 fixed the first viewer's version of this; what it
leaves is the viewer that joins later.

What we saw:

- With #4387 applied, moq.pro's `e2e/tests/rollup-history.spec.ts
  --repeat-each 12` still fails about a quarter of runs, always on the
  second viewer. The edge logs `serving group` only for the newer group on
  that viewer's subscription, while the first viewer got both.
- A late-joiner variant of `rs/moq-net/tests/history_groups.rs` showed the
  relay's late cursor spliced across two segments with its floor at the
  newer group, so the cached older group sat below it. The segment churn
  comes through `Action::Query`, `Splice`, `Park`, and `Release` in
  `rs/moq-net/src/model/origin.rs`, triggered by the TRACK_INFO request that
  precedes each subscription and then goes idle.
- That variant is not a faithful reproduction yet: under paused time the
  origin's linger timers fire during the test's idle waits, so it also failed
  for lite-03 and the IETF drafts, which the real stack does not. A
  reproduction needs to keep the relay's track warm without auto-advancing
  past the linger, or drive the timers explicitly.

Start from how a newly spliced or parked segment derives its floor for a
subscriber that asked for group 0, and whether a warm copy's cached groups
below the new segment's first group stay reachable. The
[splice edge cases](/quest/m1/splice-edges.md) touch the same code.

## Related

- [Splice edge cases](/quest/m1/splice-edges.md) - other spliced-track cases that lose or mis-judge groups
