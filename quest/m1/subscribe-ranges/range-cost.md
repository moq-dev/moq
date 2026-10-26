# [M] A subscription's range count cannot multiply per-group cost

## Goal

The model's cost per group no longer grows with the number of ranges in a
subscription, or that number is bounded by the design (flow control or an
owned budget), not by a refusal constant. A benchmark sweeps range count and
subscriber count, so a slope shows up.

## Plan

From [moq-dev/moq#6715](https://github.com/moq-dev/moq/pull/6715): lite-07
SUBSCRIBE carries a list of ranges, a peer can send as many as fit in one
message, and the model's per-group cost grows with that count. A hostile peer
can multiply the CPU a relay spends on every group of a track.

- Measure first: extend `rs/moq-net/benches/track.rs` with ranges × subscribers.
- Prefer merging overlapping or adjacent ranges at decode, and a lookup that
  touches only the ranges around a group.
- Include `track::Subscriber::covered`, which the lite-07 publisher runs on
  each idle wake of a bounded subscription and walks every range.
- Shares code with [Linear fill path](/quest/m1/subscribe-ranges/fill-linear.md).

Public API: none expected. Wire: none expected.
