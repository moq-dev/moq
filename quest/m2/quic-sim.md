# [L] QUIC on the sim

## Goal

The `moq-clock` sim drives two `moq-quic` endpoints over in-memory datagrams
with configurable latency, loss, reordering, and bandwidth, so QUIC behavior
(BBR, reliable reset, stream deadlines, the transport race) and MoQ over
real QUIC are tested deterministically with no sockets.

## Plan

Decided 2026-10-10: the in-memory session comes first
([the relay seam](/quest/m1/time/relay-seam.md)); simulated UDP follows.
`moq-quic` already takes `moq_clock::Instant`, so this is a datagram link plus a
driver. Start from moq-shaper's in-memory `UdpSocket` (`rs/moq-shaper/src/mem.rs`).
BBR's controller-level `Sim` in `bbr3/mod.rs` stays separate unless this
replaces it cleanly.

## Required

- [The relay seam](/quest/m1/time/relay-seam.md) - the sim already drives relay tests in memory

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - a consumer
- [QUIC bitrate caps](/quest/m2/rate-quic.md) - wants a simulated clock
