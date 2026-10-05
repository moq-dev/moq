# [S] TCP transports enforce bitrate caps

## Goal

A relay session over the WebSocket fallback, and a capped HTTP `/fetch` or
`/announced` request, is held to its `publish.rate` and `subscribe.rate`
like a QUIC session, and is admitted instead of refused.

## Plan

TCP carries the backpressure, but only if the kernel cannot absorb the
excess:

- **The bucket sits under the protocol**, as an IO wrapper on the upgraded
  socket (`rs/moq-relay/src/websocket.rs`) and the HTTP response body
  (`rs/moq-relay/src/web.rs`), with a cap handle set after `admit`. A bucket
  at the WebSocket message layer would accept a whole message (up to
  `max_message_size`) before charging it, so the burst would be one message,
  not about a second of `rate`.
- **Bound the kernel buffers.** A capped session sets `SO_RCVBUF` and
  `SO_SNDBUF` to about the burst, since paced reads cannot stop bytes the
  kernel already accepted and paced writes can queue a send buffer that drains
  faster than the cap. Fixing the size disables autotuning, which is the
  point.
- Reads stop at the bucket, so the client's TCP window closes; writes stop
  at the bucket, so a subscriber sees the cap. Set at auth and reset on
  revalidation. The io_uring stream sessions get the same wrapper once they
  serve WebSocket and HTTP. Drop the relay's refusal for WebSocket and HTTP in
  the same PR.

Tests: a client writing flat out is held to the cap plus the bounded
buffers; delivery to a capped subscriber or `/fetch` never exceeds it; a cap
change on a live session applies.

## Required

- [Bitrate claim](/quest/m2/rate-claim.md) - the cap this enforces
