# [S] BBR marking test replays its seed

## Goal

`bbr_marking_versus_dropping` (`rs/moq-quic/src/tests/mod.rs`) gives the same
verdict on every run of the same code. It seeds BBR's probe schedule, prints
the seed when it fails, and replays bit-exact from that seed. The packet
drop it can hit is explained and fixed at its cause.

## Plan

Found while landing [#5524](https://github.com/moq-dev/moq/pull/5524): on
unmodified `main`, a 20-run stress of this test failed once, at iteration
19, with one dropped packet on the marking run (`marking.dropped == 0`).
The test builds `Bbr3Config::default()`, so `probe_rng_seed` is unset and
`Bbr3::new` (`rs/moq-quic/src/congestion/bbr3/mod.rs`) draws the probe wait
from an unseeded RNG.

- ✅ Decided 2026-10-15 (maintainer): a CI flake, so it lives here in m1.
- Seed the test through `probe_rng_seed`, sweep enough seeds to find the
  failing schedules, and keep the seed in the failure message.
- Capture the probe schedule of a failing seed and decide whether the drop
  is a controller bug (BBR fails to answer CE in time) or the bottleneck
  model. Keep the acceptance thresholds and controller policy unchanged
  while gathering evidence; change either only with the evidence in hand.
- Check the sibling BBR tests for the same unseeded RNG.

Reopened 2026-10-16 (maintainer): [#5637](https://github.com/moq-dev/moq/pull/5637)
seeded the probe RNG (`probe_rng_seed`, kept) but made the test pass by
setting `deterministic_packet_numbers(true)`. That swaps the opt-ack
`PacketNumberFilter` for `PacketNumberFilter::disabled()`
(`rs/moq-quic/src/connection/mod.rs`), which production never does, then
asserts `marking.dropped == 0` for one hand-picked seed. Its own sweep shows
seed `[0; 16]` with production-style packet-number skips still drops one
packet on some runs, so the drop is real and still unexplained.

- Turn packet-number skipping back on in `bbr_marking_versus_dropping`. To
  replay, seed the endpoints (`EndpointConfig::rng_seed`, which feeds the
  connection RNG behind `PacketNumberFilter::new`) instead of disabling the
  filter.
- Explain the one-packet tail drop with skipping on. #5637 blamed skipped
  packet numbers changing the encoded width; that was never shown. A skip
  more plausibly shifts ACK timing (an ACK range gap, a delayed ACK) and
  with it BBR's view of delivery rate or CE. Capture both runs and compare.
- `bbr_burst_after_long_idle_keeps_learned_bandwidth` got a probe seed in
  #5637 but still uses random packet numbers, so it is not replayable as
  that PR claimed. Make it bit-exact the same way.

## Related

- [Seeded sim](/quest/m2/sim-seed.md) - the same replay rule for the moq-clock sim
