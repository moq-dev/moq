# [S] Keep aggregate closure counters monotonic

## Goal

`moq_stats::aggregate` has one stated, tested rule for whether a same-lifetime
rejoin within the grace may regress the `*_ended` counters that
`Traffic::retire` advanced on departure, documented beside the existing
restart contract in `rs/moq-stats/src/aggregate.rs`.

## Plan

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer.

Decided by [#4874](https://github.com/moq-dev/moq/pull/4874) (recorded in the
2026-10-06 audit): the restart and after-grace contract. A node returning
within `Config::grace` with lower counters regresses the merged counter, the
same fresh-segment rule a single node's restart follows; after the grace its
contribution folds into one retired total. `return_within_grace_resumes_counters`
and `return_after_grace_counts_twice` pin it.

Still open: `Traffic::retire` raises `announces_ended`, `broadcasts_ended`,
and `subscriptions_ended` to their `*_started` on departure, so the next raw
frame from the same node, rejoining within the grace with its boot-lifetime
counters intact, can show a lower `*_ended` value. The stickiness fix
([moq#3625](https://github.com/moq-dev/moq/pull/3625)) review raised it as a
P2. Two directions were discussed:

- Retire on departure (current), accepting an `*_ended` regression on rejoin.
- Keep the retired values as a floor until the node's own counters pass
  them, which suppresses a still-live node's closures after a transient
  reader failure.

Pick one, test it, and document it as part of the consumer-facing contract.
