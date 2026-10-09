# [XS] Document aggregate closure counters on rejoin

## Goal

`moq_stats::aggregate` documents, and one test pins, what a same-epoch
rejoin within the grace does to the `*_ended` counters that `Traffic`'s
`Mergeable::retire` advanced on departure: they may regress. A restart is not
this case: since #4904 it announces a new epoch, whose counters add to the
old one's kept contribution (`rs/moq-stats/src/aggregate.rs`).

## Plan

Decided by [#4874](https://github.com/moq-dev/moq/pull/4874) (recorded in the
2026-10-06 audit): the restart and after-grace contract. A node returning
within `Config::grace` with lower counters regresses the merged counter, the
same fresh-segment rule a single node's restart follows; after the grace its
contribution folds into one retired total. `return_within_grace_resumes_counters`
and `return_after_grace_counts_twice` pin it.

`retire` raises `announces_ended`, `broadcasts_ended`, and
`subscriptions_ended` to their `*_started` on departure, so the next raw frame
from the same node, rejoining within the grace with its boot-lifetime
counters intact, can show a lower `*_ended` value. The stickiness fix
([moq#3625](https://github.com/moq-dev/moq/pull/3625)) review raised it as a
P2.

Decided 2026-10-08: keep the current behavior (retire on departure, accept an
`*_ended` regression on a same-epoch rejoin within the grace), since the
regression is bounded by the closures retire assumed, it ends once the node's
own counters pass them, and no consumer has asked for anything else. Rejected: keeping
the retired values as a floor until the node's own counters pass them, which
suppresses a still-live node's closures after a transient reader failure.
Document it in the consumer-facing contract and add one test of a rejoin
within the grace.
