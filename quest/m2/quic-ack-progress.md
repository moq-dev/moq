# [S] moq-quic reports per-stream acknowledgment progress

## Goal

`moq-quic` lets a sender ask how much of a send stream the peer has
acknowledged and when each acknowledgment was received, corrected for the
peer's reported ACK delay. Nothing MoQ builds on top needs private state.

## Plan

The work lives in `moq-quic`, in-tree after [the fork](/quest/m1/quic/fork/README.md).

`moq-quic`'s `SendBuffer` (`connection/send_buffer.rs`) already tracks most
of what is needed: the acknowledged range set (`acks`), the write offset, the
unacked length, and `is_fully_acked`; the contiguous acknowledged prefix
follows from them. `Connection` also
computes the peer's `ack_delay` per ACK frame when it updates the RTT
estimator. Expose that state through the public `SendStream` handle without
copying it:

- an accessor for the contiguous acknowledged prefix of the stream;
- an event or waker registration that fires when that prefix crosses an
  offset the caller names, so a caller can await "bytes below X are
  acknowledged" without polling;
- the receive instant of the ACK that advanced the prefix, minus the peer's
  reported ACK delay. That subtraction removes the delayed-ACK timer (up to
  `max_ack_delay`, 25 ms by default) so a delivery-latency sample sees the
  network path and not the receiver's batching. State clearly that the
  instant still includes the return one-way delay.

Match Quinn semantics for partial ACKs, retransmission of a range that was
already partly acknowledged, a stream that is reset by either side, and
stream teardown: the accessor must not return a prefix that includes bytes the
peer will never acknowledge, and a waiter for an offset beyond the final
size must resolve with the reset instead of hanging.

The `web-transport-moq` half of [the ACK hook](/quest/m2/quic-ack-hook.md)
is in-tree too and can land in the same PR.

Decided in the 2026-09-30 audit: moved to m2 with its consumers, the ACK
hook and [frame-granularity starvation](/quest/m2/starvation-frames.md).

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the accessor lands in `moq-quic`

## Related

- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - the first
  consumer of the accessor
- [noq#808](https://github.com/n0-computer/noq/issues/808) - the acked-offset ask to n0
