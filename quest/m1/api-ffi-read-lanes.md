# [M] Let FFI group and datagram reads progress independently

## Goal

A pending group read never prevents a datagram read from completing, and a
pending datagram read never prevents a group read on the same subscription.

## Plan

Source evidence at dev `e2350b39a`: `MoqTrackConsumer` owns one
`Task<TrackInner>` (`rs/moq-ffi/src/consumer.rs:459`). Both read lanes use it
(`:507`, `:535`), and `Task::drive` holds its mutex across the entire awaited
operation (`rs/moq-ffi/src/ffi.rs:203`). Start a group read on an idle track,
then read and publish a datagram: the datagram reader waits for the group
operation to release the lock. Reversing the calls reverses the starvation.
This is source-traced, not yet an executed regression.

Implement independent progress in the shared FFI core while preserving one
subscription, one delivery-mode commitment, subscription control, and bounded
buffering. Prefer demand-driven polling coordination; do not hide the lock
problem behind an eager unbounded queue, a second subscription, or a timeout.

Reproduce both directions in the existing FFI suite, including ordered and
arrival modes, update during a pending read, per-call cancellation, and whole
handle cancellation. `test.rs:2156` currently exercises the lanes sequentially.
Go read deadlines cancel the entire handle (`go/wrapper/subscribe.go:282`);
preserve that documented distinction from cancellation of an individual
native async call unless a separate API decision changes it.

Public API: preserve the current surface if the shared implementation can
satisfy it; any handle split needs a maintainer decision and all wrappers.
Wire: none. Run `just check`, `just test`, and `just test smoke-full`.
