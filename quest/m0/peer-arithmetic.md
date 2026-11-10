# [M] @moq/clock's Instant has no throwing Duration arithmetic, mirroring kio

## Goal

`@moq/clock` mirrors `kio::clock::Instant`, which no longer has `+`/`-` with `Duration` ([moq-dev/moq#8220](https://github.com/moq-dev/moq/pull/8220)): JS `Instant` drops `add` and `sub` in favor of `checkedAdd`/`checkedSub` and `saturatingAdd`/`saturatingSub`, and `Duration` gains `checkedAdd` and `saturatingAdd` beside its existing `saturatingSub`. Each call site states what overflow means, as the Rust sites do.

## Plan

`@moq/clock` mirrors kio (decided 2026-11-07, maintainer, /quest-plan from the API sweep). The Rust half and the peer-reachable fixes landed in [moq-dev/moq#8220](https://github.com/moq-dev/moq/pull/8220); about 160 non-test JS call sites remain. Today `Instant.add` throws a `RangeError` past the clock's range instead of panicking, so this is API parity rather than a crash a peer can reach.
