# [S] The session burst bench completes at every shape

## Goal

`rs/moq-net/benches/session.rs`'s `burst` sweep runs to completion at 16
subscriptions and 16 or more groups per round, with or without the kio
budget, and the sweep covers those shapes again.

## Plan

Found while landing serve-budget: the bench hangs at those shapes on `main`
too, so the sweep was limited to shapes that complete. Decided 2026-10-08:
treat it as a possible session stall, not a bench setup limit. Find whether
it is cache size, stream credit, or a real stall, fix it at the cause, and
add a regression test if the cause is in the session rather than the bench.

## Required

- [A busy task yields within a budget](/quest/m0/serve-budget.md) - adds the `burst` axis
