# [M] The relay over QUIC on the sim

## Goal

The relay, and moq-tokio's `Client` and `Server`, run over `moq-quic` on the
`kio::clock` sim with configurable latency, loss, reordering, and bandwidth,
so relay hops, the transport race, and reconnects over real QUIC are tested
deterministically with no sockets.

## Plan

Already landed: MoQ sessions over moq-quic on the sim. The QUIC driver
spawns on the simulator inside `kio::clock::sim::run`, under moq-tokio's
`sim` feature. `quic::driver::sim::Network` binds a socket per address
and carries datagrams across moq-quic's seeded `Link` (now
`moq_quic::sim`), so one seed replays a run.
`rs/moq-tokio/tests/integration/sim.rs` builds both endpoints on the raw
driver and seeds their `rng_seed`, which also seeds each connection's
congestion controller.

What remains:

- moq-tokio's `Client` and `Server` bind their QUIC endpoint from an address
  (`crate::bind::udp`), so a sim test cannot reach them yet. Pick how a
  caller hands either one a `driver::endpoint::Socket`, such as a
  documented `sim`-only hook (`just check` bans `#[doc(hidden)]`). That API
  choice belongs to the maintainer (see Waiting). `Client::with_network`
  (`rs/moq-tokio/src/sim.rs`) already gives a client an in-memory network
  under the `sim` feature, and the relay's integration tests run hops and
  reconnects over it (`rs/moq-relay/tests/integration/memory.rs`), though
  not over QUIC.
- Route the session-level spawns that `server.rs` and `origin.rs` still
  send through `crate::runtime::spawn` through
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

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): ✅ m1
(rejected: staying in m4). It is the base for [fault schedules](/quest/m4/cluster-fault-schedules.md).

## Waiting

- Maintainer: how moq-tokio's `Client` and `Server` take a simulated QUIC socket. Options: (a) extend the `sim`-feature `Client::with_network` path, already documented as tracking internals that can change in any release, to carry `quic::driver::sim::Network`, with a matching hook on `Server`; (b) a new `sim`-only `with_socket(driver::endpoint::Socket)` on each; (c) drop this quest, since the memory network already covers relay hops and reconnects. Recommended: (a) if a failure only real QUIC would show is named, otherwise (c).
