# [S] Delete the old start bookkeeping

## Goal

Where a lite subscription starts lives in one place per version: lite
`TrackRun`'s `start`, `first`, `start_frame`, the pre-06 re-pin branch, and
the per-version start branches collapse into one start state that only
translates into SUBSCRIBE_START per version, and on lite-07 the
`requested == start` gate is gone, since a fresh START per update makes it
unnecessary. What a cursor is owed or was served is out of scope: it stays
with [Drop set](/quest/m1/drop-set.md).

## Waiting

- moq-dev/moq: [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670)
  merges. It rewrites the same lite publisher state (about 1,250 lines of
  `lite/publisher.rs`). Check that it is merged into `main`.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): narrow this quest to the start state and wait for #7670, as the
agent that declined it on 2026-11-05 recommended. #7670 rewrites the same
publisher state, Drop set already promises to delete the served-groups
record (`skipped`, `served`, `Delivered`), and
[moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872)'s
SUBSCRIBE_STARTED makes the lite-07 start gate removable. Not chosen:
folding it into Drop set, or doing it now on main. The bullets below
predate the narrowing; the owed-range ones are Drop set's.


Moved out of the [Subscribe ranges](/quest/m2/subscribe-ranges/README.md)
README's own work, where the 2026-10-24 pre-cut audit put it. Decided
2026-10-27 (maintainer): a child quest of its own, since
[Rust groups by start](/quest/m2/time-addressing/model.md) requires it before
the cut moves this state onto times, while the rest of the README waits on
every child.

Lite `TrackRun`'s fixes cluster on where a subscription starts and what it
is owed or dropped (e713c30991, 36f82ac450, 35fa44f6bf, e1b5d2741f,
f235fcfcbd, df0bd5c36f, 342445f251, 06765f4c1a, 4927167904, f52848d634,
0513c07369, 5ad3652871). That state spans about eight fields (`floor`,
`cap`, `start`, `first`, `skipped`, `served`, `start_frame`, `end_frame`),
plus `cursor_floor` and a pre-06 re-pin branch, encoding three wire
generations of "start".

- On lite-07 every owed range is covered by delivered stream groups or a
  DROP, since
  [Datagram tracks](/quest/m1/datagram-tracks/README.md) takes datagrams out
  of stream tracks (maintainer, 2026-11-01). The subscriber's end is "owed
  range covered and no stream active", with no datagram grace. A stream
  track's lite-05/06 subscription drops group datagrams, so a group an old
  publisher sent only as one is a hole there. Keep `Settle`'s full grace for
  lite-05/06 and moq-transport behind one version predicate.
- Lite-07 is unpublished, so sending a fresh START per update can remove the
  `requested == start` gate (`lite/subscriber.rs`, about line 4898).

Public API: none expected. Wire: lite-07 (unpublished) may send a START per
update; published versions are unchanged.
