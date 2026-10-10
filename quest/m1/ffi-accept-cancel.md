# [S] A cancelled moq-ffi accept never takes a session

## Goal

Cancelling a pending `MoqServer::accept` in any binding never takes an
incoming session: the next `accept` gets it. `MoqServer::cancel` still
releases the bound port, and every server call then resolves `Cancelled`.

## Plan

Found in #5140 (ffi-cancel-read), decided 2026-10-09. That PR made
`Task::run` await in place, so a cancelled future that is never polled again
makes no progress. `MoqServer::listen` and `accept` kept the spawned path
(`Task::spawn`) because `MoqServer::cancel` blocks its thread until the
in-flight call finishes, and an `accept` parked on that same thread would
never finish (`server_cancel_releases_the_bound_port` hangs). So an `accept`
that is cancelled but not yet freed (uniffi frees it later, at
`rust_future_free`) can still take a session that then goes nowhere.

- Decided: close the listener through a handle kept outside the `Task` lock,
  so `cancel` stops it without waiting on the in-flight call. Then `listen`
  and `accept` use `run` like every other call, and `Task::spawn` goes away.
  Rejected: accepting the race and documenting it.
- Regression in moq-ffi: poll an `accept` once and stop without freeing it,
  connect a client, free the cancelled accept, and require the next `accept`
  to return that session. Keep `server_cancel_releases_the_bound_port`
  passing.

Public API: none. Wire: none.
