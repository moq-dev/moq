# [M] Fuzz moq-quic

## Goal

`rs/moq-quic/fuzz` is a cargo-fuzz crate laid out like `rs/moq-net/fuzz`,
run nightly for 300 s per target, with every crash minimized into a
regression replayed by `just check` on stable.

Targets:

- Upstream quinn's `packet` (`PartialDecode`), `params`
  (`TransportParameters::read`), `streamid`, and `streams` (an op sequence
  against `StreamsState`), ported from its `fuzz/fuzz_targets/`.
- Frame decode through `frame::Iter`.
- A connection op sequence over a connected `Pair`: opens, writes, finishes,
  resets, stops, runtime limit changes, datagrams, and arbitrary inbound
  packets, checking that nothing panics and that delivered bytes match what
  was written.

## Plan

Decided 2026-10-16 to mirror moq-net: one-line target shims call a hidden
`moq_quic::fuzz` module behind a `fuzz` feature (and `cfg(test)`), seeds
come from an example, and `regressions/<target>/` replays in the normal test
run. This replaces today's `cfg(fuzzing)` `pub mod fuzzing` and the implicit
`arbitrary` feature. The fuzz crate joins `no_fuzz` exclusions, the
`just rs fuzz` recipe, and the nightly `fuzz` matrix.
