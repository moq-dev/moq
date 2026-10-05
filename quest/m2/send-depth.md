# [M] moq-net futures prove Send without a raised recursion limit

## Goal

A downstream crate that requires `Send` on a future awaiting
`announce::Consumer::next()`, or a track or group read, compiles without
nightly's `recursion_depth_exceeding_limit` future-incompat warning and
without raising its own `recursion_limit`.

## Plan

moq-net itself needs `#![recursion_limit = "256"]` (#4746), which downstream
crates do not inherit. Each `kio::Shared` adds about five levels to the
auto-trait chain (Shared, Lock, Arc, Mutex, State), nested from origin to
broadcast to track to group. The recursive `RouteNode` is a cycle the
compiler closes, so flattening the route tree likely does not help.

Decided (2026-10-04), in order:

1. Measure: an integration test crate in `rs/moq-net/tests`, compiled at the
   default limit, proves these futures are `Send`, and runs on nightly in CI
   so the lint shows up. Settle whether it runs in `nightly.yml`.
2. Shorten every chain in kio with `unsafe impl<T: Send> Send for Lock<T>`,
   the same condition the compiler derives.
3. If that is not enough, a zero-cost `SendCell<T>` whose only constructor
   requires the bound, gated for wasm.

Done when the `recursion_limit` line is deleted. Revisit the milestone if the
lint becomes a hard error on stable.

## Closes

- [#4648](https://github.com/moq-dev/moq/issues/4648) - close this issue when the quest finishes
