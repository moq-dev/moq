# [S] Binding announcements always carry captures

## Goal

In moq-ffi and every binding generated from it (Dart, Go, Python, Kotlin,
Swift), and in the hand-written C binding, an announcement's captures are
always present: a required list rather than an optional one. Rust's
`moq-net` and JS keep `captures` optional, because a covering route really
can have none. Builds on [moq-dev/moq#7266](https://github.com/moq-dev/moq/pull/7266),
merged 2026-10-30.

## Plan

Decided 2026-10-30 (moq.pro maintainer, planning moq.pro's 2026-10-29 merge
run follow-ups): #7266 always sets `captures` on the exact-match
announcements the bindings receive, but keeps the type `Option` and changes
only doc comments. Make the binding types say so. Redesigning the Rust and
JS types to tell exact and covering events apart was rejected for now.

- moq-ffi: `MoqAnnounce.captures` in `rs/moq-ffi/src/origin.rs` becomes
  non-optional. Regenerate and commit `dart/moq_ffi/lib/src/moq.dart`.
  `go/wrapper/types.go` follows the FFI type.
- moq-c wraps `moq-net` directly, not moq-ffi. Drop `has_captures` from its
  public struct (`rs/moq-c/src/api.rs`, whose doc comment still says false
  means the route overlaps) and from the generated header. Drop its
  assignment in `Origin::announced_info` (`rs/moq-c/src/origin.rs`) and
  update `rs/moq-c/src/test.rs`. Including C was a planning call, since the
  decision named "FFI and generated bindings"; the PR confirms it.
- Remove the nil checks this makes dead: `swift/README.md` (`captures ?? []`)
  and `go/wrapper/moq_test.go`.
- This is a breaking binding change, so it rides the next cut with #7266.
  Update the binding docs in this PR.
- Downstream, moq.pro's agent tests pass `captures=None`
  (`agent/tests/test_runtime.py`, `agent/tests/test_supervisor.py`). They
  change with the pin bump that brings this in.
