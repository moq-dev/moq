# [S] moq-clock's browser backend runs in CI

## Goal

A `wasm-bindgen-test` suite exercises `moq-clock`'s browser backend (wasmtimer
over `performance.now()`, `setTimeout`, and `Date.now()`) in headless Chromium,
and a PR that touches `rs/moq-clock` fails CI when it breaks. Today the backend
is only compiled for wasm32 (`just rs wasm`) and never run.

## Plan

Decided 2026-10-13 by the maintainer while reviewing #5465:

- Run in a browser (`wasm_bindgen_test_configure!(run_in_browser)`), not
  Node, because the browser is the backend's real environment. Reuse the
  Playwright Chromium that `wasm.yml` already installs, and trigger the job
  on `rs/moq-clock/**`.
- Cover what the host tests cover on tokio: `now()` moves forward, `sleep()`
  and `sleep_until()` fire no earlier than their deadline (wall-clock lower
  bounds only, as for any real-time test), `timeout()` elapses, an
  `interval()`'s first tick is immediate, and `wall()` reads a set clock.
- Run it through one `just` recipe, so a local run and CI match.
  `rs/moq-net/tests/wasm.rs` documents the runner invocation
  (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner`).

Public API: none. Wire: none.

## Related

- [@moq/clock](/quest/m1/time/js.md) - the JS mirror, which must share this
  backend's origin before an instant crosses between them
