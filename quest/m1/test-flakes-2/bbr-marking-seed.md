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

## Related

- [Seeded sim](/quest/m2/sim-seed.md) - the same replay rule for the moq-clock sim
