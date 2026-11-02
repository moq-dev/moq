# [S] The relay fits a Windows stack under every feature set

## Goal

`stack::debug_relay_load_and_run_fit_windows_stack`
(`rs/moq-relay/tests/integration/stack.rs`) passes under
`cargo nextest --all-features` as well as in `just ci`. The relay's
`Relay::load` and `Relay::run` futures fit a 1 MiB thread stack under every
feature set the test builds with.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning). Seen while iterating
[moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211): on main
(da0a9e1d8a) the test overflows its stack under `--all-features`, and passes
with the gate's feature set.

- Find which feature makes the future grow past the stack, for example by
  bisecting features and checking future sizes with `-Zprint-type-sizes` or
  `std::mem::size_of_val`.
- Shrink it at its source, for example by boxing a large state or moving a
  large local out of the async body.
- More evidence (2026-11-01): `Relay::run_inner`'s polling function alone
  takes about 431 KB of stack
  ([moq-dev/moq#7535](https://github.com/moq-dev/moq/pull/7535)). Debug startup
  on main is within about 32 KiB of 1 MiB with the gate's features, and
  `--features io-uring,stun` overflows too
  ([moq-dev/moq#7363](https://github.com/moq-dev/moq/pull/7363)). Shrinking that
  frame likely fixes every case.
- Don't raise the stack size, and don't narrow the test's features to hide
  the overflow: the test exists because Windows' main thread really has 1 MiB.
