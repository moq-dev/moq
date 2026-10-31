# Byte streams over MoQ

## Goal

A TCP connection rides MoQ as a lossless, ordered, duplex byte stream through
any relay chain, proxied at the TCP level and never as IP packets. Each hop
runs its own QUIC congestion control and retransmits, so a lossy last mile
recovers locally instead of end to end, and relays schedule streams against
media by the existing priority rules. Rust and JS expose the same surface.

Non-goals: IP or datagram tunneling, which would throw away the CDN's per-hop
QoS; replaying a stream to a late joiner; fanout byte streams beyond what
fastest-reader pacing gives.

## Plan

Decided 2026-10-25 with the maintainer (mesh planning interview):

- TCP-level proxying over groups, never IP over datagrams.
- Each direction is one track. A connection is one group on each; each socket
  `read()` becomes one frame, because a frame's length must be known before it
  is written and a read has one. It coalesces whatever is buffered with no
  added timer (the local socket's own Nagle or `TCP_NODELAY` still applies).
- The receiver flushes a frame's bytes to its socket as they arrive, never
  waiting for the whole frame, unless the mesh's authentication level
  (`moq-sign`) must verify it first; a frame that fails verification ends its
  group with a typed error, surfaced as a reset.
  Group finish is a FIN (half-close); a group abort with a code is a reset.
  The implementer measures and records a frame-size cap.
- Lossless delivery comes from
  [fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md), not
  from refill: a stream registers no fill handler, because most TCP traffic is
  not worth retaining for replay. A reader that falls behind the window ends
  `Evicted`, surfaced as a reset.
- Connection setup is generic: a listener accepts connections a dialer
  announces under a path prefix, and the API names peers by path. Endpoint
  identity, signing, and service names belong to the
  [mesh](/quest/m2/mesh/README.md), which builds on this.

This epic owns the end-to-end proof: an unmodified TCP client and server (SSH
and an HTTP bulk transfer) talk through a two-relay chain with loss injected on
one hop, with no corruption, FIN and reset propagated, and memory bounded by
the window.

## Required

- [Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md) - relays push back instead of evicting a reader that keeps up
- [Rust byte streams](/quest/m2/stream/rust.md) - a duplex `AsyncRead`/`AsyncWrite` stream, a listener and a dialer, and `moq` CLI forwarding
- [JS byte streams](/quest/m2/stream/js.md) - the same surface in `@moq/net`
- [Stream benchmark](/quest/m2/stream/bench.md) - throughput and latency against raw TCP through a relay chain, with and without loss
