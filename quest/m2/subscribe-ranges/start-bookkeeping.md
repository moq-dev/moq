# [S] Delete the old start bookkeeping

## Goal

Where a lite subscription starts lives in one place per version: lite
`TrackRun`'s `start` and `first`, the pre-06 re-pin branch, and
the per-version start branches collapse into one start state that only
translates into SUBSCRIBE_START per version, and on lite-07 the
`requested == start` gate is gone, since a fresh START per update makes it
unnecessary. What a cursor is owed or was served is out of scope: it stays
with [Drop set](/quest/m1/drop-set.md).

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): narrow this quest to the start state and wait for #7670, as the
agent that declined it on 2026-11-05 recommended. #7670 rewrites the same
publisher state, Drop set already promises to delete the served-groups
record (`skipped`, `served`, `Delivered`), and
[moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872)'s
SUBSCRIBE_STARTED makes the lite-07 start gate removable. Not chosen:
folding it into Drop set, or doing it now on main.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ the wait on
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670), Drop set's own
PR, becomes a `Required` link to [Drop set](/quest/m1/drop-set.md), since
#7670 rewrites the same lite publisher state (about 1,250 lines of
`lite/publisher.rs`).


Moved out of the [Subscribe ranges](/quest/m2/subscribe-ranges/README.md)
README's own work, where the 2026-10-24 pre-cut audit put it. Decided
2026-10-27 (maintainer): a child quest of its own, since
[Rust groups by start](/quest/m2/time-addressing/model.md) requires it before
the cut moves this state onto times, while the rest of the README waits on
every child.

Lite `TrackRun`'s fixes cluster on where a subscription starts and what it
is owed or dropped (e713c30991, 36f82ac450, 35fa44f6bf, e1b5d2741f,
f235fcfcbd, df0bd5c36f, 342445f251, 06765f4c1a, 4927167904, f52848d634,
0513c07369, 5ad3652871). That state spans `TrackRun`'s `floor`, `cap`,
`start`, `first`, `skipped`, and `served` (frame bounds moved to `Frames`;
`start_frame` remains only on lite FETCH, `lite/fetch.rs`), plus
`cursor_floor` and the pre-06 re-pin branch in `TrackRun::update`, encoding
three wire generations of "start".

- Lite-07 is unpublished, so sending a fresh START per update can remove the
  `active.start == active.requested` gate (`lite/subscriber.rs`,
  `apply_response`).

- The start state must make it impossible to resolve without settling what
  was given up before the start. Today there are two paths. `resolve()`
  drains `skipped`, while the shortcut that answers a start past the live
  edge at once (the "start past everything" branch in `TrackRun`'s poll) only
  sets `self.start`. So any drop collected while unresolved is never sent,
  and the subscriber waits out its grace for those groups. Reproduce this
  first. Found by the 2026-11-07 audit of implicit state machines.
  Recommended: `enum Start { Legacy, Unresolved { skipped, first },
  Resolved { start, served } }`, with `skipped` moved out at the transition.

Public API: none expected. Wire: lite-07 (unpublished) may send a START per
update; published versions are unchanged.

## Required

- [Drop set](/quest/m1/drop-set.md) - rewrites the same lite publisher state
