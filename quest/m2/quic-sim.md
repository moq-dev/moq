# [L] QUIC on the sim

## Goal

The `kio::clock` sim drives MoQ sessions and the relay over `moq-quic`, with
configurable latency, loss, reordering, and bandwidth, so MoQ over real QUIC
(the transport race, relay hops, recovery) is tested deterministically with
no sockets.

## Plan

The in-memory session and the relay connection over it
(`moq_relay::Accepted`) landed first. `moq-quic`'s own tests already run two
endpoints on a seeded, impaired sim (`Pair` and `Link` in
`rs/moq-quic/src/tests/{util,sim}.rs`, from
[#5780](https://github.com/moq-dev/moq/pull/5780) and
[#5989](https://github.com/moq-dev/moq/pull/5989)), so QUIC-level behavior is
covered there. Decided 2026-10-22 (maintainer): rescoped to MoQ sessions and
the relay over that sim, reusing `Pair` and `Link` rather than starting from
moq-shaper's in-memory `UdpSocket`.

Keep random loss for recovery correctness. Any acceptance or performance
consumer uses a rate below its media bitrate behind a deep but finite queue,
and checks that the queue overflowed. A stalled sender that never filled the
queue is not evidence about congestion loss.

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - a consumer
- [QUIC bitrate caps](/quest/m2/rate-quic.md) - wants a simulated clock
