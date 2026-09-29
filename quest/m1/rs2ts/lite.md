# [XL] Generated lite

## Goal

@moq/net's lite session and model layer are generated from moq-net by rs2ts,
and the hand-written TypeScript they replace is deleted. Hand-written
TypeScript remains only for the transport pumps, timers, and the Promise
helpers over the poll API. The translated moq-net tests and
`just test interop --all` pass, and bundle size, per-frame CPU, and
first-frame latency are no worse than the hand-written js/net.

## Plan

- The `@moq/net` API may change where the generated shape is no worse to
  use: disposable handles (`using`), `U64` for sequences and ids. Update
  watch, publish, hang, room, and the demos in the same change, and the
  `doc/` pages for anything user-facing.
- A forgotten `drop()` leaves a track open forever: add a debug-only
  `FinalizationRegistry` that reports handles collected without one, and
  runtime guards against double drop and use after drop.
- Size budget: js/net's `lite/*` is 15 KB gzip today; keep generated output
  near it. Watch for std shims and fmt/tracing pulling in weight.

Public API: breaks `@moq/net`; retargets to `dev`. Wire: none.

## Required

- [rs2ts](/quest/m1/rs2ts/translator.md) - the translator
- [Sans-IO lite session](/quest/m1/rs2ts/sans-io/lite.md) - the session shape it translates
- [Sans-IO model](/quest/m1/rs2ts/sans-io/model.md) - the model shape it translates
- [The async feature](/quest/m1/rs2ts/sans-io/async-feature.md) - rs2ts reads moq-net without it
- [Mock-clock tests](/quest/m1/rs2ts/mock-clock.md) - the tests that prove parity
