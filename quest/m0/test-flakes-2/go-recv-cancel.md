# [S] The Go wrapper's cancelled-RecvGroup test holds up under load

## Goal

`go/wrapper/moq_test.go`'s `TestRecvGroupCancelKeepsConsumer` passes under a
loaded `just check`, and its `t.Skip` is removed.

## Plan

Planned 2026-11-09 (maintainer's standing rule that flakes join this epic).
It failed once in a loaded local `just check` for
[moq-dev/moq#8363](https://github.com/moq-dev/moq/pull/8363): after a
`RecvGroup` cancelled in flight, the next `RecvGroup` on the same consumer did
not see a group appended right after it and timed out at 5 s. It passed 30 of
30 runs alone. #8363 does not touch the path: `MoqTrackConsumer::recv_group`
reads through `Polled::wait` in `rs/moq-ffi/src/ffi.rs`.

A 5 s stall on an already-appended group is a lost wakeup, not slowness.
Reproduce under CPU contention (such as `-race -count` beside CPU burners),
then check whether the cancelled call's waiter stays registered on the bridge
and takes the wake meant for the next call. Fix it at the cause, remove the
`t.Skip`, and delete this quest.

Public API: none. Wire: none.
