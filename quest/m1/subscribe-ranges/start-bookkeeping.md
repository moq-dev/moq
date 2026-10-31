# [M] Delete the old start bookkeeping

## Goal

What each lite cursor is owed and what was dropped lives in the model, and
lite `TrackRun` only translates it into SUBSCRIBE_START, SUBSCRIBE_DROP, and
SUBSCRIBE_END per version. `skipped`, `served`, `first`, and the pre-06
branches are deleted rather than layering lite-07 ranges on top, and on
lite-07 a subscriber's end is "owed range covered and no stream active".

## Plan

Moved out of the [Subscribe ranges](/quest/m1/subscribe-ranges/README.md)
README's own work, where the 2026-10-24 pre-cut audit put it. Decided
2026-10-27 (maintainer): a child quest of its own, since
[Rust groups by start](/quest/m1/time-addressing/model.md) requires it before
the cut moves this state onto times, while the rest of the README waits on
every child.

Lite `TrackRun`'s fixes cluster on where a subscription starts and what it
is owed or dropped (e713c30991, 36f82ac450, 35fa44f6bf, e1b5d2741f,
f235fcfcbd, df0bd5c36f, 342445f251, 06765f4c1a, 4927167904, f52848d634,
0513c07369, 5ad3652871). That state spans about eight fields (`floor`,
`cap`, `start`, `first`, `skipped`, `served`, `start_frame`, `end_frame`),
plus `cursor_floor` and a pre-06 re-pin branch, encoding three wire
generations of "start".

- On lite-07 a stream or a DROP accounts for every sequence except one sent
  only as a datagram, which can be lost unseen
  (`a_lost_datagram_delays_the_end` in `tests/integration/track_tail.rs`).
  So the subscriber's end is "owed range covered and no stream active",
  except that a hole a datagram may have left still settles within a bounded
  grace; only reliable accounting of datagram sequences could remove that.
  Keep `Settle`'s full grace for lite-05/06 and moq-transport behind one
  version predicate.
- Lite-07 is unpublished, so sending a fresh START per update can remove the
  `requested == start` gate (`lite/subscriber.rs`, about line 4898).

Public API: none expected. Wire: lite-07 (unpublished) may send a START per
update; published versions are unchanged.

## Required

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the lite-07 range fill this bookkeeping is rebuilt around
