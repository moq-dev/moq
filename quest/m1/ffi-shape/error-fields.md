# [M] moq-ffi error variants name their fields

## Goal

Every `MoqError` variant that carries data names its fields
(`Transport { message }`), so no binding exposes a positional `v1`, as the
generated C++ does today.

## Plan

Decided while iterating on #4079 and recorded in the FFI shape README. It
moved into its own quest when the line landed (2026-10-09). Breaking in every
binding that matches on the payload, so it should merge before the next
binding release with [Codecs](/quest/m1/ffi-shape/codec.md). Update the
wrappers that read the payload, `cpp/obs`, and the `doc/lib` samples.
Wire: none.
