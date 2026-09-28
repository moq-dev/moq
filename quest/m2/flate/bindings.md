# [M] Flate bindings

## Goal

The hand-written wrappers (Python, Swift, Kotlin, Go, Dart) expose flate
tracks, the snapshot and stream opaque tracks moq-ffi already generates, in
their own idiom beside the JSON entry. A track published from a wrapper
decodes in the browser with `@moq/flate` and vice versa.

## Plan

moq-ffi publishes opaque tracks today (`publish_binary_snapshot` and
`publish_binary_stream`, #4137), renamed after `flate` by
[moq-binary folds into moq-flate](/quest/m1/flate-binary.md). Only the
generated bindings reach them; no wrapper does. This quest binds the existing
track modes, not the bare codec: a `frame()` call across the FFI boundary
invites the window desync the track modes exist to prevent.

- moq-ffi has no consume side for these tracks. Add it next to the JSON
  consumers so each wrapper can read what it writes.
- Wrappers per the Cross-Package Sync table: `go/wrapper/json.go`,
  `py/moq-rs/moq/{publish,subscribe}.py`, `swift/Sources/Moq/Json.swift`,
  `kt/moq`'s `Json.kt` with its `Aliases.kt` re-exports, and
  `dart/moq/lib/src/aliases.dart` each gain a flate sibling. If
  [FFI shape](/quest/m1/ffi-shape/README.md) has landed, follow its `flate`
  namespace instead.
- Document in `doc/lib/{py,swift,kt,go,dart}` beside the JSON entry.
- Tests: a round trip in each wrapper that has tests, and one cross-language
  check that a wrapper-published group decodes with `@moq/flate`. Run
  `just test interop --all`.

Public API: additive on moq-ffi and every wrapper. Wire: none.

## Required

- [moq-binary folds into moq-flate](/quest/m1/flate-binary.md) - the flate snapshot and stream tracks and their moq-ffi names
