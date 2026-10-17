# [S] Rust ordered reads take the lowest group across route copies

## Goal

A Rust reader reading a logical track in sequence order (`poll_next_group`)
across a same-epoch route swap never skips a group a replaced route's copy
already holds just because the serving copy offered a later one first. This
matches JS since #5836.

## Plan

Decided 2026-10-17 by the maintainer on #5836: JS keeps reading the lowest next
group across copies, and Rust follows rather than JS reverting.

Today `Reader::poll_group_once` in `rs/moq-net/src/model/resume.rs` polls the
serving copy first in both orders. In sequence order, `track::Subscriber::hand_out`
then forgets every sequence below the group it returns. So a replaced copy's
buffered N+1 is dropped when the serving copy offers N+2 first, a group a direct
ordered reader would have delivered.

- Mirror JS `Reader.#lowest`: in sequence order, peek each live copy's next
  deliverable group without moving its cursor, drop what is not deliverable,
  and take from the copy with the lowest one. Arrival order keeps serving-first.
- The peek needs a non-consuming counterpart of `poll_next_group` on
  `track::Subscriber` that applies the same floor, cap, and drift budget.
- Feed the taken group through `Reader::observe` as the other paths do, so the
  deferred bound on replaced copies still applies.
- Regression test: an ordered reader across a swap where the old copy holds
  group 1 unread and the new copy only has group 2 reads 1 then 2, as JS
  `an ordered reader takes a replaced copy's buffered group before the new
  copy's later one` does.

Public API: behavior only. Wire: none.

## Related

- [Resume reorder](/quest/m1/resume-reorder.md) - another resume verdict that reads a passed group too eagerly
