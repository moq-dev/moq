# Generated C bindings

## Goal

C programs use moq through a C API generated from moq-ffi, shipped as the
`moq-c` package with CMake target `moq::c`, so C replaces the hand-written ABI
and tracks every moq-ffi change the way Go, Swift, Kotlin, Python, Dart, and
C++ already do. The C++ package (`cpp/moq`) and the OBS plugin are out of
scope; they already sit on moq-ffi through the generated C++.

## Plan

Decided 2026-10-25 (maintainer): this line ships after the cut, once moq builds
its bindings from rust2's UniFFI fork. The hand-written crate ships at the cut
with [parity](/quest/m1/c/README.md) instead. The generator fix for the C name
clash lands in rust2, not in a `kixelated/uniffi-bindgen-cpp` release.

Decided earlier, still standing:

- Async calls take a completion callback and user data and return a task;
  freeing the task cancels the call. Callbacks run on moq's dispatcher thread
  by default, or on the host's own loop through `moq_set_dispatcher` and
  `moq_work_run`, the C form of `moq::set_executor`. Streams are repeated calls.
- Errors and records mirror the C++ package: calls return `MoqError *`
  (NULL on success) with `moq_error_message()`, results come through
  out-params, and records are owned structs with `moq_<type>_free`. Names
  follow GLib: CamelCase types and snake_case functions.
- The header is C11: errors and data enums are a `tag` plus an anonymous union
  (`err->protocol`). Infallible calls return their value directly, optional
  scalars are `{has_value, value}`, each async function has its own callback
  typedef, and unnamed variant fields are `v1`, as in C++.
- The generated package takes over the `moq-c` name and `moq::c` target at
  0.8.0, a minor bump over the hand-written 0.7.x.
- Docs change inline: the consumer quest rewrites `doc/lib/c`, and its Impact
  bullet carries the move for the package's `MIGRATE.md`.

The `moq-c` package (`c/moq`, `just c check`) builds the generated header and
its probe in CI, so drift fails loud. Until name-clash lands, it renames the
record `MoqAudioEncoderFinish` to `MoqAudioEncoderFinished` so the current
generator emits the header; the generator fix can revert that rename. No
`c-v*` tag ships before the cut.

The line owns the pre-publication check: every `doc/lib/c` sample and the C
interop client build and run against the built release archive.
[Published archive proof](/quest/m2/release/c-publish.md) repeats it after
publication; [old-crate retirement](/quest/m2/release/c-retire.md) follows.

## Required

- [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md) - the C generator comes from rust2's fork
- [One C name per moq-ffi item](/quest/m2/c/name-clash.md) - rust2's C generator names types in CamelCase and functions in snake_case, so the record `MoqAudioEncoderFinish` and the method `moq_audio_encoder_finish` never share a name
- [C consumers](/quest/m2/c/consumers.md) - the C interop client and `doc/lib/c` samples move onto the generated API
- [Async failure contract](/quest/m2/c/async-failure-contract.md) - every generated async call returns a task and calls back exactly once, cancelled when its task was freed first
