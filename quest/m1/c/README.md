# Generated C bindings

## Goal

C programs use moq through a C API generated from moq-ffi, shipped as the
`moq-c` package with CMake target `moq::c`, so the hand-written libmoq ABI is
deleted and C tracks every moq-ffi change the way Go, Swift, Kotlin, Python,
Dart, and C++ already do. The C API shape follows moq-ffi and breaks C users
once. The C++ package and the OBS plugin are out of scope; they already sit on
moq-ffi through [C++ through moq-ffi](/quest/m1/cpp/README.md).

## Plan

Decided:

- The generator is a C backend in the `kixelated/uniffi-bindgen-cpp` fork,
  beside the C++ one, so both share its parsing, async, and error plumbing and
  one pinned tool covers both. No mature upstream C generator exists; uniffi
  itself only emits the low-level scaffolding header.
- Async calls take a completion callback and user data and return a task;
  freeing the task cancels the call. Callbacks run on moq's dispatcher thread
  by default, or on the host's own loop through `moq_set_dispatcher` and
  `moq_work_run`, the C form of `moq::set_executor`. Streams are repeated calls.
  This removes the trampolines and lock-order rules libmoq's single callback
  thread forced on hosts like OBS.
- Errors, records, and names mirror the C++ package: calls return `moq_error *`
  (NULL on success) with `moq_error_message()`, results come through
  out-params, records are owned structs with `moq_<type>_free`, and names are
  the uniffi names in snake case.
- The header is C11: errors and data enums are a `tag` plus an anonymous union
  (`err->protocol`). Infallible calls return their value directly, optional
  scalars are `{has_value, value}`, each async function has its own callback
  typedef, and unnamed variant fields are `v1`, as in C++.
- #4288 already renamed the hand-written crate to
  `moq-c` (`rs/moq-c`), and the generated package takes over that name
  and `moq::c` target, so C users migrate once. Its first release is 0.8.0, a
  minor bump over the hand-written 0.7.x.
- Docs change inline: the consumer quest rewrites `doc/lib/c`, and retirement
  adds an upgrade note. No separate guide.

The line owns the end-to-end check: every `doc/lib/c` sample and the C interop
client build and run against the released 0.8.0 archive, not only in-tree.

The hand-written crate gets no more feature work: its shutdown, CMake library,
and fetch quests were abandoned for this line, and hidden is done.

## Required

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the generator fork, package recipe, and OBS move this line builds on
- [moq-c package](/quest/m1/c/package.md) - the generated header ships as `moq-c` 0.8.0 with `moq::c`, pkg-config, and a release workflow
- [C consumers](/quest/m1/c/consumers.md) - the C interop client and `doc/lib/c` samples move onto the generated API
- [Retire the hand-written moq-c](/quest/m1/c/retire.md) - the hand-written crate is deleted after its final release points at the generated package

## Related

- [FFI shape](/quest/m1/ffi-shape/README.md) - reshapes moq-ffi, which the generated C then follows for free
