# [S] Relay bitrate cap tests run on the QUIC simulator

## Goal

The tokio half of `rs/moq-relay/tests/integration/bitrate_caps.rs` runs on
`quic::driver::sim` and asserts the capped transfer's time within a window in
simulated time, so a regression that throttles far below the cap fails, and
the tests stop waiting on the real clock.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
The tests assert only `elapsed >= 700ms` and poll every 10 ms on real time,
because [moq-dev/moq#7535](https://github.com/moq-dev/moq/pull/7535) chose
real sockets while no QUIC simulator existed; one landed in
[moq-dev/moq#8709](https://github.com/moq-dev/moq/pull/8709)
(`rs/moq-relay/tests/integration/quic_sim.rs`). The io_uring half stays on
real sockets.

Public API: none. Wire: none.
