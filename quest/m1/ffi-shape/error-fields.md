# [M] moq-ffi error variants name their fields

## Goal

Every `MoqError` variant that carries data names its fields
(`Transport { message }`), so no binding exposes a positional `v1`, as the
generated C++ does today.

## Plan

Decided while iterating on #4079 and recorded in the FFI shape README. It
moved into its own quest when the line landed (2026-10-09). Breaking in every
binding that matches on the payload. Decided in the 2026-10-10 audit: it
must merge before the next binding release, together with
[Codecs](/quest/m1/ffi-shape/codec.md) and
[Bindings](/quest/m0/broadcast-epoch/bindings.md), so consumers take one
breaking release. Update the
wrappers that read the payload, `cpp/obs`, and the `doc/lib` samples.
Wire: none.

### Cross-language name

PR #5261 implements `reason` provisionally for string diagnostic payloads,
retaining `details` for structured protocol failures. The original `message`
proposal generates duplicate Kotlin properties with UniFFI 0.32.2: its exception
template also emits `override val message`. Both local compilation and CI
reproduce that failure. Swift construction also needs the new argument label.

Options: use `reason` consistently (recommended), use `diagnostic` consistently,
or keep `message` after an upstream generator fix is available. The PR remains
draft for this API choice; the binding release gate remains in place.
