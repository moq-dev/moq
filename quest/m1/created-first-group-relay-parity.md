# [S] A created first group reads differently locally and through a relay

## Goal

A late oldest-group, key-order reader of a finished track whose first group came from
`track::Producer::create_group` gets the same result locally as through a relay. Today the
local reader gets a `NotFound` gap over the keys below that group, while through a relay whose
copy a latest-group reader filled first it gets the group.

## Plan

Decided 2026-11-24 (maintainer, kixel.ai /quest-run): planned from kixel-ai/kixel.ai#269's follow-ups.
m3, since it is a bug fix in shipped code.

Found by [kixel-ai/kixel.ai#269](https://github.com/kixel-ai/kixel.ai/pull/269).
[moq-dev/moq#9534](https://github.com/moq-dev/moq/pull/9534) makes a track's first appended group
skip every key below it, but a created group says nothing about the keys around it.

Reproduce it first. A scratch integration test (not committed) on `main` at `2d3bc05f09`
(lite-07-wip, unbounded pools, proxy origins) wrote one group at a wall-clock key K with
`create_group`, finished the group and the track, then read with `Start::Oldest`, in
ascending order:

- 0 links (local): `gap 0..K not found`
- 1 or 2 relays, with no earlier reader: `gap 0..K not found`
- 1 or 2 relays, after a `Start::LATEST` reader read group K from the same copy: `group K`

With the first group appended instead, every one of those cases reads `group K`. The relayed
answer therefore depends on what the copy already holds, which the root `AGENTS.md` cache rule
forbids. Find where the warmed copy places the oldest group at K without the proof the publisher
has, and settle which answer is right for keys a finished track never created. Both paths must
give that answer, and the reproduction becomes the regression test.

Traced 2026-11-28 (quest-start session; the reproduction is committed as
`rs/moq-net/tests/integration/created_first_group.rs`, failing on the two warm relayed cases):

- The right answer is the gap `0..K` `NotFound`. A created group does not say the producer will not
  go back, and the publisher's own wire answer is that gap in every case
  (`drop_unserved` in `lite/publisher.rs`), as is the local reader's. Only the warm copy is off.
- Warm copy: the oldest reader waits on its fill for key 0 (`Misses::lowest_wanted` is `Some(0)`).
  The fill's one-shot SUBSCRIBE (`SubFillRun`) is answered by `DROP 0..K NotFound`, and
  `req.reject(NotFound)` records it nowhere ("refused for now"). The reader's misses then give
  up `0..K` (`given_up = {0..K}`, `skipped = {}`), the wait ends, and the reader hands out the cached
  group K. The give-up is queued as a pending hole, but the ordered reader never returns it as a gap
  (`find_gap` after the read found nothing to report). The unwarmed reader gets the gap only because
  the subscription's own `DROP` reaches the track's drops before the group's stream does.
- Next: find why the queued miss `0..K` is not reported by the reader's `find_gap` after the read
  (instrument `Cursor::poll_drop` / `DropCursor::next`; its pending run printed as clipped by the
  window, yet `poll_drop` returned nothing), or record the fill's `NotFound` drop on the track as
  `fill::Inner::drop_groups` does so the drop cursor names it. Then run the nearby integration tests
  (`time_keyed_start`, `range_fill`, `waiting_time_start`, `finished_copy_history`, `late_tail_reader`)
  and the track.rs unit tests, and `/verify-subscribe` if tail accounting changes.
- The unwarmed relayed pass is ordering luck too; check the regression over several
  `KIO_SIM_SEED` values before and after the fix.

## Related

- [Test an oldest reader behind a relay copy a latest reader filled first](/quest/m4/time-keyed-warmed-relay-test.md) - the appended-group case, which already reads the same through a relay
- [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529) adds `quest/m1/relayed-refused-fill-gap.md` - the converse: a relayed reader reports a gap that a direct reader does not
