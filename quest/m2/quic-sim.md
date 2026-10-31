# [M] The relay over QUIC on the sim

## Goal

The relay, and moq-tokio's `Client` and `Server`, run over `moq-quic` on the
`moq-clock` sim with configurable latency, loss, reordering, and bandwidth,
so relay hops, the transport race, and reconnects over real QUIC are tested
deterministically with no sockets.

## Plan

Already landed: MoQ sessions over moq-quic on the sim. The QUIC driver
spawns on the simulator inside `moq_clock::sim::run`, under moq-tokio's
hidden `sim` feature. `quic::driver::sim::Network` binds a socket per address
and carries datagrams across moq-quic's seeded `Link` (now
`moq_quic::sim`), so one seed replays a run.
`rs/moq-tokio/tests/integration/sim.rs` builds both endpoints on the raw
driver and seeds their `rng_seed` and BBR3's probe phases.

What remains:

- moq-tokio's `Client` and `Server` bind their QUIC endpoint from an address
  (`crate::bind::udp`), so a sim test cannot reach them yet. Pick how a
  caller hands either one a `driver::endpoint::Socket`, such as a hidden
  `sim`-only hook. That API choice belongs to the maintainer.
- Route the session-level spawns that `server.rs`, `connection.rs`, and
  `origin.rs` still send through `crate::runtime::spawn` through
  `runtime::Spawner`, as `transport.rs` and the WebTransport session already
  do.
- Then run `moq_relay` between a publisher and a subscriber, and between two
  relays, on one `Network`. Also run the client's Happy Eyeballs race across
  addresses with different paths, and a WebTransport (`https`) dial beside
  raw QUIC.

Keep random loss for recovery correctness. Any acceptance or performance
consumer uses a rate below its media bitrate behind a deep but finite queue,
and checks that the queue overflowed. A stalled sender that never filled the
queue is not evidence about congestion loss.

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - a consumer
- [QUIC bitrate caps](/quest/m2/rate-quic.md) - wants a simulated clock
