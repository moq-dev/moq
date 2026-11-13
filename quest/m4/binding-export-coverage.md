# [S] Hand-written wrappers reach every FFI item

## Goal

The Python, Go, Swift, and Kotlin wrappers each fail a test when a moq-ffi
record, enum, or handle has no wrapper path. Today Python, Go, and Swift
hand-wrap 3,000 to 5,000 lines each, and Kotlin's `all public ffi records and
handles have aliases` test (`SmokeTest.kt`) constructs a fixed list, so a new
record without an alias still passes. Parity depends on review.

## Plan

Found by the 2026-11-07 API shape audit (bindings). Generate the list of
FFI items from UniFFI's metadata once, and have each wrapper's test check
it against the wrapper's public types, with an explicit allowlist for items
a wrapper deliberately hides. Kotlin's
fixed list moves to the shared one. When [the exporter](/quest/m3/ffi-exporter.md) replaces UniFFI,
the list comes from it.

## Waiting

- Maintainer: when to build the FFI item list: from UniFFI's metadata now,
  rebuilt once [the exporter](/quest/m3/ffi-exporter.md) lands, or only from
  the exporter, by making that quest Required here. Recommended: only from
  the exporter, since its switch keeps every wrapper's API and the UniFFI
  version would be written twice.
