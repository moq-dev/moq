# [M] A lite-06 viewer joining a relay reads its first group again

## Goal

`relay_session_join_broadcasts` and `relay_session_join_stale` finish their
moq-lite-06 cells, as they did before
[moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476), so
`Nightly (Relay session benchmarks)` (`just gh nightly rust-relay`) finishes
inside its hour with every cell.

## Plan

Found 2026-11-12 while timing the relay session group for
[moq-dev/moq#8555](https://github.com/moq-dev/moq/pull/8555). The first
moq-lite-06 cell of `relay_session_join_broadcasts`
(`relays=1/publishers=1/broadcasts=1/viewers=16`) and of
`relay_session_join_stale` never leaves its warm-up: the bench thread parks in
the cell's `join` loop (`rs/moq-net/benches/session/mod.rs`) at 0% CPU,
waiting on a join, watch, or the first group read that never completes. The
moq-transport and moq-lite-07 cells of the same groups finish in seconds, and
`relay_session_join_relays` finishes on every version.

`git bisect` over `c916855d16..75f00f92c8` (the 11-11 Nightly's commit, whose
lite-06 join cells finished in 0.2 s, to main on 2026-11-12) names
f3edb0b8d9 ([moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476), "a
failed fetch below the end no longer holds it open") as the first hanging
commit. Reproduce with
`cargo bench -p moq-relay --features test-support --bench session -- --quick 'relay_session_join_broadcasts/moq-lite-06/'`.

Find which await hangs, reproduce it as a moq-net integration test of a
lite-06 viewer joining a relay that holds history, and fix the cause. A
viewer that never receives a group it subscribed to is a product bug, not
only a bench one, so do not skip the cells.

Then confirm a scheduled Nightly's relay session job finishes inside its
hour; never dispatch Nightly.
