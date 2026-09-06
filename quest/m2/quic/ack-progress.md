# [S] noq-proto reports per-stream acknowledgment progress

## Goal

A released noq-proto lets a sender ask how much of a send stream the peer has
acknowledged and when each acknowledgment was received, corrected for the
peer's reported ACK delay. Nothing MoQ builds on top needs private state.

## Plan

The work lives in n0-computer/noq. This quest exists so the release it
produces is one condition dependents wait on.

noq-proto already tracks everything needed in `SendBuffer`: the acknowledged
range set, `fully_acked_offset()`, and the unacked length. `Connection` also
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

Propose the API on the noq tracking thread first and record the maintainers'
shape preferences in this quest. If the change is declined, this quest
creates the moq-dev fork per the [parent quest](/quest/m2/quic/parent.md)
and the release rules. The quest completes when a noq-proto release carries
the accessor and `Cargo.lock` here can name it.

## Required

- [Establish the noq relationship](/quest/m2/quic/parent.md) - creates the
  tracking thread this proposal goes to and names its reviewer

## Related

- [poll_acked in web-transport](/quest/m2/quic/ack-hook.md) - the first
  consumer of the accessor
