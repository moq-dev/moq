# [S] Remove finish

## Goal

On `dev`, the deprecated broadcast end APIs are gone in every language, and
waiting for a broadcast to close says only that it closed.

## Plan

This is a published API break, so it targets `dev`.

- Remove `broadcast::Producer::finish`, `abort`, and `Consumer::is_finished`,
  and the `finished` and `abort` fields they read.
- `Consumer::closed()`, `Consumer::poll_closed`, and `Dynamic::closed()` return
  `()` rather than an `Error` cause.
- Remove the deprecated binding `finish` methods, `moq_publish_finish`, and
  JS's `close(abort)` parameter.
- Kotlin has no generated `close()`: it would collide with `AutoCloseable.close()`,
  so `rs/moq-ffi/uniffi.toml` excludes it. Kotlin's `close()` releases the handle,
  which ends the broadcast only once no `dynamic()` handle remains. Removing
  `finish` leaves Kotlin without a forced end; decide whether it needs one.

## Required

- [Broadcast close](/quest/m1/broadcast-close/README.md) - the deprecations this removes, which must reach `main` and then `dev` first
