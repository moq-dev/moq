# [M] The send-stream trait reports acknowledgment one way

## Goal

`moq_net::transport::poll::SendStream` carries one acknowledgment primitive
when the cut freezes it: `Offset`, `offset()` and `poll_acked`. The trait is
public and unsealed, so each method that ships becomes one every out-of-tree
transport implements for good.

- `acknowledged()` and `OpenStream::acknowledged` are gone. The debug log in
  `rs/moq-net/src/transport/open.rs` that counts acknowledged open streams
  asks `poll_acked` at each stream's current offset instead.
- `delivered()` and `poll_delivered()` are gone. Egress stats
  (`stats::Delivery`, driven from `coding::writer`) bill a frame's bytes once
  `poll_acked` reports an offset past them as delivered. When it reports
  `Unsupported`, they bill the bytes the stream accepted from the writer, as the
  writer's fallback does today. They stop billing once it errors.
- Egress over QUIC stays acknowledgment-based: moq-tokio's and moq-uring's
  `quic::web` adapters implement `poll_acked` over their drivers before the
  stats move, or QUIC egress silently falls back to bytes taken.

## Plan

Found by the pre-cut feature review
([moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)) against
[moq-dev/moq#8739](https://github.com/moq-dev/moq/pull/8739), which added
`offset`/`poll_acked` beside the two older shapes. Decided 2026-11-15
(maintainer): land #8739 as it is and collapse the shapes here, keeping
`offset`/`poll_acked`.

Decided 2026-11-15 (maintainer): qmux keeps reporting `Acked::Unsupported`
(writing into TCP is not delivery, as decided 2026-10-22 in
[quic-ack-hook](/quest/m3/quic-ack-hook.md)), so qmux egress falls back to the
bytes taken. That is the count the stream accepted from the writer, not what
qmux's `delivered()` reports today (what the link took). The two differ only
while the link is blocked and holds accepted bytes. Rejected: giving the hook
qmux's "the link took it" meaning.

Guidance:

- `Delivery` can keep one outstanding offset per stream rather than one per
  frame: mint `offset()` at the bytes written when a frame waits, bill
  through it once delivered, then mint the next. That bounds memory per
  stream. The cost is that a reset can leave unbilled the bytes acknowledged
  past the last offset asked about. Measure that against per-frame offsets
  before picking.
- moq-quic's `send::Stream::poll_acked` is already public and handles the
  reset and STOP_SENDING cases. The adapters forward the driver's offset,
  which counts the WebTransport header, so no adjustment is needed. A web
  stream that was reset no longer has a driver handle, so it keeps the offset
  and the acknowledged prefix it had at the reset.
- The memory transport stands in for acknowledgment with the bytes its reader
  took, and moq-net's tests bill against it, so it gets a real `u64` offset.
- This takes part of [quic-ack-hook](/quest/m3/quic-ack-hook.md)'s driver
  half (the adapters' `poll_acked`). Update that quest to keep only the public
  driver API and its trap tests.
