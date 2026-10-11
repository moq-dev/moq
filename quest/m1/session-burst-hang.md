# [S] The session burst bench completes at every shape

## Goal

`rs/moq-net/benches/session.rs`'s `burst` sweep runs to completion at 16
subscriptions and 16 or more groups per round, with or without the serve
budgets, and the sweep covers those shapes again.

## Plan

Found while landing serve-budget
([#5088](https://github.com/moq-dev/moq/pull/5088)), which adds the `burst`
axis: with that axis applied, the bench hangs at those shapes without the
budget too, so the sweep was limited to shapes that complete. Decided 2026-10-08:
treat it as a possible session stall, not a bench setup limit. Find whether
it is cache size, stream credit, or a real stall, fix it at the cause, and
add a regression test if the cause is in the session rather than the bench.
