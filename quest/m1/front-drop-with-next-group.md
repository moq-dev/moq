# [M] A front's ordered reader reports a drop that lands with the group above it

## Goal

An ordered reader (`ordered()`) of a logical track a front serves reports a dropped run as a gap
before the group above it, even when the drop and that group land in the same poll, as a reader
of a plain track does. Today it hands out the group and never reports the gap.

## Plan

Found while building [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529): on lite-06,
`drop_record::an_ordered_reader_is_told_of_every_group_the_origin_evicted` hit it once the origin
served its queued groups newest first. That PR lets group 0 go out before the rest queue, which
keeps the test on what it checks. The bug is as old as the front's ordered reader, and the repro
below fails on the commit before that PR too.

Repro, as a test in `rs/moq-net/src/model/resume.rs`'s `mod test`: serve one copy, subscribe
`ordered()` with `Range::groups(from..)` and an hour's budget, then `a.drop_groups(from..4,
StreamError::Old)` and `write(&a, 4)` before the next poll. The read yields `group 4` with no
`gap from..4`, for each of `(start, from)` in `[(None, 0), (None, 1), (Some(1), 1)]`.

Why: `Ordered::poll_item` in `rs/moq-net/src/model/track.rs` calls `find_gap` only after a read
comes back empty, so a group ready in the same poll goes out first and the gap below it is
passed. Calling `find_gap(false)` before every read fixes the repro, but it also makes
`waiting_time_start::a_range_before_every_group_reads_past_it` fail: see
[A relayed reader reports a refused fill as a gap](/quest/m1/relayed-refused-fill-gap.md), which
lands first or with it.

## Required

- [A relayed reader reports a refused fill as a gap](/quest/m1/relayed-refused-fill-gap.md) - the discrepancy the fix unmasks
