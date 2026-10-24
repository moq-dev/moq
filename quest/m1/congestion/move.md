# [M] Move the controllers

## Goal

`rs/moq-congestion` holds `moq-quic`'s congestion controllers (BBR3, Cubic,
Unlimited) and the `Controller`/`ControllerFactory` traits, and `moq-quic`,
`moq-tokio`, and `moq-uring` use them from there. No behavior changes: the
same tests and benchmarks pass, now in the new crate where they test a
controller alone.

## Plan

A mechanical move, kept separate from [the redesign](/quest/m1/congestion/trait.md)
so git sees renames and review sees no logic change.

- Name decided 2026-10-23: `moq-congestion`. It reads as a QUIC transport's
  dependency, not as a `moq-quic` internal.
- Time is `moq_clock::Instant` (decided 2026-10-23: reuse the existing
  light clock type; other transports convert at the boundary).
- The trait still references `RttEstimator` and `SpaceId`, which live in
  `moq-quic`. Move or mirror the minimum the controllers read, and leave the
  real decoupling to the redesign.
- Transport-level tests (`moq-quic/src/tests/characterize`, the sim) stay in
  `moq-quic`; controller unit tests and the cleanup benchmark move.
- Update `rs/moq-quic/README.md`, `doc/setup/repository.md`, and any quest
  text that names `moq-quic`'s `congestion` module.
