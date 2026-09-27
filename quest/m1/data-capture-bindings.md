# [M] Bindings stamp data frames with a capture time

## Goal

A moq-ffi publisher, and every wrapper over it (Python, Swift, Kotlin, Go,
Dart), can pass a capture time with a JSON or binary snapshot `update` or
stream `append`, so its data tracks advertise `delay` and `jitter` like a Rust
publisher's. Leaving it out keeps today's behaviour. `moq-json`'s `window`
producer takes a capture time too. Settled scope: moq-ffi and its wrappers,
not libmoq.

## Plan

- #4270 gives the Rust producers `moq_net::Timed<P, T>`, built with
  `Timed::from(value).at(t)`. The `moq-mux` data producers take
  `Timed<_, Instant>`, map it onto the broadcast clock, and refuse one ahead of
  now (`Error::InvalidCapture`). The moq-ffi producers in
  `rs/moq-ffi/src/{json,binary}.rs` pass bare values.
- Settled: the capture time is a media timestamp on the broadcast's
  timeline. moq-ffi exposes the broadcast clock's `now()` as a timestamp;
  callers stamp payloads with values taken from it, and moq refuses one ahead
  of now. That keeps a device or process clock out, as the Rust `Instant`
  mapping does. The `moq-mux` producers take an `Instant` today, so either
  map the timestamp back through the clock inside moq-ffi or give the clock a
  typed timestamp moq-mux accepts; keep a raw `Timestamp` from compiling
  there. Make it optional on the existing methods, never a `_with_capture`
  twin.
- `moq-json` window: `window::Producer::push` stamps `Timestamp::now()`
  (`rs/moq-json/src/window/producer.rs`). Accept `Timed` as the snapshot and
  stream producers do. Nothing in `moq-mux` publishes window mode, so there is
  no estimator to feed.
- Wrappers follow per the cross-package sync table, each with a test that a
  past capture time is accepted and a future one refused. Update
  `doc/lib/{py,swift,kt,go,dart}`.

Public API: breaking, so it lands on `dev`. A new parameter on the generated
`update` and `append` breaks every published binding caller (Go, for one, has
no optional arguments), and a `_with_x` twin is ruled out. The broadcast clock
`now()` is additive; `window::Producer::push` accepts `Timed`, source-compatible.
Wire: none.

## Required

- [Data jitter](/quest/m1/data-jitter.md) - `Timed` and the mux capture path (#4270)

## Related

- [FFI shape](/quest/m1/ffi-shape/README.md) - moves the data producers into a json namespace
- [Generated C bindings](/quest/m1/c/README.md) - replaces libmoq, so C inherits this from moq-ffi
