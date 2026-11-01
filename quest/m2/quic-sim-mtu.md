# [S] moq-quic's sim pair fails a test that sends an oversized packet

## Goal

A moq-quic test whose endpoint sends a datagram larger than the simulated
path MTU fails, instead of the `Pair` silently dropping it. Tests that
mean to lose oversized packets (PMTU probes, a blackholed path) declare it
and still pass.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7334](https://github.com/moq-dev/moq/pull/7334).
Its online review found an ACK_FREQUENCY frame pushing a timestamped ACK
past the MTU; [moq-dev/moq#7334](https://github.com/moq-dev/moq/pull/7334) fixed it with a debug assert in `poll_transmit`, but the
sim would have hidden it: `Pair::drive_client` and `drive_server`
(`rs/moq-quic/src/tests/util.rs:161-215`) drop any packet over
`self.mtu` with only an `info!` log.

- Count oversized drops per side in `Pair`, split into PMTU probes
  (moq-quic's DPLPMTUD in `rs/moq-quic/src/connection/mtud.rs`) and
  everything else.
- Fail on a non-probe oversized packet by default. Tests that shrink the
  path on purpose opt out explicitly: `pair.mtu = 0` in
  `tests/characterize/ack_frequency.rs:144`, and the lowered `PATH_MTU`
  or 1000/1200-byte cases in `tests/mod.rs` (around lines 3146, 3195,
  4291-4323). Check each still means what it says once the drop is visible.
- Run `just rs quic-sim` too, since the sim acceptance suites share
  `Pair`.

Not here: `tests::probe::competing_cubic`, which was failing on main, was
fixed by [moq-dev/moq#7429](https://github.com/moq-dev/moq/pull/7429), and the 0-RTT ACK
report is moot ([0-RTT leftovers](/quest/m1/quic-zero-rtt-leftovers.md)).

Public API: none (test harness only). Wire: none.

## Related

- [QUIC on the sim](/quest/m2/quic-sim.md) - reuses this `Pair` for MoQ sessions
