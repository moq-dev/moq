# [S] A fired deadline costs a moq-transport driver one poll

## Goal

A moq-transport (IETF) session's driver is polled once per fired
`time::Deadline`, as a moq-lite session's is since #5393, and
`rs/moq-net/tests/idle_session.rs` bounds the IETF driver at one poll per
refresh too.

## Plan

#5393 added `kio::Waiter::batch`, which holds a `Tasks` set's wake of its own
owner during that owner's poll. IETF sessions still poll twice: the driver is
a boxed future, so its children's sets are woken inside nested polls that the
batch scope does not cover. Find where the nested poll escapes the scope and
cover it, or restructure the driver so its children belong to the driver's
own `Tasks` set.

Measure with the existing `session_idle` bench and the poll-counting harness
(`rs/moq-net/tests/support/harness.rs`).

Public API: none expected. Wire: none.

