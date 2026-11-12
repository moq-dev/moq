# [S] Binding server helpers observe session errors and describe close truthfully

## Goal

- Python's `serve()` retrieves every session task's exception, so asyncio
  never logs "Task exception was never retrieved". On exit it cancels and
  awaits the session tasks it still holds.
- The Kotlin, Go and Dart server docs say what happens: `serve()` returns
  once accepting stops and its sessions have closed, and closing the server
  closes every connection.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`): milestone ✅ m3 (rejected: m1), audit scores 5/10 and 3/10.
The audit's premises were narrowed after checking the source:

- Python (`py/moq-rs/moq/server.py:244-250`): the `finally` cancels
  unfinished session tasks, which contradicts its comment (`:246-247`). But
  the accept loop ends only after `cancel()` closes the listener, and that
  closes every QUIC connection (`rs/moq-ffi/src/server.rs:330-337`,
  `rs/moq-tokio/src/server.rs:897-905`), so no session could drain anyway.
  The real bug is the done callback (`:244`), which only discards: a
  non-clean disconnect or a failed handshake leaves an unretrieved exception.
  Retrieve it, cancel and gather the remaining tasks on exit as Go's
  `wg.Wait` does (`go/wrapper/server.go:266`), and fix the comment. Telling
  pending handshakes apart from established sessions isn't needed.
- Kotlin (`kt/moq/src/jvmAndAndroidMain/kotlin/dev/moq/Server.kt:57-83`):
  `serve()` can't wait forever, since `close()` tears connections down and
  each child's `closed()` then returns. The docs are wrong the other way:
  "In-flight sessions stay alive until their handles are dropped or
  cancelled" (`Server.kt:16-17`, `:79-80`) is false. The same claim is in
  Go's `Close` (`go/wrapper/server.go:298`) and Dart
  (`dart/moq/lib/src/server.dart:58-60`). Fix all three, and say `serve()`
  returns once its sessions have closed.

Public API: docs only, plus Python's `serve()` no longer logging stray task
exceptions. Wire: none.
