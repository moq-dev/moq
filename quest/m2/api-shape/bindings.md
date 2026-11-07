# [S] Bindings' small surface fixes

## Goal

- Go: a cancelled context on `Next`, `ReadFrame`, or `RecvGroup` cancels
  only that read, as in Python, Swift, Kotlin, Dart, and C++. Today
  `go/wrapper/subscribe.go` passes the consumer's `Cancel`, so a caller
  cannot bound one read and continue. Range-loop iterators still cancel the
  consumer when the loop exits. Check first that the Go generator fork
  supports a per-call cancel.
- JSON tracks have one constructor shape in moq-ffi and every wrapper: a
  producer the catalog advertises comes from the catalog producer, not from
  a free constructor that the caller must then register.
- Python: `moq.testing` leaves the published wheel; tests import it from the
  source tree.

## Plan

Found by the 2026-11-07 API shape audit (bindings); see the
[epic](/quest/m2/api-shape/README.md). The generated check that each
hand-written wrapper exports every FFI item is
[binding-export-coverage](/quest/m4/binding-export-coverage.md). moq-c's
renames are in [moq-dev/moq#8060](https://github.com/moq-dev/moq/pull/8060).
