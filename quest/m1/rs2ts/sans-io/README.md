# Sans-IO moq-net

## Goal

moq-net builds and runs with no async runtime: bytes and timestamps go in,
events and bytes come out. The async helper methods sit behind an `async`
cargo feature, and a CI lane builds and tests the crate without it.

## Plan

Decided in planning: moq-net itself is the core, not a second crate. JS
reimplements the async helpers natively with Promises, so the translator
reads the crate without the `async` feature. Split by layer so each lands on
`dev` independently.

The line has no work of its own beyond its children.

## Quests

- [Sans-IO lite session](/quest/m1/rs2ts/sans-io/lite.md) - the lite session is driven by bytes, stream events, and `tick(now)`
- [Sans-IO model](/quest/m1/rs2ts/sans-io/model.md) - origin, broadcast, track, and group handles run without a runtime, with time supplied by the caller
- [The async feature](/quest/m1/rs2ts/sans-io/async-feature.md) - the async helpers sit behind an `async` feature and a CI lane builds and tests moq-net without it
- [Sans-IO IETF session](/quest/m1/rs2ts/sans-io/ietf.md) - the moq-transport session is driven the same way as lite
