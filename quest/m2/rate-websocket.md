# [S] WebSocket enforces bitrate caps

## Goal

A relay session over the WebSocket fallback is held to its `publish_rate`
and `subscribe_rate` like a QUIC session, and is admitted instead of refused.
When caps are reintroduced, HTTP `/fetch` and `/announced` must refuse them:
a request has no connection to meter.

## Plan

Token claims and HTTP grants carry path patterns only; object roles, including
bitrate caps, are refused during parsing. Decided 2026-10-25 (maintainer):
[QUIC caps](/quest/m2/rate-quic.md) reintroduces optional caps with a type
moq-auth owns (or a lower shared units crate), converted at the relay boundary
so moq-auth never depends on moq-net, and [the grant](/quest/m2/rate-grant.md)
Requires it. This quest reuses that type. Its parsers and refusal rules must
prevent any transport without enforcement from silently discarding a cap, so
WebSocket admission keeps refusing caps until this wrapper enforces them.

TCP carries the backpressure, but only if the kernel cannot absorb the
excess:

- **The bucket sits under the protocol**, as an IO wrapper on the upgraded
  socket (`rs/moq-relay/src/websocket.rs`, an HTTP/1.1 upgrade on its own
  socket), with a cap handle set after `admit`. A capped WebSocket over h2
  extended CONNECT (RFC 8441), should the listener ever enable it, shares a
  socket and is refused. A bucket
  at the WebSocket message layer would accept a whole message (up to
  `max_message_size`) before charging it, so the burst would be one message,
  not about a second of `rate`.
- **Bound the kernel buffers.** A capped session sets `SO_RCVBUF` and
  `SO_SNDBUF` to about the burst, since paced reads cannot stop bytes the
  kernel already accepted and paced writes can queue a send buffer that drains
  faster than the cap. Fixing the size disables autotuning, which is the
  point. The buffers bound the burst only; the sustained cap comes from the
  paced IO. Behind a TCP-terminating proxy, the buffers bound only the
  relay's side.
- Reads stop at the bucket, so the client's TCP window closes; writes stop
  at the bucket, so a subscriber sees the cap. Set at auth and reset on
  revalidation. The io_uring stream sessions get the same wrapper once they
  serve WebSocket. WebSocket admission accepts capped roles only once this wrapper enforces them.

Tests: a client writing flat out is held to the cap plus the bounded
buffers; delivery to a capped subscriber never exceeds it; a cap change on a
live session applies.

## Required

- [QUIC caps](/quest/m2/rate-quic.md) - introduces the moq-auth cap type this reuses

## Related

- [TCP acceptor](/quest/m2/one-port/tcp-demux.md) - boxes the accepted stream, so it must keep the socket handle these buffer sizes need
