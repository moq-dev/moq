# [S] A group a subscriber gives up on is always reported

## Goal

Every group a moq-lite subscriber gives up on surfaces to the application.
In the API, `track::Subscriber`'s drop signal (today the crate-private
`poll_drop`, which the ordered reader and resume use to skip gaps) is public,
so the receive-windows benchmark and applications can count losses instead of
waiting forever, and that benchmark's overcommitted cells finish with
`delivered_frames` plus the reported drops accounting for every group sent.

## Plan

Planned 2026-11-04 with the maintainer, from
[moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838): no public API
reports a group the subscriber dropped without `recv_group` returning it, so a
reader that paces on delivered-or-evicted groups stalls. Split 2026-11-14 (the
m2 triage before the cut, under /quest-afk, for the maintainer's review):
the wire half made every abandoned group known and decided lite-07's part;
this publishes it. It stays in m2 because the `Ordered` change below is
breaking.

The wire half ([moq-dev/moq#8774](https://github.com/moq-dev/moq/pull/8774))
found every group the overcommitted cells lose already in the drop set: a
scratch reader polling `poll_drop` beside `recv_group` finished every cell of
the receive-windows benchmark with delivered, evicted-after-header and dropped
groups summing to those sent, on lite-06 and lite-07. So surfacing
`poll_drop` alone unskips those cells.

- ✅ **Milestone** (maintainer, 2026-11-04): it changes public API, so it
  lands before the cut.
- **After the drop set.** Drop set finished in
  [moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476) on 2026-11-12.
  `poll_drop` now takes no `Delivered` argument and yields the given-up run
  with its `Hole` (`rs/moq-net/src/model/track.rs`, `track/hole.rs`). Publish
  the API against that record. The drop set owns the per-cursor drop runs and
  their reasons; this reports what a cursor gives up.
- Mirror it in `@moq/net` and the bindings only for a named consumer (for
  example a player or container reader that skips gaps), per the API audit
  rule that new surface needs one; name it in the PR or leave the mirror out.

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and
remaining quests): ✅ key the drop signal by group start and land after
[time addressing's model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md), so its public
API never ships sequence-keyed (rejected: landing first and rekeying with
the model). ✅ Drops surface as a `Gap { range, error }` item on
`track::Ordered`, which already skips gaps through `poll_drop`, mirroring
the container consumer's `Read::Gap` (`rs/moq-mux/src/container/consumer.rs`);
`recv_group` is unchanged (rejected: a separate `recv_drop` read; a drop
variant on `recv_group`).

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ a
drop is a span and nothing more: SUBSCRIBE_DROP carries no group count on
lite-07, and `Gap` reports its start-keyed `range` with no count (rejected:
a count on SUBSCRIBE_DROP that `Gap` passes on, which would also have let a
skip become a drop with a count of 0). This settles the question left open
here on 2026-11-07, against its recommendation. Once starts are sparse a
span's length is not a number of groups, and only the node that dropped
them could count; a relay's downstream would need the count carried on the
wire at every hop for a number few readers use. A reader that needs a count
measures what it lost in time, which the span gives, or counts the groups
it did receive. So "count losses" in the Goal means account for every span:
the receive-windows benchmark's overcommitted cells finish when delivered
groups plus reported spans cover everything sent. At HEAD
`SubscribeDrop`'s `Group Count` (`rs/moq-net/src/lite/subscribe.rs`) is a
count of keys, which [the wire rewrite](/quest/m2/time-addressing/lite07.md)
turns into the span's end.

Decided 2026-11-16 (the unused moq-net API sweep): once the container reader
reads gaps through `Gap`, `track::Subscriber::next_in_range`, `is_skipped`,
and `poll_start` (their one consumer is moq-mux's container reader, which
orders groups itself) go crate-private in the same PR. So does
`Subscriber::with_range`, whose last production caller is
`rs/moq-mux/src/container/source.rs`'s resubscription, which leaves out the
group it already read (a time start names the group containing it, so a
`Subscription` range cannot exclude one); if the reader can skip that group
through `Gap` or its own cursor, the sweep's other callers (tests in moq-mux
and moq-json) move to `Subscription::ranges`, otherwise it stays.

Public API: breaking for `track::Ordered` only. Its one read,
`poll_next_group` (and its async wrapper), returns
`Result<Option<Item>>` instead of `Result<Option<group::Consumer>>`, where
`Item` is a group or a `Gap { range, error }`; `Ok(None)` still ends the
read and `Err` still carries a track abort.
Callers match the new item, and a caller that ignores drops takes the group
arm; the PR's MIGRATE notes say so. `recv_group` and the unordered cursor are
unchanged, so a reader that needs drop accounting reads through `Ordered`.
`@moq/net` and the bindings mirror it only with a named consumer.
Wire: none.

## Related

- [The lite-07 wire addresses groups by start](/quest/m2/time-addressing/lite07.md) - SUBSCRIBE_DROP names the time spans `Gap` reports
- [The receive-windows benchmark](/quest/m3/receive-windows-evicted.md) - its overcommitted cells wait on this (skipped by [moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838) until it lands)
