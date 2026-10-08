# [S] A dropped moq-uring session closes its connection

## Goal

A `moq-uring` `Session` dropped without `close()` closes its QUIC connection
when its last handle goes, instead of living until the idle timeout, which
never fires while the peer keeps the connection alive.

## Plan

Found by #4965 (WebTransport close backends), which made the HTTP/3 close task
keep the session alive structurally until its capsule is sent.

Decided 2026-10-07: first check whether moq-net always calls `close()` before
dropping a session (the `Connection` doc comment in
`rs/moq-uring/src/quic/noq/connection.rs` says its session machine does); if
it does, the leak only affects direct moq-uring users, which still need it
fixed. Close with a generic application code on drop of
the last handle, matching what the other backends do, with a test that drops
a session and observes the peer's close.

Public API: none expected. Wire: none.

## Related

- [moq-uring tests under load](/quest/m1/uring-tests-under-load.md) - the same crate's test isolation
