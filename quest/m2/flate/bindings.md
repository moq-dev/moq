# [M] Flate bindings

## Goal

The hand-written wrappers (Python, Swift, Kotlin, Go, Dart) expose flate
tracks, the snapshot and stream opaque tracks moq-ffi already generates, in
their own idiom beside the JSON entry. A track published from a wrapper
decodes in the browser with `@moq/flate` and vice versa.

## Plan

moq-ffi publishes opaque tracks today (`MoqFlateSnapshotProducer` and
`MoqFlateStreamProducer`, constructed from a broadcast and a track). Only the generated bindings reach them; no wrapper does. This quest binds the existing
track modes, not the bare codec: a `frame()` call across the FFI boundary
invites the window desync the track modes exist to prevent.

- moq-ffi has no consume side for these tracks. Add it next to the JSON
  consumers so each wrapper can read what it writes.
- Wrappers per the Cross-Package Sync table: each gains a `flate` namespace
  beside its `json` one (`py/moq-rs/moq/json.py`, `go/wrapper/json/`,
  `swift/Sources/Moq/Json.swift`, `kt/moq`'s `dev.moq.json`, and
  `dart/moq/lib/json.dart`), per `rs/moq-ffi/AGENTS.md`.
- Document in `doc/lib/{py,swift,kt,go,dart}` beside the JSON entry.
- Tests: a round trip in each wrapper that has tests, and one cross-language
  check that a wrapper-published group decodes with `@moq/flate`. Run
  `just test interop --all`.

Public API: additive on moq-ffi and every wrapper. Wire: none.
