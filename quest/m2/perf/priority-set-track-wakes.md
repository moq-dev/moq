# [S] Stop set_track waking groups that end up where they started

## Goal

`lite::priority`'s `set_track` (rs/moq-net/src/lite/priority.rs:342) re-ranks
an item as `extract` (:322) then `place` (:261). The extract shifts every following vec entry up one and the place shifts them back
down, so an entry whose rank does not net-change is still woken: the shift up
takes its parked waker, and the shift back finds nothing left to restore.

A track priority change should wake only the groups whose rank actually moved.

## Plan

The insert and remove paths are already exact, because each performs a single
monotone shift where every touched entry genuinely moves by one. Only the
double mutation in `set_track` can cancel itself out.

Two shapes, cheapest first:

- Defer the reconcile. Have `update_location` (:236) record the touched id and
  leave `PriorityEntry::rank` (:185) holding the last *published* rank, then compare once at
  the end of the mutation and wake only the net movers. Simple and total, but it
  adds a second slab lookup per shifted entry to the insert hot path, so measure
  it against `priority_queue_insert_front` (rs/moq-net/benches/priority.rs:25)
  before taking it.
- Rotate instead of remove-and-reinsert. Moving an item within the sorted vec
  only shifts the entries strictly between its old and new index, so computing
  that range directly is both exact and less work than the two full shifts. It
  has to keep the vec/overflow boundary in `place` intact, including the case
  where the re-ranked item crosses it.

The queue rework in #3298 left the wake count on this path as it was. It is a
cold path (a SUBSCRIBE_UPDATE
priority change), never group open or close.

Acceptance: a test that parks each handle on its own waiter, calls `set_track`
on the front entry within a range where it stays first, and asserts no other
handle woke. No regression on the `priority_queue_insert_front` benches.

## Related

- [Send order width](/quest/m2/perf/send-order-width.md) - makes this moot if the queue goes away
