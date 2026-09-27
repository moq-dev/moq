# [L] Mock-clock tests

## Goal

moq-net's tests run on the sans-IO clock instead of tokio, so they need no
runtime and rs2ts translates them alongside the code. The generated
TypeScript runs the same tests under bun.

## Plan

tokio is in moq-net's tests only for paused, advanceable time
(`start_paused`, `advance`): 591 `tokio::test`s on dev, 86 of them paused.
Drive them from the model's injectable clock and a small synchronous
executor instead.

Guidance:

- Port mechanically where possible; keep each test's assertions unchanged.
- Tests that exercise the `async` helpers stay behind that feature and are
  not translated.

Public API: none. Wire: none.

## Required

- [Sans-IO model](/quest/m1/rs2ts/sans-io/model.md) - supplies the clock seam
