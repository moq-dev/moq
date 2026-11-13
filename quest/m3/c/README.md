# Generated C bindings

## Goal

C programs use moq through a C API generated from moq-ffi, shipped as the
`moq-c` package with CMake target `moq::c`, so C replaces the hand-written ABI
and tracks every moq-ffi change the way Go, Swift, Kotlin, Python, Dart, and
C++ already do. The C++ package (`cpp/moq`) and the OBS plugin are out of
scope; they already sit on moq-ffi through the generated C++.

## Plan

Decided 2026-10-25 (maintainer): this line ships after the cut, once moq builds
its bindings from [rust2's exporter](/quest/m3/ffi-exporter.md) (amended
2026-11-02, when moq chose the exporter over rust2's UniFFI fork). The hand-written crate ships at the cut
instead, with the binding features moq-ffi gained in m1 and m2
([moq-dev/moq#8060](https://github.com/moq-dev/moq/pull/8060)). Features moq-c
never had, such as single-group fetch and group-level consumption, wait for this
line. The C generator's GLib-style names, which fix
the C name clash, come from rust2
([moq-dev/rust2#66](https://github.com/moq-dev/rust2/pull/66)), not from a
`kixelated/uniffi-bindgen-cpp` release, and reach moq with the switch.

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
  the shared minor of the cut that ships it, after the hand-written crate's
  0.22 (RELEASE.md's [Versions](/RELEASE.md#versions),
  [moq-dev/moq#7610](https://github.com/moq-dev/moq/pull/7610)).
- Docs change inline: the consumer quest rewrites `doc/lib/c`, and its Impact
  bullet carries the move for the package's `MIGRATE.md`.

The `moq-c` package (`c/moq`, `just c check`) builds the generated header and
its probe in CI, so drift fails loud. moq-ffi's record
`MoqAudioEncoderFinished` was renamed from `MoqAudioEncoderFinish` so moq's
current generator emits the header;
[the switch](/quest/m3/ffi-exporter.md) reverts that rename. No `c-v*` tag ships
before the cut.

The line owns the pre-publication check: every `doc/lib/c` sample and the C
interop client build and run against the built release archive.
[Published archive proof](/quest/m3/release/c-publish.md) repeats it after
publication; [old-crate retirement](/quest/m3/release/c-retire.md) follows.

## Required

- [Switch to rust2's exporter](/quest/m3/ffi-exporter.md) - the C ABI comes from rust2's exporter instead of a UniFFI generator
