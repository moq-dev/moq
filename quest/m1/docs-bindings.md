# [M] Document the moq-ffi and moq-c APIs

## Goal

Every public item in `moq-ffi` has a one-line rustdoc, so every generated
binding carries it, including the [generated C package](/quest/m1/c/README.md)'s
`moq.h`, and `moq-ffi` sets `#![warn(missing_docs)]`.

## Plan

The m0 documentation audit (2026-10-19) found 116 undocumented public items
in `moq-ffi` (42 in `error.rs`, the rest across `media`, `audio`, `origin`,
`video`, and the producer and consumer types). Error codes and variants make
up most of it.

Count them with `cargo rustdoc -p moq-ffi --lib --all-features -- -W
missing_docs`. Check that the docs reach the outputs a binding user reads:
UniFFI copies rustdoc into the generated Python, Swift, Kotlin, and Go code,
and the C backend in the `uniffi-bindgen-cpp` fork into the generated
`moq.h`. The hand-written wrappers listed in the
Cross-Package Sync table keep their own docs; align them where they restate
the same item.

Decided 2026-10-20 (maintainer): the moq-c half lands on the
[generated C package](/quest/m1/c/README.md), not the frozen hand-written
ABI, so C users break once; the hand-written `moq-c` stays undocumented.

## Required

- [Generated C bindings](/quest/m1/c/README.md) - `moq.h` comes from the generated package; the hand-written crate gets no more work
