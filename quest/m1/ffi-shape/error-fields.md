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
