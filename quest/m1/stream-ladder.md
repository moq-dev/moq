# [M] Byte-stream renditions form a ladder

## Goal

A simulcast ladder of moq-ffi byte-stream producers
(`MoqMediaTrackStreamProducer`, which imports fMP4, MKV, TS, or FLV bytes)
works like a ladder of frame producers (`MoqMediaTrackProducer`):

- A stream producer can be passed as another rendition's `lower`, and sits
  one priority below it.
- It disables itself when the bandwidth grant can't fund it, and resumes at
  the next keyframe, as
  [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029) does for
  frame rungs.

Every binding exposes this.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning; m1 so the binding API
breaks before the cut). Raised by CodeRabbit on #7029
([r4436515610](https://github.com/moq-dev/moq/pull/7029#discussion_r4436515610)).

- The stream producer records its priority and exports `set_enabled`.
- `lower` (`MoqVideoInit.lower`, `MoqVideoProducerOptions.lower`) accepts
  either producer kind. That changes a public UniFFI field type in every
  binding (Python, Swift, Kotlin, Go, Dart).
  - Pick the type that keeps one field, for example a small enum or a shared
    rendition handle.
  - Record the choice, and the migration in each `MIGRATE.md`.
- The C ABI follows when the C ABI parity work for #7029 does.
- Test a two-rung stream ladder in `rs/moq-ffi/src/test.rs`: the lower rung
  is funded first, and the upper rung disables and resumes at a keyframe.

Public API: breaking in every binding (`lower`'s type). Wire: none.
