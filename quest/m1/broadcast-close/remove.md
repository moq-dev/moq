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
- Kotlin keeps a forced end, spelled `end()`: `rs/moq-ffi/uniffi.toml` renames the
  generated `close()` so it doesn't collide with `AutoCloseable.close()`, which
  releases the handle and ends the broadcast only once no `dynamic()` handle
  remains. Without it, a serving loop holding `dynamic()` could only be ended by
  cancelling that loop.
