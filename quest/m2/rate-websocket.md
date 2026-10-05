# [S] WebSocket enforces bitrate caps

## Goal

A relay session over the WebSocket fallback is held to its `publish.rate`
and `subscribe.rate` like a QUIC session, and is admitted instead of refused.

## Plan

TCP already carries the backpressure: the relay reads from the socket no
faster than `publish.rate` (a token bucket of about one second of `rate`),
so the client's send buffer fills and its TCP stack backs off, and writes no
faster than `subscribe.rate`. Lives in `rs/moq-relay/src/websocket.rs` (and
the io_uring stream sessions once they serve WebSocket), set at auth and
reset on revalidation. Drop the relay's refusal for WebSocket in the same PR.

Tests: a client writing flat out is read at the cap; writes to a capped
subscriber never exceed it; a cap change on a live session applies.

## Required

- [Bitrate claim](/quest/m2/rate-claim.md) - the cap this enforces
