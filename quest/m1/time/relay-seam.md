# [L] The relay seam

## Goal

One in-memory session pair implements moq-net's poll transport traits and
replaces the duplicate poll-session mocks (`rs/moq-net/tests/support/mock.rs`,
`rs/moq-e2ee/tests/support/mock.rs`, and the session bench's transport).
moq-tokio's `FakeSession` and `Scripted` stay: they implement
`web_transport_trait::Session` to test moq-tokio's adapter onto the poll
traits, which this pair cannot stand in for. `moq_relay::Connection` runs
over any moq-net transport session, not only a `moq_tokio::server::Request`,
so moq-relay's integration tests (auth, the cluster origin, stats) run on the
sim with no sockets, and [the relay session bench](/quest/m1/bench-relay.md)
uses the same seam.

## Plan

Decided 2026-10-10: in-memory, on moq-net's poll transport traits, not a
`web_transport_trait` implementation, and nothing changes in
moq-dev/web-transport. Backend-only methods (`poll_acked`, `set_deadline`,
`set_limits`) answer "unsupported", as
[one adapter](/quest/m2/transport-adapter-dedup.md) decided. Optional latency
and loss run on the moq-time clock. It waits for the switch, which reshapes
moq-tokio's server types the seam abstracts over.

Find the smallest seam that keeps `moq_relay`'s public API narrow. Propose
where the session lives (recommended: a moq-net module behind a cargo
feature, since other crates' tests consume it) and its name in the PR.
Simulated UDP is [QUIC on the sim](/quest/m2/quic-sim.md).

Public API: the in-memory session and the relay seam. Wire: none.

## Required

- [moq-net on moq-time](/quest/m1/time/net.md) - the sim and instant the relay tests run on
- [Switch](/quest/m1/quic/fork/switch.md) - moq-tokio's server types settle first
