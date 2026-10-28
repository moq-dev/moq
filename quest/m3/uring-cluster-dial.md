# [M] Cluster dials on the ring

## Goal

A measured verdict on whether an io_uring relay's outbound cluster links
should run on its workers instead of the shared tokio runtime. The port
lands only if the measurement shows a win.

## Plan

`Cluster` holds every outbound link as a `moq_tokio::Client`
(`rs/moq-relay/src/cluster.rs`), wired in by `with_connect` whatever runtime
serves inbound traffic, so a ring relay forwards its cluster media through
tokio. A port needs the ring client to grow what `moq-tokio` provides:
DNS and address racing, reconnect, and fingerprint pinning (the raw ring
client takes one `SocketAddr`). Share that policy with `moq-tokio` rather
than duplicate it.

Measure first with a prototype dialing a fixed address from a worker:
CPU per Gbps and latency on the relay-to-relay fanout shape, ring against
tokio, swept over links and subscribers.

Decided 2026-10-22 (maintainer), from the io_uring audit: cluster dials
stay measure-gated. Iroh and WebRTC data channels stay on tokio. TCP and
WebSocket move onto the workers through
[Stream transports](/quest/m2/perf/stream/README.md), which its own
decision exempts from a gate.

Public API: none until the port. Wire: none.

## Related

- [Stream transports](/quest/m2/perf/stream/README.md) - the TCP and WebSocket half, ported in m2
