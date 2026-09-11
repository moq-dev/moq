# [M] Flate bindings

## Goal

moq-ffi and libmoq publish and subscribe a compressed track of opaque frames,
and every wrapper (Python, Swift, Kotlin, Go, Dart, C) reaches it. A track
written from C decodes in the browser with `@moq/flate` and vice versa.

## Plan

Bind the track wrapper, not the codec: the bindings' job is to make the group
discipline unrepresentable to misuse, and a bare `frame()` call across an FFI
boundary invites the desync the wrapper exists to prevent.

moq-ffi, next to `json.rs` and named the same way:

- `MoqBroadcastProducer::publish_flate(name, MoqFlateConfig) -> MoqFlateProducer`
  with `append_group() -> MoqFlateGroupProducer`, `finish()`, `abort(code)`.
- `MoqFlateGroupProducer::write_frame(Vec<u8>)`, `finish()`, `abort(code)`.
- `MoqBroadcastConsumer::subscribe_flate(name, MoqFlateConfig) -> MoqFlateConsumer`
  with `next_group() -> Option<MoqFlateGroupConsumer>`, `cancel()`.
- `MoqFlateGroupConsumer::read_frame() -> Option<Vec<u8>>`, `cancel()`.
- `MoqFlateConfig { level = 6, max_frame_size = 64 MiB }` as `#[uniffi(default)]`
  literals, with the same drift test `json.rs` keeps against the crate defaults.

Payloads cross as bytes, not `MoqFrame`: the wrapper carries no timestamp.
Explicit groups rather than a flat `append(bytes)` because the window resets
at the boundary and the caller chooses where that is; a helper that rolls
groups on a size or count budget can follow if a consumer asks.

libmoq mirrors `moq_publish_json_*` and `moq_consume_json_*`:
`moq_publish_flate`, `moq_publish_flate_group`, `moq_publish_flate_frame`,
`moq_publish_flate_group_finish`, `moq_publish_flate_finish`,
`moq_consume_flate`, `moq_consume_flate_group`, `moq_consume_flate_frame`,
`moq_consume_flate_frame_free`, `moq_consume_flate_close`, with a
`moq_flate_config` struct. Follow the terminal-status callback contract for
the consume side and regenerate `moq.h`.

Wrappers per the Cross-Package Sync table: the uniffi bindings regenerate;
`go/wrapper/moq/json.go`, `py/moq-rs/moq/{publish,subscribe}.py`,
`swift/Sources/Moq/Json.swift`, and `dart/moq` each gain a hand-written
sibling. Document in `doc/lib/{c,py,swift,kt,go,dart}` beside the JSON entry.

Tests: a moq-ffi round trip next to `json_snapshot_roundtrip`, a libmoq C
round trip in `src/test.rs`, and one cross-language check that a C-published
group decodes with the shared vector from the track quest. Run
`just test smoke-full`.

Public API impact: additive on moq-ffi, libmoq, and every wrapper; `main`.
Wire impact: none.

## Required

- [Track wrapper](/quest/m2/flate/track.md) - the surface being bound
