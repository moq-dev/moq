# [L] First-party consumers follow an epochless Restart

## Goal

Every first-party consumer that resolves a broadcast and keeps reading holds
the announce stream for its path, and on `Restart` drops its handles and
resolves the path again. On `Unroutable` it fails; it never retries a
request. A GOAWAY drain or a publisher replacement on an epochless route
then costs a long-lived consumer nothing but the restart.

## Plan

Moved to m1 on 2026-10-13 (maintainer): the WebSocket-to-QUIC upgrade is
removed, so an epochless Restart no longer follows
every connect; it remains for GOAWAY drains and publisher replacement.

The origin side is done: a handle resolved through a peer keeps serving new
requests while the peer's epochless replacement is held, and refuses them
with `Unroutable` only once the `Restart` is deliverable (`Release` in `rs/moq-net/src/model/origin.rs`,
`rs/moq-net/tests/integration/replacement.rs`). So a consumer that never follows `Restart`
works through the hold (300 ms by default), and its open subscriptions stay
sticky on the old session until it closes, where they end cleanly. A
consumer that opens a request after the hold, or reads past the old
session's close, needs this quest.

Audit and fix:

- Rust: `moq export` and `moq subscribe` (the interop subscriber), and any
  other moq-cli command that resolves a broadcast and keeps reading. The TS
  export's `Follower` (`rs/moq-mux/src/container/ts/follower.rs`) already
  watches the path, but treats any epochless return as a replacement:
  sticky until the old instance ends, then `Error::Replaced`, unless
  stitching. The fmp4, mkv, h264, h265, and flv exports don't follow at all.
  Decide whether an epochless `Restart` restarts the output (a new init
  segment mid-stream), stitches like TS, or ends the export.
  `moq play` already follows `Restart` (`rs/moq-cli/src/play/media.rs`).
- Bindings: moq-ffi's catalog and container consumers, as Go, C++, and
  Python reach them, and moq-c's (`moq_origin_announced_broadcast`,
  `moq_consume_catalog`). Prefer following once in moq-ffi and moq-c over
  duplicating it in every language wrapper. New API goes through the
  Cross-Package Sync table.
- JS: `js/net`'s origin owns restart following (`js/watch/src/broadcast.ts`
  mirrors its current handle). Confirm the js-native interop subscriber,
  which requests with `announced: true`, follows a Rust relay's `Restart`.
- The interop clients under `test/interop/clients`, where they resolve a
  broadcast themselves.

Regression: extend `rs/moq-net/tests/integration/replacement.rs` (or the consumer's own
tests) so each follower reads past the old session's close after an
epochless replacement. Then run `just test interop --all`.

If a failing client still leaves its connection to idle out, fix that
client's error path to close its session, as a separate commit. The C
client's `fail()` (`_exit` without `moq_session_close`) and the C++ client
on a consumer error both look suspect.

Public API: consumer-follow API in moq-ffi and moq-c, if the audit needs it.
Wire: none.

## Related

- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - the players that already follow `Restart`
