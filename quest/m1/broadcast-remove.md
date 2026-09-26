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

## Required

- [Kotlin end](/quest/m1/kotlin-end.md) - Kotlin keeps a forced end once `finish` is gone
- `main` merged into `dev` once #4031 lands, so the deprecations exist there
