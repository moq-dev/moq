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

Decided 2026-10-22 (maintainer), from the io_uring audit: the ring's media
I/O beyond QUIC stays measure-gated. Iroh and WebRTC data channels stay on
tokio, and TCP, WebSocket, and Unix keep working on uring relays through the
shared tokio server.

Public API: none until the port. Wire: none.

## Required

- [Profile the io_uring workers](/quest/m2/perf/uring-profile.md) - the recipe this measures with

## Related

- [Stream sessions on the ring](/quest/m3/uring-tcp/README.md) - the TCP half, gated the same way
