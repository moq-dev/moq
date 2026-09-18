# [S] libmoq: server-side accept

## Goal

A C embedder accepts sessions itself through the same two-phase SETUP the
FFI exposes.

## Plan

Most of the catch-up #2152 lists has landed in dev's `rs/libmoq/src/api.rs`:
subscription options, track info, abort codes, client TLS roots, datagrams
(`moq_datagram` :462-468, `moq_publish_track_datagram` :1948, and
`moq_consume_datagrams` with its read, free, and close :2599-2650), and raw
frame timestamps (:2538-2541). One gap remains: moq-ffi's
`MoqServer::accept` yields a `MoqRequest` whose own `accept()` completes
SETUP (rs/moq-ffi/src/server.rs:54, :144, :186, :259); a C embedder cannot
accept sessions at all. Mirror it on the datagram task's handle and
terminal-status contract. Broadcast requests are not in this quest:
`requested_broadcast` reaches C through the origin dynamic handle, so do
not add a separate path here.

The addition regenerates `moq.h`, touches `cpp/obs/src` only if used, and
updates `doc/lib/c/index.md`.

Branch from main after the dev merge. The new entry point is additive. The
C output-configuration layout change is owned by its separate M1 quest.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the dev-only API must be released on main before this implementation starts

## Closes

- [#2152](https://github.com/moq-dev/moq/issues/2152) - close this issue when the quest finishes

## Related

- [libmoq fetch](/quest/m2/libmoq-fetch.md) - additive group fetch, on main
