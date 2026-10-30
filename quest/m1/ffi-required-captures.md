# [S] Binding announcements always carry captures

## Goal

In moq-ffi and every binding generated from it (C, Dart, Go, Python,
Kotlin, Swift), an announcement's `captures` is a required list rather than
an optional one. Rust's `moq-net` and JS keep `captures` optional, because
a covering route really can have none. Starts once
[moq-dev/moq#7266](https://github.com/moq-dev/moq/pull/7266) merges.

## Plan

Decided 2026-10-30 (moq.pro maintainer, planning moq.pro's 2026-10-29 merge
run follow-ups): #7266 always sets `captures` on the exact-match
announcements the bindings receive, but keeps the type `Option` and changes
only doc comments. Make the binding type say so. Redesigning the Rust and JS
types to tell exact and covering events apart was rejected for now.

- FFI: `MoqAnnounce.captures` in `rs/moq-ffi/src/origin.rs` and the C
  binding's `rs/moq-c/src/origin.rs` become non-optional. Regenerate and
  commit `dart/moq_ffi/lib/src/moq.dart`. `go/wrapper/types.go` follows the
  FFI type.
- Remove the `?? []` and None checks this makes dead: the Swift README,
  `doc/lib/py/index.md`, and the binding tests.
- This is a breaking binding change, so it rides the next cut with #7266.
  Update the binding docs in this PR.
- Downstream, moq.pro's agent tests pass `captures=None`
  (`agent/tests/test_runtime.py`, `agent/tests/test_supervisor.py`). They
  change with the pin bump that brings this in.
