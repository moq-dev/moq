# [M] A group read to its FIN stays finished across updates

## Goal

A lite-07 subscriber that read a group's stream to its FIN never records that
group stale, however many SUBSCRIBE_UPDATEs it sent since, so the publisher
never serves it again or names it in another drop, as the draft promises.

## Plan

Found 2026-11-10 by the subscribe model ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`rs/moq-net/verify/subscribe/StreamWins.cfg` breaks `StreamWins`. The
subscriber reads a group whole, then sends an update; the group goes stale
before the publisher sees the FIN acknowledged, so it is named in a stale drop
under that update. `Tail::updated` in `rs/moq-net/src/tail.rs` already cleared
`Tail::finished`, so once the cache evicted the group the subscriber records
it stale and owes it again: the publisher resends it whole, or drops it again
on every later update. `residual_a_stale_unacknowledged_fin_drops_and_resends_a_served_group`
in `lite/publisher.rs` pins the publisher's half;
`a_finished_group_wins_over_a_late_stale_drop_once_evicted` in
`lite/subscriber.rs` pins the subscriber's.

Decided 2026-11-10 (maintainer, /quest-plan): ✅ the subscriber keeps
`Tail::finished` across updates, bounded by forgetting entries below the
tail's floor, and `js/net/src/lite/subscriber.ts` does the same (rejected: the
publisher skipping the drop for a stream that expires after its FIN, which
leaves a subscriber whose stream was lost waiting out its grace period). The
model passes all four rules with this change (about 1.9M states).

Flip both tests, move `StreamWins` into `SubscribeAccounting.cfg`, delete
`StreamWins.cfg`, and update the `verify-subscribe` skill.

Caused by: [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670), which
added budget-qualified stale drops and the per-update `finished` record.

Public API: none. Wire: none.
