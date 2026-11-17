# [S] A subscription's drop ledger is billed and folds under memory pressure

## Goal

The runs a publisher's SUBSCRIBE_DROP and SUBSCRIBE_SKIP messages add to a
relay subscription's tail ledger are charged to that copy's account and fold
away under cache-pool pressure, so a publisher sending alternating
single-group drops cannot grow relay memory for the life of a subscription
that has no max age and no max delay.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; confirmed from source.
`Tail::account` (`rs/moq-net/src/tail.rs`) records any range the peer names,
and `rebill` charges only `finished`, never the ledger's runs. With no track
max age, no `cache.duration`, and no viewer max delay, the grace is
`Duration::MAX`, so `ledger.rs` never folds a run: each 4 to 6 wire bytes of
drop leaves about 160 B on the relay. On `release` the ledger was unbilled
too, but an unset max age gave a 1 s grace.

Recommended: bill the ledger's runs to the copy's account and fold the
oldest gap with `Ledger::fold_oldest` under pressure, as the track's own
arrivals ledger already does (rejected: a cap on runs, which the review
rules refuse). This holds whatever
[the tail drop count](/quest/m3/lite-tail-drop-count.md) later decides about
the grace.

Test: apply N disjoint drops on a track with no max age and no max delay,
advance the simulated clock, apply pool pressure, and assert the ledger
folds.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597) - made an unset max age an unbounded grace

## Related

- [Tail drop count](/quest/m3/lite-tail-drop-count.md) - removes the grace from deciding the tail
