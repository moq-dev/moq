# [XS] Seeded BBR3 in tests

## Goal

Every moq-congestion test that builds `Bbr3Config::default()` without a seed
replays the same randomness on every run, as it did before
[moq-dev/moq#6602](https://github.com/moq-dev/moq/pull/6602).

## Plan

- `Bbr3::new` falls back to `Pcg32::from_rng(&mut rand::rng())`
  (`rs/moq-congestion/src/bbr3/mod.rs`, about line 620); before the extraction
  it used `crate::entropy::rng()`, which tests seeded per thread.
- Under `cfg(test)`, fall back to a fixed seed (moq-quic already reseeds its
  own tests in `rs/moq-quic/src/config/transport.rs`). Production keeps OS
  randomness.
