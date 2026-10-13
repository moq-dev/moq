# [S] A fired deadline costs a moq-transport driver one poll

## Goal

A moq-transport (IETF) session's driver is polled once per fired
`time::Deadline`, as #5393 makes a moq-lite session's, and
`rs/moq-net/tests/idle_session.rs` bounds the IETF driver at one poll per
refresh too.

## Plan

Start after #5393 (open as of 2026-10-13) merges, and remeasure on top of it.
#5393 adds `kio::Waiter::batch`, which holds a `Tasks` set's wake of its own
owner during that owner's poll. On that branch, IETF sessions still poll
twice: the driver is a boxed future, so its children's sets are woken inside
nested polls that the batch scope does not cover. Find where the nested poll
escapes the scope and cover it, or restructure the driver so its children
belong to the driver's own `Tasks` set.

Measure with the existing `session_idle` bench and the poll-counting harness
(`rs/moq-net/tests/support/harness.rs`).

Public API: none expected. Wire: none.

