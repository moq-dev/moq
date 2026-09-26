# [M] Bindings stamp data frames with a capture time

## Goal

A moq-ffi publisher, and every wrapper over it (Python, Swift, Kotlin, Go,
Dart), can pass a capture time with a JSON or binary snapshot `update` or
stream `append`, so its data tracks advertise `delay` and `jitter` like a Rust
publisher's. Leaving it out keeps today's behaviour. `moq-json`'s `window`
producer takes a capture time too. libmoq is out of scope.

## Plan

- #4270 gives the Rust producers `moq_net::Timed<P, T>`, built with
  `Timed::from(value).at(t)`. The `moq-mux` data producers take
  `Timed<_, Instant>`, map it onto the broadcast clock, and refuse one ahead of
  now (`Error::InvalidCapture`). The moq-ffi producers in
  `rs/moq-ffi/src/{json,binary}.rs` pass bare values.
- An `Instant` cannot cross the FFI. Pick a form every language can produce
  without a shared epoch. Recommendation: an optional age (how long ago the
  payload was captured), turned into `Instant::now() - age` inside moq-ffi. A
  raw timestamp is the unmapped clock the Rust type exists to refuse. Make it
  optional on the existing methods, never a `_with_capture` twin.
- `moq-json` window: `window::Producer::push` stamps `Timestamp::now()`
  (`rs/moq-json/src/window/producer.rs`). Accept `Timed` as the snapshot and
  stream producers do. Nothing in `moq-mux` publishes window mode, so there is
  no estimator to feed.
- Wrappers follow per the cross-package sync table, each with a test that a
  past capture time is accepted and a future one refused. Update
  `doc/lib/{py,swift,kt,go,dart}`.

Public API: additive optional capture time on moq-ffi's data producers and
every wrapper; `window::Producer::push` accepts `Timed`, source-compatible.
Wire: none.

## Required

- [Data jitter](/quest/m1/data-jitter.md) - `Timed` and the mux capture path (#4270)

## Related

- [FFI shape](/quest/m1/ffi-shape/README.md) - moves the data producers into a json namespace
