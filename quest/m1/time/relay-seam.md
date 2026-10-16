# [L] The relay seam

## Goal

`moq_relay::Connection` runs over any moq-net transport session, not only a
`moq_tokio::server::Request`, so relay integration tests run on the sim without
sockets and the relay session benchmark can reuse the same seam.

## Plan

The in-memory poll transport landed in #5530 as
`rs/moq-net/src/transport/memory.rs`. The moq-net test mock wraps it and the
e2ee harness uses it. Reuse that pair; do not recreate the transport or its
removed duplicate mocks. moq-tokio's `FakeSession` and `Scripted` still test
the web-transport adapter and are not replaced by a poll transport.

Find the smallest remaining relay connection seam and keep its public API
narrow. Move auth, cluster-origin, and stats integration tests onto the sim
where they do not require real I/O. It abstracts over moq-tokio's server
types as the switch to `moq-quic` (#5664) left them. Simulated UDP belongs to
QUIC on the sim.

Public API: the remaining relay seam. Wire: none.

## Required

- [moq-net on moq-clock](/quest/m1/time/net.md) - the sim and instant the relay tests run on
