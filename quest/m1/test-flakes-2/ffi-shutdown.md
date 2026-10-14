# [S] moq-ffi shutdown tests hold under load

## Goal

`moq-ffi`'s `shutdown_delivers_the_finished_track` and
`shutdown_times_out_on_an_unfinished_track` (`rs/moq-ffi/src/test.rs`) pass
under a loaded `just check`, fixed at their cause. They still prove that a
graceful shutdown delivers a finished track's tail, and gives up on an
unfinished one after the documented one second with `DeliveryTimeout`.

## Plan

Seen 2026-10-13 on #5471 and #5486, once each under a loaded `just check`;
both passed alone. Which assertion failed wasn't recorded. Both tests run a
real QUIC pair on the wall clock and bound most steps with a 10 s `TIMEOUT`,
but not all: the first leaves the detached `shutdown()` unbounded, and the
second leaves `shutdown_pair()`, the subscribe, and `shutdown()` unbounded
while asserting shutdown took at least one second.

Reproduce under load first and record the failing assertion, or the stage
that hung if no assertion fired. Then move the tests to a paused clock if
the transport allows it, or assert on events rather than elapsed time, per
the epic's rules. If shutdown itself loses the tail or drains for the wrong
time under load, that is a product bug: fix shutdown. Never widen a timeout
or add a retry.

Public API: none expected. Wire: none.
