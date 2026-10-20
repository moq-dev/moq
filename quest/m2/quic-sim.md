# [L] QUIC on the sim

## Goal

The `moq-clock` sim drives two `moq-quic` endpoints over in-memory datagrams
with configurable latency, loss, reordering, and bandwidth, so QUIC behavior
(BBR, reliable reset, stream deadlines, the transport race) and MoQ over
real QUIC are tested deterministically with no sockets.

## Plan

The in-memory session and the relay connection over it
(`moq_relay::Accepted`) landed first; simulated UDP follows.
`moq-quic` already takes `moq_clock::Instant`, so this is a datagram link plus a
driver. Start from moq-shaper's in-memory `UdpSocket` (`rs/moq-shaper/src/mem.rs`).
BBR's controller-level `Sim` in `bbr3/mod.rs` stays separate unless this
replaces it cleanly.

Keep random loss for recovery correctness. Any acceptance or performance
consumer uses a rate below its media bitrate behind a deep but finite queue,
and checks that the queue overflowed. A stalled sender that never filled the
queue is not evidence about congestion loss.

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - a consumer
- [QUIC bitrate caps](/quest/m2/rate-quic.md) - wants a simulated clock
