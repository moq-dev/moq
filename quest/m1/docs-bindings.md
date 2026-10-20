# [M] Document the moq-ffi and moq-c APIs

## Goal

Every public item in `moq-ffi` and `moq-c` has a one-line rustdoc, so the
generated bindings and `moq.h` carry it, and both crates set
`#![warn(missing_docs)]`.

## Plan

The m0 documentation audit (2026-10-19) found 116 undocumented public items
in `moq-ffi` (42 in `error.rs`, the rest across `media`, `audio`, `origin`,
`video`, and the producer and consumer types) and 133 in `moq-c` (39 in
`error.rs`, 63 in `api.rs`, plus `audio` and `video`). Error codes and
variants make up most of it.

Count them with `cargo rustdoc -p <crate> --lib --all-features -- -W
missing_docs`. Check that the docs reach the outputs a binding user reads:
UniFFI copies rustdoc into the generated Python, Swift, Kotlin, and Go code,
and cbindgen copies it into `moq.h`. The hand-written wrappers listed in the
Cross-Package Sync table keep their own docs; align them where they restate
the same item.
