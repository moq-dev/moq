# [S] A session the client sees is one the drain waits for

## Goal

A listening `moq` CLI never exits without a GOAWAY to a session its client
already counts as established. A SIGTERM that arrives right after the
handshake drains that session like any other. `listening_import_drains_viewers_on_sigterm`
(`rs/moq-cli/tests/listen.rs`, added by #5510) passes reliably under load,
and a regression test pins the window.

## Plan

Seen once in CI. It passed 50 of 50 runs locally. The test waits for its
client's connection to be `established()`, then sends SIGTERM. The client
counts the session as established before the CLI has registered it for
shutdown. A SIGTERM in that gap finds nothing to wait for, so the drain
finishes at once and the CLI exits without a GOAWAY.

This is a product race, not just a test one: a real client connecting just
before a shutdown is cut off the same way. Fix it at the source. The session
must be counted from before the peer can see it established, continuously
through to the guard that holds the drain for the live session. Admission
already holds `Activity` through the handshake (`moq_relay::shutdown`), so
look for where that hold ends before the established session's own guard
starts. Check the relay's listeners (QUIC, io_uring, WebSocket) too, since
they share `moq_relay::supervise` and the same observer. Fix any that have the
gap in the same PR.

The regression must hit the window deterministically. Prefer an in-process
test on a paused clock that sends the shutdown at the exact point the peer
sees the session established, over widening the process test. No sleeps.

Public API: none expected. Wire: none.

## Related

- [Drain cleanup](/quest/m1/drain-cleanup.md) - the other end of the same drain: sessions force-closed at its deadline finish their cleanup
