# [L] Mock-clock tests

## Goal

moq-net's tests run on the sans-IO clock instead of tokio, so they need no
runtime and rs2ts translates them alongside the code. The generated
TypeScript runs the same tests under bun.

## Plan

tokio is in moq-net's tests only for paused, advanceable time
(`start_paused`, `advance`): 591 `tokio::test`s on dev, 86 of them paused.
Drive them by polling the drivers with supplied instants, the seam the
model already exposes, and a small synchronous executor instead.

Guidance:

- Port mechanically where possible; keep each test's assertions unchanged.
- Retire the test-only time hooks in production code as their tests move:
  `time::Clock::tokio` and the `model::clock` pool registry.
- Tests that exercise the `async` helpers stay behind that feature and are
  not translated.

Public API: none. Wire: none.
