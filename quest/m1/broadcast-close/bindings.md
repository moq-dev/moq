# [M] Binding close

## Goal

Every binding ends a broadcast with `close()`, mirroring Rust, and its
`finish` is deprecated.

## Plan

- moq-ffi: add `MoqBroadcastProducer::close()` next to `finish()` in
  `rs/moq-ffi/src/producer.rs`. It closes the broadcast, then the catalog, as
  `finish` does, so a catalog error cannot stop the broadcast from ending.
  Deprecate `finish`.
- libmoq: export `moq_publish_close` and deprecate `moq_publish_finish`. Move
  `cpp/obs/src/moq-output.cpp` and the C tests over.
- Wrappers: `py/moq-rs` `BroadcastProducer.close()` (its `finish` docstring
  wrongly says it closes the tracks), swift `Broadcast.close()`, and the Go
  `Publish.Close()`. Kotlin and Dart only alias the generated type, so they
  pick `close` up from moq-ffi.
- Move the binding tests over, keeping one test per binding that a second
  `close` errors or no-ops, whichever Rust settles on.
- Update `doc/lib/{py,swift,kt,go,dart,c}`, including `doc/lib/go/index.md`'s
  `broadcast.Finish()` sample.
- Fix the moq-ffi `origin.rs` doc comment that points users at a
  `broadcast.closed()` the bindings don't have.

## Required

- [Rust close](/quest/m1/broadcast-close/rust.md) - the binding forwards to `broadcast::Producer::close`
