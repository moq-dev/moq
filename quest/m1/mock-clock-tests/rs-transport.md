# [M] Transport crate tests run on a mocked clock

## Goal

The transport crates' tests that wait on real time move to a paused or
simulated clock, or an awaited event. See the
[epic](/quest/m1/mock-clock-tests/README.md) for the rules.

## Plan

As of 2026-11-06:

- moq-tokio, 24: `tests/integration/broadcast.rs` 9, `src/mdns.rs` 4,
  `tests/integration/reconnect.rs` 3, `src/quic/driver/tests.rs` 2,
  `tests/integration/subscription_end_integrity.rs` 2, and one each in
  `src/jemalloc.rs`, `src/server.rs`, `src/tcp.rs`, and
  `tests/integration/backend.rs`.
- moq-quic, 6: `src/connection/mtud.rs` 4 and
  `src/tests/characterize/endpoint.rs` 2.
- moq-sock, 4: `tests/udp/main.rs` 3 and `src/shard.rs`.
- moq-uring, 3: `src/worker.rs` 2 and
  `tests/integration/socket_queues.rs`.

Public API: none. Wire: none.
