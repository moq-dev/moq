# [XS] Incoming is not a future

## Goal

moq-tokio's `Incoming` no longer implements `IntoFuture`, so its public API
names no `futures` type. A server accepts with `incoming.accept()?.await`,
which awaits the named `Connecting` future.

## Plan

Decided 2026-10-27 (maintainer), from
[moq-dev/moq#6841](https://github.com/moq-dev/moq/pull/6841): delete the impl
rather than name a new future or fold an immediate refusal into
`Connecting`. The impl came from quinn and glues `accept()`'s two outcomes
with `futures::future::Either` (`rs/moq-tokio/src/quic/driver/incoming.rs`).
The only direct `.await` on an `Incoming` is one test
(`rs/moq-tokio/tests/integration/receive_credit.rs`); every other caller
already calls `accept()`.

- Delete `impl IntoFuture for Incoming` and the `Either`/`Ready` imports.
- Rewrite the test to `accept()?.await`.
- Check moq-uring's driver for the same impl and drop it too if present.
- Fix any docs or examples that await an `Incoming` directly.

Public API: breaking for moq-tokio (`Incoming` is no longer `IntoFuture`);
migrate with `incoming.accept()?.await`. Wire: none.
