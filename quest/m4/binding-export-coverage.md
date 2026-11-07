# [S] Hand-written wrappers reach every FFI item

## Goal

The Python, Go, and Swift wrappers each fail a test when a moq-ffi record,
enum, or handle has no wrapper path, as Kotlin's smoke test already does
(`SmokeTest.kt`). Today these three hand-wrap 3,000 to 5,000 lines each,
and parity depends on review.

## Plan

Found by the 2026-11-07 API shape audit (bindings). Generate the list of
FFI items from UniFFI's metadata once, and have each wrapper's test check
it against the wrapper's public types, with an explicit allowlist for items
a wrapper deliberately hides. Kotlin keeps its own check or moves to the
shared list. When [the exporter](/quest/m3/ffi-exporter.md) replaces UniFFI,
the list comes from it.
