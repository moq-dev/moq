# [L] Binding payloads cross the FFI without per-byte copies

## Goal

A binding moves a frame payload across the FFI with no per-byte loop: every
copy on the path is a bulk copy, and the report counts them per direction.
Proven on the nightly [binding benchmark](/bench/README.md) in every
wrapper (Python, Go, Kotlin, Swift, Dart, and C++).

## Plan

Decided with the maintainer on 2026-10-13:

- **Benchmark first.** `just bench-ffi` ([bench/README.md](/bench/README.md))
  measures every wrapper before and after. Add a Rust microbenchmark of
  UniFFI's lift and lower for `Vec<u8>` here, beside the patch it proves.
- **Bulk copy in the fork's core.** UniFFI lifts and lowers `Vec<u8>`
  through the generic `Vec<T>` path, one byte at a time with a bounds check
  each (`uniffi_core` `ffi_converter_impls.rs`), in both directions. That
  path covers arguments, record fields (`MoqFrame.payload`, video and audio
  frames), and returns (`read_frame`, `recv_datagram`). Make it one
  `memcpy` in [moq-dev/uniffi](/quest/m2/uniffi-fork.md). The serialized
  format is unchanged, so no generator or binding API changes. Two copies
  remain for records and returns (the wrapper's container to `RustBuffer`,
  then `RustBuffer` to `Vec<u8>`); removing the second needs the ownership
  change in [zero-copy returns](/quest/m2/ffi-bytes-return.md).
  #2946 already removed the per-byte copy on the Python side.
- **`&[u8]` arguments only if the numbers say so.** UniFFI 0.32 makes
  `&[u8]` arguments zero-copy, but only for sync calls that take bytes
  directly: flate `update`/`append` and the three `write(payload)` methods.
  It cannot be used in records, async calls, or callback interfaces. Adopting
  it means fixing the Go and C++/C generators (both lower to `RustBuffer`
  where the declaration expects `ForeignBytes`, so they would not compile)
  and Dart (copies and never frees). Kotlin's parameter type would become
  `ByteBuffer`. Do it only if the benchmark shows the remaining copy
  matters after the bulk copy; otherwise record the numbers and skip it.
- Zero-copy returns are [their own quest](/quest/m2/ffi-bytes-return.md),
  gated on these numbers.

Public API: none unless `&[u8]` lands, which changes Kotlin's parameter
type. Wire: none.

## Required

- [Swift, Dart, and C++ benchmarks](/quest/m2/ffi-bench-wrappers.md) - the other three wrappers' numbers
- [Hard fork](/quest/m2/uniffi-fork.md) - the repository the core patch lands in
