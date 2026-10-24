# Generated C bindings

## Goal

C programs use moq through a C API generated from moq-ffi, shipped as the
`moq-c` package with CMake target `moq::c`, so C replaces the hand-written libmoq ABI and tracks every moq-ffi change the way Go, Swift, Kotlin, Python,
Dart, and C++ already do. The C API shape follows moq-ffi and breaks C users
once. The C++ package (`cpp/moq`) and the OBS plugin are out of scope; they
already sit on moq-ffi through the generated C++.

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
- Docs change inline: the consumer quest rewrites `doc/lib/c`, and its
  Impact bullet carries the move for the package's `MIGRATE.md`. No separate
  guide.

The line owns the pre-cut check: every `doc/lib/c` sample and the C interop
client build and run against the built release archive, not only in-tree.
[Published archive proof](/quest/m2/release/c-publish.md) repeats it after
publication; [old-crate retirement](/quest/m2/release/c-retire.md) follows
that publication and is not a prerequisite of this preparation.

The hand-written crate gets no more feature work: its shutdown, CMake library,
and fetch quests were abandoned for this line, and hidden is done.
Decided in the quest audit: [claim epochs](https://github.com/moq-dev/moq/pull/5678)
is a narrow correctness exception. Its existing claim API migrates with
Rust and JS so C cannot retain the unsafe accept path while this line waits.
That change owns its C tests and migration notes; other API reshaping stays here.

Decided 2026-10-20 (maintainer): m1 quests that change the C API
([exact binding timestamps](/quest/m1/binding-timestamps.md)
and [Rust identity enforcement](/quest/m1/catalog-identity-rs.md)) Require this
line and land their C half on the generated package, so C users break once.
Signed priorities already reached the hand-written ABI as `int8_t` in
[moq-dev/moq#6142](https://github.com/moq-dev/moq/pull/6142).
[Dynamic subscribe ranges](/quest/m1/subscribe-ranges/dynamic.md) is the
settled exception from its later plan in
[moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123): if generation
has not landed, the frozen C crate splits fills into its existing per-group
requests internally and keeps its ABI, so that quest does not wait here.

## Required

- [moq-c package](/quest/m1/c/package.md) - the generated header ships as `moq-c` 0.8.0 with `moq::c`, pkg-config, and a release workflow
- [C consumers](/quest/m1/c/consumers.md) - the C interop client and `doc/lib/c` samples move onto the generated API
- [Async failure contract](/quest/m1/c/async-failure-contract.md) - a generated async call that fails early fires no callback, and one that started always delivers its terminal callback
- [Durations in generated C](/quest/m1/c/duration.md) - the C backend renders UniFFI's `Duration` instead of refusing it
